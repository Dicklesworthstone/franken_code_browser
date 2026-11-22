import Foundation
import Observation
import Dispatch

@MainActor final class AtlasWorkspaceStructurePresentation: Identifiable {
    let id = UUID()
    let source: AtlasWorkspaceStreamSource
    let model: AtlasWorkspaceStructureModel
    private var started = false
    private var retired = false

    init(source: AtlasWorkspaceStreamSource, query: String, mode: AtlasWorkspaceStructureMode,
        nameMode: AtlasSearchNameMode, discoveryPolicy: AtlasDiscoveryPolicy,
        transport: AtlasWorkspaceStructureTransport) {
        self.source = source
        model = .init(transport: transport, query: query, mode: mode, nameMode: nameMode,
            discoveryPolicy: discoveryPolicy, registryDomain: AtlasReaderTransport.native.comparison?.domain,
            makeReader: { AtlasPagedReaderModel(transport: .native, search: .native, outline: .native, document: .native) })
    }
    func start() {
        guard !started, !retired else { return }
        started = true; model.open(source)
    }
    func close() { retired = true; model.close() }
}

/// A copied result reader is independently owned and has no live-path refresh
/// capability. Its selected structural range remains tied to the original hit.
@MainActor final class AtlasWorkspaceStructureReader: Identifiable {
    let id = UUID()
    let hit: AtlasWorkspaceStructureHit
    let reader: AtlasPagedReaderModel
    init(hit: AtlasWorkspaceStructureHit, reader: AtlasPagedReaderModel) {
        self.hit = hit; self.reader = reader
    }
}

struct AtlasWorkspaceStructureReaderReceipt: Sendable {
    let info: AtlasReaderInfo
    let initialOffset: UInt64

    static func decode(_ json: String?, target: AtlasWorkspaceStructureTarget,
        summary: AtlasWorkspaceStructureSummary, root: [UInt8], destination: UInt64) throws -> Self {
        do {
            guard destination > 0, destination != summary.owner,
                  target.owner == summary.owner, target.generation == summary.generation,
                  target.revision == summary.revision else { throw AtlasWorkspaceStructureError.invalidResponse }
            let wire = try AtlasWorkspaceStructureWire.read(json, owner: summary.owner, command: "open-reader")
            let repeatedSummary = try AtlasWorkspaceStructureSummary(wire, owner: summary.owner,
                generation: summary.generation, root: root, request: summary.request)
            let hit = try AtlasWorkspaceStructureHit(wire.object("hit"), summary: repeatedSummary)
            guard repeatedSummary == summary, hit == target.hit,
                  try wire.number("reader_owner") == destination,
                  try wire.text("activation_source") == "retained-search-capture",
                  try wire.number("additional_source_bytes_read") == 0,
                  try wire.number("additional_read_calls") == 0,
                  let json, let object = try JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any],
                  let nestedInfo = object["reader"] as? [String: Any],
                  let nestedWindow = object["window"] as? [String: Any] else {
                throw AtlasWorkspaceStructureError.invalidResponse
            }
            let infoJSON = String(decoding: try JSONSerialization.data(withJSONObject: nestedInfo, options: [.sortedKeys]), as: UTF8.self)
            let info = try AtlasReaderInfo.decodeRetainedInitialization(infoJSON, handle: destination, pathBytes: hit.path)
            // A new reader has its own file/revision namespace. The old hit is
            // bound above; path, digest, extent and encoding bind its copied bytes.
            guard info.captureSHA256 == hit.digest, info.identity.capturedBytes == hit.sourceBytes,
                  info.identity.encoding == hit.encoding else { throw AtlasWorkspaceStructureError.invalidResponse }
            let windowJSON = String(decoding: try JSONSerialization.data(withJSONObject: nestedWindow, options: [.sortedKeys]), as: UTF8.self)
            let start = hit.range.start >= 256 ? hit.range.start - 256 : 0
            _ = try AtlasReaderPage.decode(windowJSON, info: info, request: .window(offset: start, bytes: 16 * 1024))
            // The bounded initial window need not contain a whole long heading.
            // The independent reader retains the complete source and starts at
            // the authoritative original offset, with no invented query token.
            return Self(info: info, initialOffset: hit.range.start)
        } catch let error as AtlasWorkspaceStructureError { throw error }
        catch { throw AtlasWorkspaceStructureError.invalidResponse }
    }
}

private final class AtlasWorkspaceStructurePermit: @unchecked Sendable {
    private final class State: @unchecked Sendable { let lock = NSLock(); var count = 0 }
    private static let state = State()
    init?() {
        Self.state.lock.lock(); defer { Self.state.lock.unlock() }
        guard Self.state.count < 4 else { return nil }
        Self.state.count += 1
    }
    deinit {
        Self.state.lock.lock(); defer { Self.state.lock.unlock() }
        Self.state.count -= 1
    }
}

private final class AtlasWorkspaceStructureLease: @unchecked Sendable {
    let access: AnyObject?
    init(_ access: AnyObject?) { self.access = access }
}

/// Failed retirement keeps both the original grant and the four-owner capacity
/// permit. Large source destruction runs on this utility queue, never in a view.
private final class AtlasWorkspaceStructureRetirement: @unchecked Sendable {
    static let shared = AtlasWorkspaceStructureRetirement()
    private let queue = DispatchQueue(label: "dev.frankencode.browser.structure-retirement", qos: .utility)
    private var quarantine: [Record] = []
    private let lock = NSLock()
    private var retryScheduled = false
    final class Record: @unchecked Sendable {
        let handle: UInt64
        let transport: AtlasWorkspaceStructureTransport
        let lease: AtlasWorkspaceStructureLease
        let permit: AtlasWorkspaceStructurePermit
        init(handle: UInt64, transport: AtlasWorkspaceStructureTransport,
            lease: AtlasWorkspaceStructureLease, permit: AtlasWorkspaceStructurePermit) {
            self.handle = handle; self.transport = transport; self.lease = lease; self.permit = permit
        }
        func close() -> Bool {
            withExtendedLifetime(lease) {
                for attempt in 0..<8 {
                    if transport.close(handle) { return true }
                    if attempt < 7 { Thread.sleep(forTimeInterval: 0.001) }
                }
                transport.retirementFailed()
                return false
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
        let old = quarantine; quarantine = []
        for record in old where !record.close() { quarantine.append(record) }
    }
}

private final class AtlasWorkspaceStructureSession: @unchecked Sendable {
    let handle: UInt64
    let transport: AtlasWorkspaceStructureTransport
    private let permit: AtlasWorkspaceStructurePermit
    private let lease: AtlasWorkspaceStructureLease
    init(transport: AtlasWorkspaceStructureTransport, access: AnyObject?) throws {
        AtlasWorkspaceStructureRetirement.shared.retry()
        guard let permit = AtlasWorkspaceStructurePermit() else { throw AtlasWorkspaceStructureError.unavailable }
        let handle = transport.create()
        guard handle > 0 else { throw AtlasWorkspaceStructureError.unavailable }
        self.handle = handle; self.transport = transport; self.permit = permit; lease = .init(access)
    }
    func cancel() { _ = transport.cancel(handle) }
    deinit {
        AtlasWorkspaceStructureRetirement.shared.retire(.init(handle: handle, transport: transport, lease: lease, permit: permit))
    }
}

/// Main-actor scheduling and identity ownership. Every foreign operation runs on
/// the existing bounded worker; a pause takes effect between admitted batches.
/// Only one result page and one independently opened reader are displayed.
@Observable @MainActor final class AtlasWorkspaceStructureModel {
    var queryText: String {
        didSet { if !oldValue.utf8.elementsEqual(queryText.utf8) { edited() } }
    }
    var mode: AtlasWorkspaceStructureMode {
        didSet { if oldValue != mode { edited() } }
    }
    var nameMode: AtlasSearchNameMode {
        didSet { if oldValue != nameMode { edited() } }
    }
    var scanBytes: UInt64 = AtlasWorkspaceStructureLimits.defaultScanBytes {
        didSet { if oldValue != scanBytes { edited() } }
    }
    let discoveryPolicy: AtlasDiscoveryPolicy
    private(set) var page: AtlasWorkspaceStructurePage?
    private(set) var currentReader: AtlasWorkspaceStructureReader?
    private(set) var busy = false
    private(set) var running = false
    private(set) var pausing = false
    private(set) var stopping = false
    private(set) var closed = true
    private(set) var authorized = false
    private(set) var notice = "Enter a symbol name or Markdown heading to search the selected project scope."
    @ObservationIgnored private let transport: AtlasWorkspaceStructureTransport
    @ObservationIgnored private let registryDomain: UUID?
    @ObservationIgnored private let makeReader: @MainActor () -> AtlasPagedReaderModel
    @ObservationIgnored private let io = AtlasProjectIO()
    @ObservationIgnored private var authority: AtlasWorkspaceStreamSource?
    @ObservationIgnored private var session: AtlasWorkspaceStructureSession?
    @ObservationIgnored private var pendingReader: AtlasPagedReaderModel?
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var cancellation: AtlasSearchCancellation?
    @ObservationIgnored private var operation = UUID()
    @ObservationIgnored private var generation: UInt64 = 0
    @ObservationIgnored private var previousWindows: [UInt64] = []

    init(transport: AtlasWorkspaceStructureTransport, query: String = "", mode: AtlasWorkspaceStructureMode = .symbols,
        nameMode: AtlasSearchNameMode = .contains, discoveryPolicy: AtlasDiscoveryPolicy = .repositoryRules,
        registryDomain: UUID?, makeReader: @escaping @MainActor () -> AtlasPagedReaderModel) {
        self.transport = transport; queryText = query; self.mode = mode; self.nameMode = nameMode
        self.discoveryPolicy = discoveryPolicy; self.registryDomain = registryDomain; self.makeReader = makeReader
    }
    deinit { task?.cancel() }

    var canSearch: Bool { !closed && !busy && generation < UInt64.max && (try? request(startAfter: 0)) != nil }
    var canPause: Bool { !closed && busy && running && !pausing && !stopping }
    var canContinue: Bool { canSelect && page?.summary.more == true }
    var canStopAndKeep: Bool {
        !closed && authorized && page?.summary.more == true && session != nil && (!busy || running) && !stopping
    }
    var canSelect: Bool { !closed && !busy && authorized && session != nil && page != nil }
    var canPreviousPage: Bool { canSelect && (page?.offset ?? 0) > 0 }
    var canNextPage: Bool { canSelect && page?.nextOffset != nil }
    var canLaterWindow: Bool {
        canSelect && page?.summary.more == false && page?.summary.hasMoreMatches == true
            && (page?.summary.retainedHits ?? 0) > 0 && generation < UInt64.max
    }
    var canEarlierWindow: Bool { canSelect && !previousWindows.isEmpty && generation < UInt64.max }

    private func request(startAfter: UInt64) throws -> AtlasWorkspaceStructureRequest {
        guard let authority else { throw AtlasWorkspaceStructureError.unavailable }
        return try .init(needle: queryText, mode: mode, nameMode: nameMode,
            fileScope: authority.fileScope, pathScope: authority.pathScope, discoveryPolicy: discoveryPolicy,
            maximumScanBytes: scanBytes, startAfter: startAfter)
    }

    func open(_ source: AtlasWorkspaceStreamSource) {
        close()
        guard source.identity.browserOwner > 0, source.root.utf8.first == 47,
              source.root.utf8.count <= 16_384, !source.root.utf8.contains(0) else {
            notice = AtlasWorkspaceStructureError.invalidInput.message
            return
        }
        authority = source; closed = false
        notice = "The selected location, file types and discovery policy stay fixed in this sheet."
        if canSearch { search() }
    }

    func search() {
        guard canSearch else { return }
        start(startAfter: 0, windows: [])
    }

    private func start(startAfter: UInt64, windows: [UInt64]) {
        guard !closed, !busy, let authority, generation < UInt64.max else { return }
        let root = authority.root
        do {
            let request = try self.request(startAfter: startAfter)
            cancelWork(retireSession: true)
            page = nil; authorized = false
            generation += 1
            let generation = generation
            let next = try AtlasWorkspaceStructureSession(transport: transport, access: authority.access)
            session = next; previousWindows = Array(windows.suffix(32))
            let (ticket, flag) = begin("Discovering project files for \(request.mode.label.lowercased())…")
            running = true
            task = Task {
                do {
                    let opened = try await io.perform(cancellation: flag, keepingAlive: [next]) {
                        let page = try AtlasWorkspaceStructurePage.decode(
                            next.transport.open(next.handle, root, generation, request), command: "info",
                            owner: next.handle, generation: generation, root: Array(root.utf8), request: request,
                            acceptingCanonicalRoot: true)
                        guard page.summary.revision == 1, page.summary.retainedHits == 0,
                              page.summary.counts["payload_bytes_read"] == 0, page.summary.counts["read_calls"] == 0 else {
                            throw AtlasWorkspaceStructureError.invalidResponse
                        }
                        return page
                    }
                    guard accepts(ticket, flag, session: next) else { return }
                    page = opened; authorized = true
                    try await scan(next, starting: opened, root: opened.summary.root, ticket: ticket, flag: flag)
                } catch { fail(error, ticket: ticket) }
            }
        } catch { notice = message(error) }
    }

    private func scan(_ session: AtlasWorkspaceStructureSession, starting: AtlasWorkspaceStructurePage,
        root: [UInt8], ticket: UUID, flag: AtlasSearchCancellation) async throws {
        var accepted = starting
        while accepted.summary.more {
            guard accepts(ticket, flag, session: session) else { throw AtlasWorkspaceStructureError.canceled }
            if pausing && !stopping {
                finish(ticket)
                notice = "Paused between completed file batches. Open retained results or Continue this observation."
                return
            }
            let previous = accepted, stop = stopping
            let next = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                let json = try stop
                    ? session.transport.stop(session.handle, previous.summary.generation)
                    : session.transport.advance(session.handle, previous.summary.generation, previous.summary.revision, 4)
                let page = try AtlasWorkspaceStructurePage.decode(json, command: stop ? "stop" : "advance",
                    owner: session.handle, generation: previous.summary.generation, root: root, request: previous.summary.request)
                guard page.summary.follows(previous.summary), !stop || !page.summary.more else {
                    throw AtlasWorkspaceStructureError.invalidResponse
                }
                return page
            }
            guard accepts(ticket, flag, session: session) else { return }
            accepted = next; page = next; authorized = true
            notice = next.summary.coverage
        }
        finish(ticket)
        notice = accepted.summary.complete
            ? "Finished the admitted extraction. Retained results open exact captured source; language and extraction limits remain explicit."
            : "Search ended with partial extraction coverage. Retained results remain available."
    }

    func pause() {
        guard canPause else { return }
        pausing = true; notice = "Pausing after the current bounded file batch…"
    }
    func resume() {
        guard canContinue, let session, let page else { return }
        let (ticket, flag) = begin("Continuing the same project observation…")
        running = true
        let root = page.summary.root
        task = Task {
            do { try await scan(session, starting: page, root: root, ticket: ticket, flag: flag) }
            catch { fail(error, ticket: ticket) }
        }
    }
    func stopAndKeep() {
        guard canStopAndKeep else { return }
        if busy {
            stopping = true; pausing = false; notice = "Stopping after the current bounded batch and keeping accepted results…"
        } else {
            guard let session, let page else { return }
            let (ticket, flag) = begin("Stopping discovery and keeping accepted results…")
            running = true; stopping = true
            let root = page.summary.root
            task = Task {
                do { try await scan(session, starting: page, root: root, ticket: ticket, flag: flag) }
                catch { fail(error, ticket: ticket) }
            }
        }
    }

    func previousPage() {
        guard canPreviousPage, let page else { return }
        loadPage(start: page.offset >= AtlasWorkspaceStructureLimits.page ? page.offset - AtlasWorkspaceStructureLimits.page : 0)
    }
    func nextPage() {
        guard canNextPage, let offset = page?.nextOffset else { return }
        loadPage(start: offset)
    }
    private func loadPage(start: UInt64) {
        guard canSelect, let session, let page else { return }
        let summary = page.summary, root = page.summary.root
        let (ticket, flag) = begin("Reading a retained result page…")
        task = Task {
            do {
                let result = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                    let page = try AtlasWorkspaceStructurePage.decode(
                        session.transport.page(session.handle, summary.generation, summary.revision, start, AtlasWorkspaceStructureLimits.page),
                        command: "page", owner: session.handle, generation: summary.generation, root: root,
                        request: summary.request, offset: start)
                    guard page.summary == summary else { throw AtlasWorkspaceStructureError.invalidResponse }
                    return page
                }
                guard accepts(ticket, flag, session: session) else { return }
                self.page = result; finish(ticket)
                notice = "Retained result page loaded without reading source files."
            } catch { fail(error, ticket: ticket) }
        }
    }

    /// A later window reobserves the project. Omitted labels/captures can make
    /// matching ordinals noncontiguous, so use the actual last retained ordinal.
    func laterWindow() {
        guard canLaterWindow, let session, let page else { return }
        let summary = page.summary, root = page.summary.root
        let (ticket, flag) = begin("Finding the next observed match window…")
        task = Task {
            do {
                let last = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                    let page = try AtlasWorkspaceStructurePage.decode(
                        session.transport.page(session.handle, summary.generation, summary.revision, summary.retainedHits - 1, 1),
                        command: "page", owner: session.handle, generation: summary.generation, root: root,
                        request: summary.request, offset: summary.retainedHits - 1, count: 1)
                    guard page.summary == summary, let hit = page.rows.first,
                          hit.occurrence > summary.request.startAfter else { throw AtlasWorkspaceStructureError.invalidResponse }
                    return hit.occurrence
                }
                guard accepts(ticket, flag, session: session) else { return }
                guard last < summary.matchesSeen else {
                    finish(ticket)
                    notice = "No later observed matches. Omitted labels or captures require a narrower query."
                    return
                }
                var windows = previousWindows
                windows.append(summary.request.startAfter)
                finish(ticket)
                start(startAfter: last, windows: windows)
            } catch { fail(error, ticket: ticket) }
        }
    }
    func earlierWindow() {
        guard canEarlierWindow, let previous = previousWindows.last else { return }
        start(startAfter: previous, windows: Array(previousWindows.dropLast()))
    }

    func open(_ target: AtlasWorkspaceStructureTarget) {
        guard canSelect, let session, let page, page.admits(target), let registryDomain else { return }
        let summary = page.summary, root = page.summary.root
        let next = makeReader()
        let (ticket, flag) = begin("Opening the exact retained source at original byte \(target.hit.range.start)…")
        pendingReader = next
        task = Task {
            do {
                try await next.openRetained(registryDomain: registryDomain) { destination in
                    guard !flag.isCanceled else { throw AtlasWorkspaceStructureError.canceled }
                    let receipt = try AtlasWorkspaceStructureReaderReceipt.decode(
                        session.transport.openReader(session.handle, destination, summary.generation, summary.revision, target.hit.index),
                        target: target, summary: summary, root: root, destination: destination)
                    guard !flag.isCanceled else { throw AtlasWorkspaceStructureError.canceled }
                    return AtlasReaderRetainedInitialization(info: receipt.info, initialOriginalOffset: receipt.initialOffset)
                }
                guard accepts(ticket, flag, session: session), pendingReader === next,
                      authorized, self.page?.admits(target) == true else {
                    next.close()
                    if operation == ticket { pendingReader = nil; finish(ticket) }
                    return
                }
                let previous = currentReader
                currentReader = .init(hit: target.hit, reader: next)
                pendingReader = nil; finish(ticket)
                previous?.reader.close()
                notice = "Opened the retained capture at original byte \(target.hit.range.start). Candidate range: "
                    + "\(target.hit.range.start)..<\(target.hit.range.end). No live source was reopened."
            } catch {
                next.close()
                if operation == ticket { pendingReader = nil; finish(ticket); notice = message(error) }
            }
        }
    }
    func closeReader() {
        pendingReader?.close(); pendingReader = nil
        currentReader?.reader.close(); currentReader = nil
    }

    func cancel() {
        cancelWork(retireSession: true)
        notice = "Search canceled. Displayed counts describe only the last accepted batch; start a new search to select results."
    }
    func close() {
        cancelWork(retireSession: true)
        closeReader(); closed = true; authority = nil; page = nil; previousWindows = []
    }
    private func edited() {
        guard !closed else { return }
        cancelWork(retireSession: true)
        previousWindows = []
        notice = "Search inputs changed. Start a new search; any open retained source is unchanged."
    }
    private func begin(_ notice: String) -> (UUID, AtlasSearchCancellation) {
        let ticket = UUID(), flag = AtlasSearchCancellation()
        operation = ticket; cancellation = flag; busy = true; running = false; pausing = false; stopping = false
        self.notice = notice
        return (ticket, flag)
    }
    private func cancelWork(retireSession: Bool) {
        operation = UUID(); cancellation?.cancel(); task?.cancel(); io.cancel()
        pendingReader?.close(); pendingReader = nil
        if retireSession { session?.cancel(); session = nil; authorized = false }
        cancellation = nil; task = nil; busy = false; running = false; pausing = false; stopping = false
    }
    private func accepts(_ ticket: UUID, _ flag: AtlasSearchCancellation, session: AtlasWorkspaceStructureSession) -> Bool {
        !closed && operation == ticket && !flag.isCanceled && !Task.isCancelled && self.session === session
    }
    private func finish(_ ticket: UUID) {
        guard operation == ticket else { return }
        task = nil; cancellation = nil; busy = false; running = false; pausing = false; stopping = false
    }
    private func fail(_ error: Error, ticket: UUID) {
        guard operation == ticket else { return }
        cancelWork(retireSession: true); notice = message(error)
    }
    private func message(_ error: Error) -> String {
        if Task.isCancelled { return AtlasWorkspaceStructureError.canceled.message }
        return (error as? AtlasWorkspaceStructureError)?.message
            ?? (error as? AtlasReaderError)?.message ?? "The operation could not finish. Previously opened source is unchanged."
    }
}

