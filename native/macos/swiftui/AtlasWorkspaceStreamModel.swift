import Foundation
import Observation
import Dispatch

struct AtlasWorkspaceStreamSourceIdentity: Equatable, Sendable {
    let root: String
    let project: UUID
    let browserOwner: UInt64
    let grant: ObjectIdentifier?
    let fileScope: AtlasSearchFileScope
    let pathScope: AtlasSearchPathScope
    static func == (a: Self, b: Self) -> Bool {
        a.project == b.project && a.browserOwner == b.browserOwner && a.grant == b.grant
            && a.root.utf8.elementsEqual(b.root.utf8)
            && a.fileScope == b.fileScope && a.pathScope == b.pathScope
    }
}

@MainActor struct AtlasWorkspaceStreamSource {
    let identity: AtlasWorkspaceStreamSourceIdentity
    let access: AnyObject?
    var root: String { identity.root }
    var fileScope: AtlasSearchFileScope { identity.fileScope }
    var pathScope: AtlasSearchPathScope { identity.pathScope }
    init(root: String, project: UUID, browserOwner: UInt64, access: AnyObject?,
         fileScope: AtlasSearchFileScope = .all, pathScope: AtlasSearchPathScope = .workspace) {
        identity = .init(root: root, project: project, browserOwner: browserOwner,
            grant: access.map(ObjectIdentifier.init), fileScope: fileScope, pathScope: pathScope)
        self.access = access
    }
    var scopeDescription: String {
        let location: String
        switch pathScope.kind {
        case .workspace: location = "Workspace"
        case .directory: location = "Folder: \(String(reflecting: pathScope.path))"
        case .file: location = "File: \(String(reflecting: pathScope.path))"
        case .visible: location = "Frozen atlas view (\(pathScope.visibleCount ?? 0) files)"
        }
        if fileScope.isAll { return location + " · all file types" }
        let extensions = (fileScope.extensions ?? "").split(separator: ",").map { "." + $0 }
        let names = (fileScope.filenames ?? "").split(separator: ",").map(String.init)
        let keys = extensions + names
        return location + " · " + (keys.isEmpty ? "no file types selected" : keys.joined(separator: ", "))
    }
}

/// The project owner can revoke this surface and its child context synchronously,
/// before a sheet animation or onDisappear releases the visible view.
@MainActor final class AtlasWorkspaceStreamPresentation: Identifiable {
    let id = UUID()
    let source: AtlasWorkspaceStreamSource
    let model: AtlasWorkspaceStreamModel
    private var started = false
    private var retired = false
    init(source: AtlasWorkspaceStreamSource, query: String, caseSensitive: Bool = true,
         countAllMatches: Bool = false, transport: AtlasWorkspaceStreamTransport) {
        self.source = source
        model = .init(transport: transport, query: query, caseSensitive: caseSensitive, countAllMatches: countAllMatches)
    }
    func start() {
        guard !started, !retired else { return }
        started = true; model.open(source)
    }
    func close() { retired = true; model.close() }
}

private final class AtlasWorkspaceStreamLease: @unchecked Sendable {
    let access: AnyObject?
    init(_ access: AnyObject?) { self.access = access }
}

/// Admission includes quarantined failed retirements. At most four local
/// owners and grants can survive; a failed close never releases live authority.
private final class AtlasWorkspaceStreamPermit: @unchecked Sendable {
    private final class State: @unchecked Sendable { let lock = NSLock(); var count = 0 }
    private static let state = State()
    private var acquired = false
    init?() {
        Self.state.lock.lock(); defer { Self.state.lock.unlock() }
        guard Self.state.count < 4 else { return nil }
        Self.state.count += 1
        acquired = true
    }
    deinit {
        guard acquired else { return }
        Self.state.lock.lock(); defer { Self.state.lock.unlock() }
        Self.state.count -= 1
    }
}

private final class AtlasWorkspaceStreamRetirement: @unchecked Sendable {
    static let shared = AtlasWorkspaceStreamRetirement()
    private let queue = DispatchQueue(label: "dev.frankencode.browser.workspace-stream-retirement", qos: .utility)
    private let lock = NSLock()
    private var retryScheduled = false
    private var quarantine: [Record] = [] // Queue only; bounded by admission permits.
    final class Record: @unchecked Sendable {
        let handle: UInt64
        let transport: AtlasWorkspaceStreamTransport
        let lease: AtlasWorkspaceStreamLease
        let permit: AtlasWorkspaceStreamPermit
        init(handle: UInt64, transport: AtlasWorkspaceStreamTransport,
             lease: AtlasWorkspaceStreamLease, permit: AtlasWorkspaceStreamPermit) {
            self.handle = handle; self.transport = transport; self.lease = lease; self.permit = permit
        }
        func close() -> Bool {
            withExtendedLifetime(lease) {
                for attempt in 0..<8 {
                    if transport.close(handle) { return true }
                    if attempt < 7 { Thread.sleep(forTimeInterval: 0.001) }
                }
                transport.retirementFailed(); return false
            }
        }
    }
    func retire(_ record: Record) {
        queue.async { [self, record] in
            drain()
            if !record.close() { quarantine.append(record) }
        }
    }
    func retry() {
        lock.lock()
        guard !retryScheduled else { lock.unlock(); return }
        retryScheduled = true; lock.unlock()
        queue.async { [self] in
            drain(); lock.lock(); retryScheduled = false; lock.unlock()
        }
    }
    private func drain() {
        let pending = quarantine; quarantine = []
        for record in pending where !record.close() { quarantine.append(record) }
    }
}

private final class AtlasWorkspaceStreamSession: @unchecked Sendable {
    let handle: UInt64
    let transport: AtlasWorkspaceStreamTransport
    private let lease: AtlasWorkspaceStreamLease
    private let permit: AtlasWorkspaceStreamPermit
    init(transport: AtlasWorkspaceStreamTransport, access: AnyObject?) throws {
        AtlasWorkspaceStreamRetirement.shared.retry()
        guard let permit = AtlasWorkspaceStreamPermit() else { throw AtlasWorkspaceStreamError.unavailable }
        let handle = transport.create()
        guard handle > 0 else { throw AtlasWorkspaceStreamError.unavailable }
        self.handle = handle; self.transport = transport
        self.permit = permit; lease = .init(access)
    }
    func cancel() { _ = transport.cancel(handle) }
    deinit {
        AtlasWorkspaceStreamRetirement.shared.retire(.init(handle: handle, transport: transport,
            lease: lease, permit: permit))
    }
}

/// Decode on the existing worker, retain only the latest typed preview, and
/// queue at most one main-actor delivery. A terminal seal closes this channel
/// before paging or publishing the accepted retained inventory.
private final class AtlasWorkspaceStreamProgressMailbox: @unchecked Sendable {
    private let lock = NSLock()
    private let catalog: AtlasWorkspaceStreamCatalog
    private let request: AtlasWorkspaceStreamRequest
    private let minimumGeneration: UInt64
    private let cancellation: AtlasSearchCancellation
    private let deliver: @MainActor (AtlasWorkspaceStreamProgress) -> Void
    private var last: AtlasWorkspaceStreamProgress?
    private var latest: AtlasWorkspaceStreamProgress?
    private var scheduled = false
    private var closed = false
    private var faulted = false

    init(catalog: AtlasWorkspaceStreamCatalog, request: AtlasWorkspaceStreamRequest,
        minimumGeneration: UInt64, cancellation: AtlasSearchCancellation,
        deliver: @escaping @MainActor (AtlasWorkspaceStreamProgress) -> Void) {
        self.catalog = catalog; self.request = request; self.minimumGeneration = minimumGeneration
        self.cancellation = cancellation; self.deliver = deliver
    }
    var hasFault: Bool { lock.lock(); defer { lock.unlock() }; return faulted }
    func offer(_ json: String) {
        lock.lock(); let accepts = !closed && !faulted; lock.unlock()
        guard accepts, !cancellation.isCanceled else { return }
        let progress: AtlasWorkspaceStreamProgress
        do { progress = try .decode(json, catalog: catalog, request: request) }
        catch { reject(); return }
        lock.lock()
        guard !closed, !faulted else { lock.unlock(); return }
        do {
            guard progress.summary.identity.generation > minimumGeneration else {
                throw AtlasWorkspaceStreamError.invalidResponse
            }
            if let last { try progress.validate(after: last) }
        } catch {
            faulted = true; latest = nil; lock.unlock(); cancellation.cancel(); return
        }
        last = progress; latest = progress
        let enqueue = !scheduled
        scheduled = true; lock.unlock()
        if enqueue { DispatchQueue.main.async { self.drain() } }
    }
    func seal(validating terminal: AtlasWorkspaceStreamPage) throws {
        lock.lock(); defer { lock.unlock() }
        closed = true; latest = nil
        guard !faulted, terminal.summary.identity.generation > minimumGeneration else {
            throw AtlasWorkspaceStreamError.invalidResponse
        }
        if let last { try last.validateTerminal(terminal) }
    }
    func close() {
        lock.lock(); closed = true; latest = nil; lock.unlock()
    }
    private func reject() {
        lock.lock()
        guard !closed else { lock.unlock(); return }
        faulted = true; latest = nil; lock.unlock()
        cancellation.cancel()
    }
    @MainActor private func drain() {
        lock.lock()
        let value = closed || faulted ? nil : latest
        latest = nil; scheduled = false; lock.unlock()
        if let value { deliver(value) }
    }
}

@Observable @MainActor final class AtlasWorkspaceStreamModel {
    private(set) var catalog: AtlasWorkspaceStreamCatalog?
    private(set) var report: AtlasWorkspaceStreamReport?
    private(set) var progress: AtlasWorkspaceStreamProgress?
    private(set) var searching = false
    private(set) var stopping = false
    private(set) var witness: AtlasWorkspaceStreamWitness?
    private(set) var currentContext: AtlasObservedReaderPresentation?
    private(set) var busy = false
    private(set) var opening = false
    private(set) var closed = false
    private(set) var reportAuthorized = false
    private(set) var notice = "Open the project catalog, then search the selected location and file types."
    var queryText: String {
        didSet { if !oldValue.utf8.elementsEqual(queryText.utf8) { revokeQuery() } }
    }
    var scanBytes: UInt64 = AtlasWorkspaceStreamLimits.scanBytes {
        didSet { if oldValue != scanBytes { revokeQuery() } }
    }
    var countAllMatches = false {
        didSet { if oldValue != countAllMatches { revokeQuery() } }
    }
    var caseSensitive = true {
        didSet { if oldValue != caseSensitive { revokeQuery() } }
    }
    @ObservationIgnored private let transport: AtlasWorkspaceStreamTransport
    @ObservationIgnored private let io = AtlasProjectIO()
    @ObservationIgnored private var authority: AtlasWorkspaceStreamSource?
    @ObservationIgnored private var session: AtlasWorkspaceStreamSession?
    @ObservationIgnored private var pendingSession: AtlasWorkspaceStreamSession?
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var flag: AtlasSearchCancellation?
    @ObservationIgnored private var operation = UUID()
    @ObservationIgnored private var lastGeneration: UInt64 = 0
    @ObservationIgnored private var stopFlag: AtlasWorkspaceStreamStop?
    @ObservationIgnored private var progressMailbox: AtlasWorkspaceStreamProgressMailbox?

    init(transport: AtlasWorkspaceStreamTransport, query: String = "",
         caseSensitive: Bool = true, countAllMatches: Bool = false) {
        self.transport = transport; queryText = query
        self.caseSensitive = caseSensitive; self.countAllMatches = countAllMatches
    }
    deinit { task?.cancel() }
    static func retryRetirements() { AtlasWorkspaceStreamRetirement.shared.retry() }

    var canSearch: Bool {
        !closed && !opening && session != nil && catalog != nil && lastGeneration < UInt64.max
            && (try? currentRequest()) != nil
    }
    private func currentRequest() throws -> AtlasWorkspaceStreamRequest {
        guard let authority else { throw AtlasWorkspaceStreamError.unavailable }
        return try .init(needle: queryText, maximumScanBytes: scanBytes,
            fileScope: authority.fileScope, pathScope: authority.pathScope,
            countAllMatches: countAllMatches, caseSensitive: caseSensitive)
    }
    var canRefresh: Bool { !closed && authority != nil }
    var canStopAndKeep: Bool { !closed && busy && searching && !stopping && stopFlag != nil }
    var canSelect: Bool { !closed && !busy && reportAuthorized && session != nil && report != nil }
    var canReadCurrentContext: Bool {
        guard canSelect, let report, let witness else { return false }
        return report.admits(witness.target)
    }
    var coverage: String? {
        if let progress { return progress.coverage }
        guard let report else { return nil }
        return (reportAuthorized ? "" : "Previous results are read-only. Search again before selecting or opening a result. ")
            + report.summary.coverage
    }

    func open(_ source: AtlasWorkspaceStreamSource) {
        close(); authority = source; closed = false
        refreshCatalog()
    }

    /// Metadata discovery is explicitly refreshed. A failed replacement keeps
    /// the prior accepted catalog; it never turns prior hits into new targets.
    func refreshCatalog() {
        guard canRefresh, let authority else { return }
        cancelWork(); revokeTargets(clearEvidence: true)
        guard authority.identity.browserOwner > 0, authority.root.utf8.first == 47,
              authority.root.utf8.count <= AtlasObservedReaderLimits.maximumPathBytes,
              !authority.root.utf8.contains(0) else { notice = AtlasWorkspaceStreamError.invalidInput.message; return }
        let candidate: AtlasWorkspaceStreamSession
        do { candidate = try .init(transport: transport, access: authority.access) }
        catch { notice = message(error); return }
        let (ticket, flag) = begin("Discovering a fresh catalog of at most 20,000 project files…")
        opening = true; pendingSession = candidate
        let root = authority.root
        task = Task {
            do {
                let accepted = try await io.perform(cancellation: flag, keepingAlive: [candidate]) {
                    try AtlasWorkspaceStreamCatalog.decode(candidate.transport.open(candidate.handle, root, flag),
                        owner: candidate.handle)
                }
                guard accepts(ticket, flag), pendingSession === candidate else { return }
                session = candidate; pendingSession = nil; catalog = accepted; lastGeneration = 0
                finish(ticket)
                notice = "Catalogued \(accepted.files) files. Press Search to read file contents within the selected total byte budget."
                if !accepted.discoveryComplete { notice += " Discovery is incomplete; results cannot cover the whole project." }
            } catch { fail(error, ticket: ticket) }
        }
    }

    func search() {
        guard canSearch, let session, let catalog else { return }
        let request: AtlasWorkspaceStreamRequest
        do { request = try currentRequest() }
        catch { notice = message(error); return }
        // A foreign call can replace the query before its reply is lost or
        // rejected. Preserve old bytes for reading, but revoke their actions.
        revokeTargets(clearEvidence: false)
        let (ticket, flag) = begin("Searching the selected location and file types, including large files…")
        searching = true
        let progressive = session.transport.progressiveSearch
        let stop = progressive == nil ? nil : AtlasWorkspaceStreamStop()
        stopFlag = stop
        let mailbox: AtlasWorkspaceStreamProgressMailbox?
        if progressive != nil {
            mailbox = .init(catalog: catalog, request: request, minimumGeneration: lastGeneration,
                cancellation: flag) { [weak self, weak session] progress in
                guard let self, let session, self.busy, self.searching,
                      self.accepts(ticket, flag), self.session === session, self.catalog == catalog,
                      request == (try? self.currentRequest()) else { return }
                self.progress = progress
                if !self.stopping { self.notice = progress.countLabel }
            }
        } else { mailbox = nil }
        progressMailbox = mailbox
        task = Task {
            do {
                let accepted = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                    defer { mailbox?.close() }
                    let response: String?
                    if let progressive, let stop, let mailbox {
                        response = try progressive(session.handle, request, flag, stop) { mailbox.offer($0) }
                    } else { response = try session.transport.search(session.handle, request, flag) }
                    var page = try AtlasWorkspaceStreamPage.decode(response, catalog: catalog,
                        root: catalog.root, request: request, requireTerminal: progressive != nil)
                    try mailbox?.seal(validating: page)
                    let summary = page.summary
                    var hits = page.hits
                    while let next = page.next {
                        guard !flag.isCanceled else { throw AtlasWorkspaceStreamError.canceled }
                        page = try AtlasWorkspaceStreamPage.decode(session.transport.page(session.handle,
                            summary.identity.generation, next, AtlasWorkspaceStreamLimits.page),
                            catalog: catalog, root: catalog.root, request: request, previous: summary,
                            offset: next, requireTerminal: progressive != nil)
                        hits.append(contentsOf: page.hits)
                    }
                    return try AtlasWorkspaceStreamReport(summary: summary, hits: hits)
                }
                guard accepts(ticket, flag), self.session === session, self.catalog == catalog,
                      request == (try? currentRequest()) else { return }
                guard accepted.summary.identity.generation > lastGeneration else {
                    throw AtlasWorkspaceStreamError.invalidResponse
                }
                lastGeneration = accepted.summary.identity.generation
                report = accepted; witness = nil; reportAuthorized = true
                finish(ticket)
                notice = accepted.summary.stop == "user-stopped"
                    ? "Stopped and kept \(accepted.summary.retainedHits) matches from finalized file observations."
                    : accepted.summary.countLabel
            } catch {
                // Project I/O correctly gives cancellation precedence. Keep an
                // invalid callback's cause visible after using that flag to
                // abort the foreign scan; no provisional rows gain authority.
                if mailbox?.hasFault == true { fail(AtlasWorkspaceStreamError.invalidResponse, ticket: ticket) }
                else { fail(error, ticket: ticket) }
            }
        }
    }

    /// Stop preserves the current operation/generation. It neither cancels the
    /// task nor revokes the ticket, and waits for the verified retained result.
    func stopAndKeep() {
        guard canStopAndKeep, let stopFlag else { return }
        stopping = true; stopFlag.requestStop()
        notice = "Stopping and keeping matches from finalized files… The unfinished file's matches will be omitted."
    }

    func select(_ target: AtlasWorkspaceStreamTarget) {
        guard canSelect, let session, let report, let catalog, report.admits(target) else { return }
        closeContext(); witness = nil
        let (ticket, flag) = begin("Reading the retained literal without reopening its source file…")
        task = Task {
            do {
                let accepted = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                    try AtlasWorkspaceStreamWitness.decode(session.transport.hit(session.handle,
                        target.identity.generation, target.hit.index), report: report, root: catalog.root, target: target)
                }
                guard accepts(ticket, flag), self.session === session, reportAuthorized,
                      self.report?.admits(target) == true else { return }
                witness = accepted; finish(ticket)
                notice = "Retained literal selected. Its original bytes were copied with no new file read."
            } catch { fail(error, ticket: ticket) }
        }
    }

    /// This is an explicit new open under the current native root grant. Only
    /// matching literal bytes can earn a highlight in the new observed page.
    func readCurrentContext(transport: AtlasObservedReaderTransport) {
        guard canReadCurrentContext, let witness, let authority, let catalog else { return }
        do {
            let input = try AtlasObservedReaderInput(root: catalog.root, relativePath: witness.target.hit.path)
            let match = try AtlasObservedReaderExternalMatch(input: input, range: witness.target.hit.range,
                encoding: witness.target.hit.encoding, text: witness.text, originalHex: witness.originalHex)
            closeContext()
            let source = AtlasObservedReaderSource(input: input, project: authority.identity.project,
                access: authority.access, currentMatch: match)
            currentContext = .init(source: source, transport: transport)
        } catch { notice = "The exact current pathname or match range could not be represented. No context was opened." }
    }
    func closeContext() { currentContext?.close(); currentContext = nil }

    func clear() {
        revokeTargets(clearEvidence: true)
        guard !closed, let session, let catalog else { return }
        let (ticket, flag) = begin("Releasing retained search literals…")
        task = Task {
            do {
                let generation = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                    let wire = try AtlasWorkspaceStreamWire.read(session.transport.clear(session.handle), owner: session.handle)
                    try wire.header(schema: "fcb.atlas-stream-search/1", command: "clear", owner: session.handle)
                    let identity = try wire.streamIdentity(owner: session.handle, root: catalog.root, caseSensitive: true)
                    guard identity.manifest == catalog.manifest, identity.layout == catalog.layout,
                          try wire.number("retained_hits") == 0, try wire.number("retained_witness_bytes") == 0 else {
                        throw AtlasWorkspaceStreamError.invalidResponse
                    }
                    return identity.generation
                }
                guard accepts(ticket, flag), self.session === session else { return }
                guard generation > lastGeneration else { throw AtlasWorkspaceStreamError.invalidResponse }
                lastGeneration = generation; finish(ticket); notice = "Retained search literals cleared. The file catalog is unchanged."
            } catch { fail(error, ticket: ticket) }
        }
    }

    func cancel() {
        cancelWork()
        notice = "Operation canceled. Accepted evidence remains readable; entered file work may still be draining."
    }
    func close() {
        cancelWork(); revokeTargets(clearEvidence: true)
        closed = true; authority = nil; session = nil; pendingSession = nil; catalog = nil; lastGeneration = 0
    }
    private func revokeQuery() {
        if !opening { cancelWork() }
        revokeTargets(clearEvidence: true)
    }
    private func revokeTargets(clearEvidence: Bool) {
        closeContext(); reportAuthorized = false
        if clearEvidence { report = nil; witness = nil }
    }
    private func begin(_ notice: String) -> (UUID, AtlasSearchCancellation) {
        cancelWork()
        let ticket = UUID(), flag = AtlasSearchCancellation()
        operation = ticket; self.flag = flag; busy = true; self.notice = notice
        return (ticket, flag)
    }
    private func cancelWork() {
        progressMailbox?.close(); progressMailbox = nil; progress = nil
        stopFlag = nil; searching = false; stopping = false
        operation = UUID(); flag?.cancel(); task?.cancel(); io.cancel()
        session?.cancel(); pendingSession?.cancel()
        task = nil; flag = nil; pendingSession = nil; busy = false; opening = false
    }
    private func accepts(_ ticket: UUID, _ flag: AtlasSearchCancellation) -> Bool {
        !closed && operation == ticket && !flag.isCanceled && !Task.isCancelled
    }
    private func finish(_ ticket: UUID) {
        guard operation == ticket else { return }
        progressMailbox?.close(); progressMailbox = nil; progress = nil
        stopFlag = nil; searching = false; stopping = false
        task = nil; flag = nil; pendingSession = nil; busy = false; opening = false
    }
    private func fail(_ error: Error, ticket: UUID) {
        guard operation == ticket else { return }
        finish(ticket); notice = message(error)
    }
    private func message(_ error: Error) -> String {
        if error is CancellationError { return AtlasWorkspaceStreamError.canceled.message }
        if let error = error as? AtlasWorkspaceStreamError { return error.message }
        if let error = error as? AtlasProjectIOError, case .canceled = error { return AtlasWorkspaceStreamError.canceled.message }
        return AtlasWorkspaceStreamError.invalidResponse.message
    }
}
