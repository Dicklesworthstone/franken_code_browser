import Foundation
import Dispatch

private final class RootGrant: Sendable {
    let id: Int
    let probe: RootProbe
    init(_ id: Int, probe: RootProbe) { self.id = id; self.probe = probe }
    deinit { probe.released(id) }
}
private final class InputGrant {
    let probe: RootProbe
    init(_ probe: RootProbe) { self.probe = probe }
    deinit { probe.inputReleased() }
}
private final class RootProbe: @unchecked Sendable {
    private let lock = NSLock()
    private var started: [Int] = []
    private var retired: [Int] = []
    private var active = 0, maximum = 0, inputReleases = 0
    private let blocked: Int?
    private let failed: Int?
    let resume = DispatchSemaphore(value: 0)
    init(blocked: Int? = nil, failed: Int? = nil) { self.blocked = blocked; self.failed = failed }
    var observations: ([Int], [Int], Int, Int) {
        lock.lock(); defer { lock.unlock() }; return (started, retired, maximum, inputReleases)
    }
    func released(_ id: Int) { lock.lock(); retired.append(id); lock.unlock() }
    func inputReleased() { lock.lock(); inputReleases += 1; lock.unlock() }
    func resolve(_ request: AtlasRootAccessRequest, _ cancellation: AtlasSearchCancellation) throws -> RootGrant {
        precondition(!Thread.isMainThread, "native resolution must leave the interaction thread")
        let id: Int
        switch request { case .restored(let data): id = Int(data[0]); case .selected: id = 77 }
        lock.lock(); started.append(id); active += 1; maximum = max(maximum, active); lock.unlock()
        defer { lock.lock(); active -= 1; lock.unlock() }
        if id == blocked { precondition(resume.wait(timeout: .now() + 5) == .success) }
        if id == failed { throw AtlasRootAccessError.unavailable }
        // A foreign resolver can finish successfully after cancellation. The
        // production queue and loader must drain and retire that result.
        return RootGrant(id, probe: self)
    }
}

/// Exercises the production admission model and actual bounded I/O worker.
/// Injected grant objects do not qualify OS bookmark restoration or a Mac app.
@MainActor @main struct AtlasRootAccessLoaderTests {
    static func until(_ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(5)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 1_000_000) }
        precondition(condition(), "root access worker did not drain within the test bound")
    }
    static func request(_ id: UInt8) -> AtlasRootAccessRequest { .restored(Data([id])) }
    static func main() async throws {
        try await successAndFailure()
        try await newestChoiceWins()
        try await canceledChoiceKeepsInputUntilDrain()
        try await independentWindows()
        try await latePanelCallbackCannotReplaceCurrentIntent()
        try await invalidRequestsDoNoNativeWork()
        print("Root access admission, background resolution, coalescing, cancellation and lifetime scenarios passed")
    }
    static func successAndFailure() async throws {
        let probe = RootProbe(failed: 1), loader = AtlasRootAccessLoader(resolve: { try probe.resolve($0, $1) })
        var failed = false
        loader.open(request(1)) { result in
            precondition(!loader.isLoading)
            if case .failure(.unavailable) = result { failed = true }
        }
        precondition(loader.isLoading)
        try await until { failed }
        var accepted: RootGrant?
        loader.open(.selected(URL(fileURLWithPath: "/selected/project"))) { result in
            guard case .success(let access) = result else { preconditionFailure("valid recovery failed") }
            accepted = access
        }
        try await until { accepted != nil }
        precondition(accepted?.id == 77 && probe.observations.0 == [1, 77])
        precondition(!probe.observations.1.contains(77), "accepted access must survive its caller")
        accepted = nil
        try await until { probe.observations.1 == [77] }
    }
    static func newestChoiceWins() async throws {
        let probe = RootProbe(blocked: 0), loader = AtlasRootAccessLoader(resolve: { try probe.resolve($0, $1) })
        var delivered: [Int] = []
        let receive: @MainActor (Result<RootGrant, AtlasRootAccessError>) -> Void = { result in
            guard case .success(let access) = result else { preconditionFailure("abandoned request was delivered") }
            delivered.append(access.id)
        }
        loader.open(request(0), completion: receive)
        try await until { probe.observations.0 == [0] }
        for id: UInt8 in 1...24 {
            loader.open(request(id), completion: receive)
            try await Task.sleep(nanoseconds: 1_000_000)
        }
        precondition(probe.observations.0 == [0] && loader.isLoading)
        probe.resume.signal()
        try await until { delivered == [24] }
        precondition(probe.observations.0 == [0, 24] && probe.observations.2 == 1)
        try await until { probe.observations.1.contains(0) }
        precondition(!loader.isLoading)
    }
    static func canceledChoiceKeepsInputUntilDrain() async throws {
        let probe = RootProbe(blocked: 0), loader = AtlasRootAccessLoader(resolve: { try probe.resolve($0, $1) })
        var input: InputGrant? = InputGrant(probe); weak var retainedInput = input
        var delivered = false
        loader.open(request(0), keepingAlive: [input!]) { _ in delivered = true }
        try await until { probe.observations.0 == [0] }
        input = nil; loader.cancel()
        precondition(!loader.isLoading && retainedInput != nil && probe.observations.3 == 0)
        probe.resume.signal()
        try await until { retainedInput == nil && probe.observations.1.contains(0) }
        precondition(!delivered && probe.observations.3 == 1)
        loader.open(request(2)) { result in
            if case .success(let access) = result { precondition(access.id == 2); delivered = true }
        }
        try await until { delivered }
        precondition(probe.observations.0 == [0, 2])
    }
    static func independentWindows() async throws {
        let probe = RootProbe(blocked: 0)
        let first = AtlasRootAccessLoader(resolve: { try probe.resolve($0, $1) }), second = AtlasRootAccessLoader(resolve: { try probe.resolve($0, $1) })
        var firstDelivered = false, secondDelivered = false
        first.open(request(0)) { _ in firstDelivered = true }
        try await until { probe.observations.0 == [0] }
        second.open(request(1)) { result in
            if case .success(let access) = result { precondition(access.id == 1); secondDelivered = true }
        }
        first.cancel()
        try await until { secondDelivered }
        probe.resume.signal()
        try await until { probe.observations.1.contains(0) }
        precondition(!firstDelivered && !second.isLoading)
    }
    static func invalidRequestsDoNoNativeWork() async throws {
        let probe = RootProbe(), loader = AtlasRootAccessLoader(resolve: { try probe.resolve($0, $1) })
        let invalid: [AtlasRootAccessRequest] = [
            .restored(Data()), .restored(Data(repeating: 0, count: AtlasRootAccessRequest.maximumBookmarkBytes + 1)),
            .selected(URL(string: "https://example.invalid/source")!),
            .selected(URL(string: "file://remote.invalid/source")!),
            .selected(URL(fileURLWithPath: "/nul\u{0}path")),
            .selected(URL(fileURLWithPath: "/" + String(repeating: "x", count: 16_384))),
        ]
        var refused = 0
        for request in invalid {
            loader.open(request) { result in
                if case .failure(.invalidRequest) = result { refused += 1 }
                else { preconditionFailure("invalid native request admitted") }
            }
        }
        precondition(refused == invalid.count && !loader.isLoading && probe.observations.0.isEmpty)
        precondition(AtlasRootAccessRequest.restored(Data(repeating: 1, count: AtlasRootAccessRequest.maximumBookmarkBytes)).isValid)
    }

    static func latePanelCallbackCannotReplaceCurrentIntent() async throws {
        let probe = RootProbe(blocked: 0), loader = AtlasRootAccessLoader(resolve: { try probe.resolve($0, $1) })
        let oldPanel = loader.cancel()
        var delivered: [Int] = []
        loader.open(request(0)) { result in
            if case .success(let access) = result { delivered.append(access.id) }
        }
        try await until { probe.observations.0 == [0] }
        precondition(!loader.accepts(oldPanel))
        loader.open(request(9), ifCurrent: oldPanel) { _ in preconditionFailure("obsolete panel changed the root") }
        precondition(loader.isLoading, "stale panel completion must not cancel the current resolver")
        probe.resume.signal()
        try await until { delivered == [0] }
        precondition(probe.observations.0 == [0])
        let closedPanel = loader.cancel(); loader.cancel()
        loader.open(request(8), ifCurrent: closedPanel) { _ in preconditionFailure("closed window accepted late panel") }
        precondition(!loader.isLoading && probe.observations.0 == [0])
    }
}
