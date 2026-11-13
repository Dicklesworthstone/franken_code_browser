import Foundation
import Observation

enum AtlasNativeOpenOrigin: Sendable { case panel, external }

enum AtlasNativeOpenError: Error, Sendable {
    case invalidSelection, unsupported, unavailable
    var message: String {
        switch self {
        case .invalidSelection:
            return "Open one local file or folder at a time, using a lossless UTF-8 path without control characters."
        case .unsupported:
            return "Choose a regular file or folder. Symbolic links and special files cannot be opened here."
        case .unavailable:
            return "Read access is unavailable. Reconnect the volume if needed, then choose the item again."
        }
    }
}

/// Only native URL syntax is inspected here; metadata and bookmarks stay on
/// the bounded worker. Reject a lossy URL-to-String conversion before it can
/// select a different filesystem object.
enum AtlasNativeOpenRequest {
    static func selectedURL(_ urls: [URL]) -> URL? {
        guard urls.count == 1, let url = urls.first,
              AtlasRootAccessRequest.selected(url).isValid,
              url.query == nil, url.fragment == nil, url.user == nil,
              url.password == nil, url.port == nil,
              !url.path.unicodeScalars.contains(where: { $0.value < 32 || $0.value == 127 }) else { return nil }
        let components = url.path.split(separator: "/", omittingEmptySubsequences: true)
        guard !components.contains(where: { $0 == "." || $0 == ".." }) else { return nil }
        let expected = Array(url.path.utf8)
        let lossless = url.withUnsafeFileSystemRepresentation { pointer -> Bool in
            guard let pointer else { return false }
            for index in expected.indices {
                guard pointer[index] != 0, UInt8(bitPattern: pointer[index]) == expected[index] else { return false }
            }
            return pointer[expected.count] == 0
        }
        return lossless ? url : nil
    }


}

/// A lifetime token only. The retained native grant is never inspected or
/// mutated by the worker; its final owner balances access after entered I/O.
final class AtlasNativeOpenLease: @unchecked Sendable {
    private let value: AnyObject
    init(_ value: AnyObject) { self.value = value }
}

enum AtlasNativeOpenTarget: Sendable {
    case file(URL, AtlasNativeOpenLease?)
    case observedFile(URL, AtlasNativeOpenLease?)
#if FCB_APP_STORE
    case folder(AppStoreRootAccess)
#else
    case folder(URL)
#endif

    var url: URL {
        switch self {
        case .file(let url, _), .observedFile(let url, _): return url
#if FCB_APP_STORE
        case .folder(let access): return access.url
#else
        case .folder(let url): return url
#endif
        }
    }

    /// Metadata chooses the reader strategy before payload is read. The Rust
    /// open still validates the current regular object; size is not identity.
    static func selectedFile(_ url: URL, lease: AtlasNativeOpenLease?,
                             observedBytes: UInt64, retainedLimit: UInt64) -> Self {
        observedBytes > retainedLimit ? .observedFile(url, lease) : .file(url, lease)
    }
}

/// One active classification/capture and one newest waiting user intent. A
/// canceled native call must drain before another reader is created; repeated
/// drops cannot allocate one worker/capture per event. The accepted reader is
/// replaced only after the next reader's first page has validated.
@Observable @MainActor final class AtlasNativeOpen<Reader: AnyObject> {
    private(set) var reader: Reader?
    private(set) var fileURL: URL?
    private(set) var presentationID: UUID?
    private(set) var isLoading = false
    private(set) var notice = ""
    var showsReader = false

    @ObservationIgnored var cancelExternal: @MainActor () -> Void = {}
    @ObservationIgnored private let io = AtlasProjectIO()
    @ObservationIgnored private let prepare: @MainActor (URL, AtlasNativeOpenOrigin) throws -> AtlasNativeOpenLease?
    @ObservationIgnored private let resolve: @Sendable (URL, AtlasSearchCancellation, AtlasNativeOpenLease?) throws -> AtlasNativeOpenTarget
    @ObservationIgnored private let capture: @MainActor (AtlasNativeOpenTarget) async throws -> Reader
    @ObservationIgnored private let retire: @MainActor (Reader) -> Void
    @ObservationIgnored private let failureMessage: @MainActor (Error) -> String
    @ObservationIgnored private var active = false
    @ObservationIgnored private var generation = UUID()
    @ObservationIgnored private var runningID: UUID?
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var cancellation: AtlasSearchCancellation?
    @ObservationIgnored private var pending: Submission?

    private struct Submission {
        let id: UUID
        let url: URL
        let lease: AtlasNativeOpenLease?
        let openFolder: @MainActor (AtlasNativeOpenTarget) -> Void
    }

    init(prepare: @escaping @MainActor (URL, AtlasNativeOpenOrigin) throws -> AtlasNativeOpenLease?,
         resolve: @escaping @Sendable (URL, AtlasSearchCancellation, AtlasNativeOpenLease?) throws -> AtlasNativeOpenTarget,
         capture: @escaping @MainActor (AtlasNativeOpenTarget) async throws -> Reader,
         retire: @escaping @MainActor (Reader) -> Void,
         failureMessage: @escaping @MainActor (Error) -> String) {
        self.prepare = prepare; self.resolve = resolve; self.capture = capture
        self.retire = retire; self.failureMessage = failureMessage
    }

    var canReceive: Bool { active }
    func activate() { active = true }
    func accepts(_ ticket: UUID) -> Bool { active && ticket == generation }

    /// Reserve intent before presenting a panel or loading a drop provider.
    /// Its late completion cannot replace a newer choice or a closed window.
    @discardableResult func reserve() -> UUID {
        generation = UUID()
        cancelExternal()
        pending = nil
        cancellation?.cancel()
        task?.cancel()
        io.cancel()
        isLoading = false
        notice = ""
        return generation
    }

    func open(_ urls: [URL], origin: AtlasNativeOpenOrigin, ifCurrent ticket: UUID? = nil,
              openFolder: @escaping @MainActor (AtlasNativeOpenTarget) -> Void) {
        guard active, ticket == nil || accepts(ticket!) else { return }
        guard let url = AtlasNativeOpenRequest.selectedURL(urls) else {
            reject(AtlasNativeOpenError.invalidSelection.message); return
        }
        let intent = reserve()
        do {
            let submission = Submission(id: intent, url: url, lease: try prepare(url, origin), openFolder: openFolder)
            isLoading = true
            notice = "Opening \(String(reflecting: url.lastPathComponent.isEmpty ? url.path : url.lastPathComponent))…"
            if runningID == nil { start(submission) }
            else { pending = submission }
        } catch {
            notice = failureMessage(error)
        }
    }

    func reject(_ message: String) { if active { notice = message } }
    func clearNotice() { notice = "" }

    func cancel() {
        reserve()
        notice = reader == nil ? "Opening canceled." : "Opening canceled. The current file is unchanged."
    }

    func closeReader() {
        reserve()
        if let reader { retire(reader) }
        reader = nil; fileURL = nil; presentationID = nil; showsReader = false
    }

    /// A sheet's delayed disappearance retires only the file it displayed.
    /// It must neither cancel a newer opening task nor close its replacement.
    func dismissReader(_ presentation: UUID) {
        guard presentationID == presentation else { return }
        if let reader { retire(reader) }
        reader = nil; fileURL = nil; presentationID = nil; showsReader = false
    }

    func waitForDrop(_ ticket: UUID) {
        guard accepts(ticket) else { return }
        isLoading = true
        notice = "Reading the dropped file URL…"
    }

    func rejectDrop(_ ticket: UUID) {
        guard accepts(ticket) else { return }
        isLoading = false
        notice = AtlasNativeOpenError.invalidSelection.message
    }

    func close() {
        active = false
        closeReader()
    }

    private func start(_ submission: Submission) {
        guard accepts(submission.id), runningID == nil else { return }
        let flag = AtlasSearchCancellation(), resolve = resolve
        let url = submission.url, lease = submission.lease
        runningID = submission.id
        cancellation = flag
        task = Task { [self] in
            do {
                let target = try await io.perform(cancellation: flag,
                    keepingAlive: lease.map { [$0] } ?? []) {
                    try resolve(url, flag, lease)
                }
                try Task.checkCancellation()
                guard accepts(submission.id) else { throw CancellationError() }
                switch target {
                case .folder:
                    // All native root work has completed. The existing project
                    // activation path owns persistence and discovery.
                    submission.openFolder(target)
                    notice = ""
                case .file, .observedFile:
                    let candidate = try await capture(target)
                    guard accepts(submission.id), !Task.isCancelled else {
                        retire(candidate)
                        throw CancellationError()
                    }
                    let previous = reader
                    reader = candidate; fileURL = target.url; presentationID = UUID(); showsReader = true
                    notice = ""
                    if let previous { retire(previous) }
                }
            } catch {
                if accepts(submission.id), !Task.isCancelled {
                    notice = failureMessage(error)
                    if reader != nil { notice += " The current file is unchanged." }
                }
            }
            // Even a canceled, closed or superseded request owns its slot until
            // the entered work and its grant have returned.
            guard runningID == submission.id else { return }
            runningID = nil; cancellation = nil; task = nil
            if let next = pending {
                pending = nil
                start(next)
            } else if accepts(submission.id) {
                isLoading = false
            }
        }
    }
}


/// Provider decoding is also admitted before invoking native code. Canceling
/// Progress does not release an entered provider or its slot until callback;
/// one latest waiting drop coalesces behind it.
@MainActor final class AtlasNativeDrop<Provider: AnyObject> {
    typealias Completion = @MainActor (Result<URL, AtlasNativeOpenError>) -> Void
    typealias Load = @MainActor (Provider, @escaping @Sendable (Result<URL, AtlasNativeOpenError>) -> Void) -> Progress
    private final class Entry {
        let id = UUID()
        let provider: Provider
        let completion: Completion
        var progress: Progress?
        var canceled = false
        init(_ provider: Provider, completion: @escaping Completion) {
            self.provider = provider; self.completion = completion
        }
    }
    private let load: Load
    private var active: Entry?
    private var pending: Entry?
    init(load: @escaping Load) { self.load = load }

    func submit(_ provider: Provider, completion: @escaping Completion) {
        cancel()
        let entry = Entry(provider, completion: completion)
        if active == nil { start(entry) }
        else { pending = entry }
    }
    func cancel() {
        pending = nil
        active?.canceled = true
        active?.progress?.cancel()
    }
    private func start(_ entry: Entry) {
        precondition(active == nil)
        active = entry
        let id = entry.id
        entry.progress = load(entry.provider) { [self] result in
            Task { @MainActor in finish(id, result: result) }
        }
    }
    private func finish(_ id: UUID, result: Result<URL, AtlasNativeOpenError>) {
        guard let entry = active, entry.id == id else { return }
        active = nil
        if !entry.canceled { entry.completion(result) }
        if active == nil, let next = pending { pending = nil; start(next) }
    }
}

/// App-level delivery chooses one registered window. There is one latest cold
/// launch/reopen event, never a broadcast to every WindowGroup instance.
@MainActor final class AtlasNativeOpenRouting {
    enum Event { case choose, open(URL), invalid }
    static let shared = AtlasNativeOpenRouting()
    private struct Endpoint {
        let id: UUID
        let available: @MainActor () -> Bool
        let preferred: @MainActor () -> Bool
        let receive: @MainActor (Event) -> Void
    }
    private var endpoints: [Endpoint] = []
    private var pending: Event?
    private var requestingWindow = false
    var requestWindow: (@MainActor () -> Void)?

    func register(_ id: UUID, available: @escaping @MainActor () -> Bool,
                  preferred: @escaping @MainActor () -> Bool,
                  receive: @escaping @MainActor (Event) -> Void) {
        endpoints.removeAll { $0.id == id }
        endpoints.append(Endpoint(id: id, available: available, preferred: preferred, receive: receive))
        flush(createWindow: false)
    }
    func unregister(_ id: UUID) { endpoints.removeAll { $0.id == id } }
    func open(_ urls: [URL]) {
        send(AtlasNativeOpenRequest.selectedURL(urls).map(Event.open) ?? .invalid)
    }
    func choose() { send(.choose) }
    private func send(_ event: Event) {
        pending = event
        flush()
    }
    func flush(createWindow: Bool = true) {
        guard let event = pending else { return }
        let available = endpoints.filter { $0.available() }
        if let endpoint = available.last(where: { $0.preferred() }) ?? available.last {
            pending = nil; requestingWindow = false
            endpoint.receive(event)
        } else if createWindow, !requestingWindow, let requestWindow {
            requestingWindow = true
            requestWindow()
        }
    }
}

#if os(macOS)
private final class AtlasNativeSelectedScope: Sendable {
    let url: URL
    let started: Bool
    init(_ url: URL) { self.url = url; started = url.startAccessingSecurityScopedResource() }
    deinit { if started { url.stopAccessingSecurityScopedResource() } }
}

#if FCB_APP_STORE
/// In-memory, read-only access to the exact selected file. It is never stored as
/// a recent project, widened to a parent bookmark, or offered to local links.
private final class AtlasNativeFileScope: Sendable {
    let url: URL
    init(_ selected: URL, cancellation: AtlasSearchCancellation) throws {
        let bookmark = try selected.bookmarkData(
            options: [.withSecurityScope, .securityScopeAllowOnlyReadAccess],
            includingResourceValuesForKeys: nil, relativeTo: nil)
        guard !cancellation.isCanceled, !bookmark.isEmpty,
              bookmark.count <= AtlasRootAccessRequest.maximumBookmarkBytes else {
            throw AtlasNativeOpenError.unavailable
        }
        var stale = false
        let resolved = try URL(resolvingBookmarkData: bookmark,
            options: [.withSecurityScope, .withoutUI, .withoutMounting], relativeTo: nil,
            bookmarkDataIsStale: &stale)
        guard !stale, AtlasNativeOpenRequest.selectedURL([resolved]) != nil,
              resolved.path.utf8.elementsEqual(selected.path.utf8),
              resolved.startAccessingSecurityScopedResource() else {
            throw AtlasNativeOpenError.unavailable
        }
        url = resolved
    }
    deinit { url.stopAccessingSecurityScopedResource() }
}
#endif

enum AtlasNativeOpenAccess {
    @MainActor static func selection(_ url: URL, origin: AtlasNativeOpenOrigin) throws -> AtlasNativeOpenLease? {
#if FCB_APP_STORE
        if origin == .panel {
            guard let (_, selection) = AppStoreRootAccess.selectionRequest(url) else {
                throw AtlasNativeOpenError.invalidSelection
            }
            return AtlasNativeOpenLease(selection)
        }
#endif
        let scope = AtlasNativeSelectedScope(url)
#if FCB_APP_STORE
        guard scope.started else { throw AtlasNativeOpenError.unavailable }
#endif
        return AtlasNativeOpenLease(scope)
    }

    /// Native metadata is admission only. The shared Rust source reader still
    /// verifies the opened regular object, byte limits and capture identity.
    static func resolve(_ url: URL, cancellation: AtlasSearchCancellation,
                        lease: AtlasNativeOpenLease?, retainedLimit: UInt64) throws -> AtlasNativeOpenTarget {
        guard AtlasNativeOpenRequest.selectedURL([url]) != nil else { throw AtlasNativeOpenError.invalidSelection }
        guard !cancellation.isCanceled else { throw CancellationError() }
        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        guard let kind = attributes[.type] as? FileAttributeType else {
            throw AtlasNativeOpenError.unsupported
        }
        guard !cancellation.isCanceled else { throw CancellationError() }
        switch kind {
        case .typeDirectory:
#if FCB_APP_STORE
            return .folder(try AppStoreRootAccess.resolve(.selected(url), cancellation: cancellation))
#else
            return .folder(url)
#endif
        case .typeRegular:
            guard let size = attributes[.size] as? NSNumber,
                  let bytes = UInt64(size.stringValue) else { throw AtlasNativeOpenError.unavailable }
#if FCB_APP_STORE
            let access = try AtlasNativeFileScope(url, cancellation: cancellation)
            guard !cancellation.isCanceled else { throw CancellationError() }
            return .selectedFile(access.url, lease: AtlasNativeOpenLease(access),
                observedBytes: bytes, retainedLimit: retainedLimit)
#else
            return .selectedFile(url, lease: lease, observedBytes: bytes, retainedLimit: retainedLimit)
#endif
        default:
            throw AtlasNativeOpenError.unsupported
        }
    }
}
#endif
