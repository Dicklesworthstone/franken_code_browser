import Foundation
import Dispatch

// Real serial-worker/main-queue lifecycle tests with gated work. These qualify
// the scheduler contract only; the separate bridge tests exercise source scans.
private final class Trace: @unchecked Sendable {
    private let lock = NSLock()
    private var names: [String] = []
    private var flags: [AtlasSearchCancellation] = []
    private var active = 0
    private var peak = 0
    private var usedMain = false
    func enter(_ query: String, _ flag: AtlasSearchCancellation) {
        lock.lock(); defer { lock.unlock() }
        names.append(query); flags.append(flag); active += 1
        peak = max(peak, active); usedMain = usedMain || Thread.isMainThread
    }
    func leave() { lock.lock(); active -= 1; lock.unlock() }
    var snapshot: ([String], [AtlasSearchCancellation], Int, Bool) {
        lock.lock(); defer { lock.unlock() }
        return (names, flags, peak, usedMain)
    }
}
private final class Lease {}

@MainActor @main struct AtlasSearchCoordinatorTests {
    nonisolated static func report(_ name: String, complete: Bool = true) -> AtlasSearchReport {
        let hit = SearchHit(id: 0, path: name, sourcePath: name, start: 0, end: 1,
                            captureSHA256: nil, captureByteLength: nil)
        return AtlasSearchReport(hits: [hit], complete: complete, truncated: !complete,
                                 unavailableFiles: 0, unsupportedFiles: 0, matchesSeen: 1)
    }
    static func pump(until condition: () -> Bool) {
        precondition(Thread.isMainThread)
        let deadline = Date().addingTimeInterval(5)
        while !condition() && Date() < deadline {
            _ = RunLoop.main.run(mode: .default, before: Date().addingTimeInterval(0.005))
        }
        precondition(condition(), "bounded main-queue completion timeout")
    }
    nonisolated static func wait(_ semaphore: DispatchSemaphore) {
        precondition(semaphore.wait(timeout: .now() + 5) == .success, "bounded worker gate timeout")
    }
    static func success(_ value: Result<AtlasSearchReport, AtlasSearchError>) -> AtlasSearchReport {
        switch value { case .success(let report): return report
        case .failure(let error): preconditionFailure("unexpected failure: \(error)") }
    }
    static func main() throws {
        precondition(Thread.isMainThread)
        try workRunsOffMainAndCompletesOnMain()
        try onlyNewestPendingRequestRuns()
        try obsoleteFailureCannotReplaceNewResults()
        try cancelRetainsAccessUntilForeignWorkReturns()
        try ownerDestructionDrainsWithoutDelivery()
        try completionCanSubmitAnotherRequest()
        try invalidRequestsDoNotCancelCurrentWork()
        try cancellationDropsPendingRequests()
        try partialAndEmptyReportsRemainDistinct()
        try workerCancellationFinishesLatestRequest()
        print("AtlasSearchCoordinator: 10 worker, cancellation, publication and grant-lifetime scenarios passed")
    }
    static func workRunsOffMainAndCompletesOnMain() throws {
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        let trace = Trace()
        let coordinator = AtlasSearchCoordinator { _, query, flag in
            trace.enter(query, flag); defer { trace.leave() }
            entered.signal(); wait(release); return report(query)
        }
        var delivered = false
        try coordinator.submit(root: "/project", query: "first") { result in
            precondition(Thread.isMainThread)
            precondition(success(result).hits[0].path == "first"); delivered = true
        }
        wait(entered)
        precondition(!delivered, "submit must not wait for source work")
        precondition(!trace.snapshot.3, "search ran on the main thread")
        release.signal(); pump { delivered }
    }
    static func onlyNewestPendingRequestRuns() throws {
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        let trace = Trace()
        let coordinator = AtlasSearchCoordinator { _, query, flag in
            trace.enter(query, flag); defer { trace.leave() }
            if query == "first" { entered.signal(); wait(release) }
            return report(query)
        }
        var delivered: [String] = []
        try coordinator.submit(root: "/old", query: "first") { delivered.append(success($0).hits[0].path) }
        wait(entered)
        for index in 1...128 {
            try coordinator.submit(root: "/new", query: "replacement-\(index)") {
                delivered.append(success($0).hits[0].path)
            }
        }
        precondition(trace.snapshot.1[0].isCanceled)
        release.signal(); pump { !delivered.isEmpty }
        precondition(delivered == ["replacement-128"], "obsolete completion published")
        precondition(trace.snapshot.0 == ["first", "replacement-128"], "pending work was not coalesced")
        precondition(trace.snapshot.2 == 1, "two bridge calls overlapped")
    }
    static func obsoleteFailureCannotReplaceNewResults() throws {
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        let coordinator = AtlasSearchCoordinator { _, query, _ in
            if query == "fails" { entered.signal(); wait(release); throw AtlasSearchError.unavailable }
            return report(query)
        }
        var oldDelivered = false, newDelivered = false
        try coordinator.submit(root: "/old", query: "fails") { _ in oldDelivered = true }
        wait(entered)
        try coordinator.submit(root: "/new", query: "valid") {
            precondition(success($0).hits[0].path == "valid"); newDelivered = true
        }
        release.signal(); pump { newDelivered }
        precondition(!oldDelivered)
    }
    static func cancelRetainsAccessUntilForeignWorkReturns() throws {
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        let coordinator = AtlasSearchCoordinator { _, query, flag in
            entered.signal(); wait(release)
            precondition(flag.isCanceled); return report(query)
        }
        var lease: Lease? = Lease(); weak var retained = lease
        var delivered = false
        try coordinator.submit(root: "/scoped", query: "old", accessLease: lease) { _ in delivered = true }
        lease = nil; wait(entered); coordinator.cancel()
        precondition(retained != nil, "access ended during a foreign call")
        release.signal(); pump { retained == nil }
        precondition(!delivered, "cancel published a stale result")
    }
    static func ownerDestructionDrainsWithoutDelivery() throws {
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        var coordinator: AtlasSearchCoordinator? = AtlasSearchCoordinator { _, query, flag in
            entered.signal(); wait(release)
            precondition(flag.isCanceled); return report(query)
        }
        weak var owner = coordinator
        var lease: Lease? = Lease(); weak var retained = lease
        var delivered = false
        try coordinator!.submit(root: "/scoped", query: "close", accessLease: lease) { _ in delivered = true }
        lease = nil; wait(entered); coordinator = nil
        precondition(owner == nil, "worker must not retain the UI owner")
        precondition(retained != nil)
        release.signal(); pump { retained == nil }
        precondition(!delivered)
    }
    static func completionCanSubmitAnotherRequest() throws {
        let trace = Trace()
        let coordinator = AtlasSearchCoordinator { _, query, flag in
            trace.enter(query, flag); defer { trace.leave() }; return report(query)
        }
        var delivered: [String] = []
        try coordinator.submit(root: "/project", query: "one") { value in
            delivered.append(success(value).hits[0].path)
            do {
                try coordinator.submit(root: "/project", query: "two") { delivered.append(success($0).hits[0].path) }
            } catch { preconditionFailure("reentrant submit failed") }
        }
        pump { delivered.count == 2 }
        precondition(delivered == ["one", "two"] && trace.snapshot.2 == 1)
    }
    static func invalidRequestsDoNotCancelCurrentWork() throws {
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        let trace = Trace()
        let coordinator = AtlasSearchCoordinator { _, query, flag in
            trace.enter(query, flag); defer { trace.leave() }
            if query == "valid" { entered.signal(); wait(release) }; return report(query)
        }
        var delivered: [String] = []
        let first = try coordinator.submit(root: "/project", query: "valid") {
            delivered.append(success($0).hits[0].path)
        }
        precondition(first == 1)
        wait(entered)
        for (root, query) in [("", "x"), ("/project", ""), ("/project\0ignored", "x"),
                              ("/project", "x\0ignored"), ("/project", String(repeating: "x", count: 1025)),
                              (String(repeating: "x", count: 16385), "x")] {
            do {
                try coordinator.submit(root: root, query: query) { _ in preconditionFailure("invalid work delivered") }
                preconditionFailure("invalid request admitted")
            } catch AtlasSearchError.invalidRequest {} catch { preconditionFailure("wrong admission error") }
        }
        precondition(!trace.snapshot.1[0].isCanceled, "invalid input canceled valid work")
        release.signal(); pump { delivered.count == 1 }
        let unicode = String(repeating: "é", count: 512)
        let next = try coordinator.submit(root: "/project", query: unicode) { delivered.append(success($0).hits[0].path) }
        precondition(next == 2, "invalid admission consumed an identity")
        pump { delivered.count == 2 }; precondition(delivered.last == unicode)
    }
    static func cancellationDropsPendingRequests() throws {
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        let trace = Trace()
        let coordinator = AtlasSearchCoordinator { _, query, flag in
            trace.enter(query, flag); defer { trace.leave() }
            if query == "first" { entered.signal(); wait(release) }; return report(query)
        }
        var obsolete = false, done = false
        try coordinator.submit(root: "/old", query: "first") { _ in obsolete = true }
        wait(entered)
        var pendingLease: Lease? = Lease(); weak var retained = pendingLease
        try coordinator.submit(root: "/discarded", query: "pending", accessLease: pendingLease) { _ in obsolete = true }
        pendingLease = nil; coordinator.cancel()
        precondition(retained == nil, "a never-started grant was retained after cancel")
        try coordinator.submit(root: "/current", query: "current") { _ in done = true }
        release.signal(); pump { done }
        precondition(!obsolete && trace.snapshot.0 == ["first", "current"] && trace.snapshot.2 == 1)
    }
    static func partialAndEmptyReportsRemainDistinct() throws {
        let coordinator = AtlasSearchCoordinator { _, query, _ in
            if query == "error" { throw AtlasSearchError.unavailable }
            return AtlasSearchReport(hits: [], complete: query == "empty", truncated: query == "partial",
                                     unavailableFiles: 0, unsupportedFiles: 0, matchesSeen: 0)
        }
        var states: [Bool] = [], failed = false
        try coordinator.submit(root: "/project", query: "partial") { states.append(success($0).complete) }
        pump { states.count == 1 }
        try coordinator.submit(root: "/project", query: "empty") { states.append(success($0).complete) }
        pump { states.count == 2 }
        try coordinator.submit(root: "/project", query: "error") {
            if case .failure(.unavailable) = $0 { failed = true } else { preconditionFailure("failure became empty results") }
        }
        pump { failed }; precondition(states == [false, true])
    }
    static func workerCancellationFinishesLatestRequest() throws {
        let coordinator = AtlasSearchCoordinator { _, query, flag in
            flag.cancel(); return report(query)
        }
        var done = false
        try coordinator.submit(root: "/project", query: "canceled") { result in
            if case .failure(.canceled) = result { done = true }
            else { preconditionFailure("worker cancellation published a source result") }
        }
        pump { done }
    }

}
