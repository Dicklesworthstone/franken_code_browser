import Foundation
import Observation
import Dispatch

struct AtlasObservedReaderSourceIdentity: Equatable, Sendable {
    let input: AtlasObservedReaderInput
    let project: UUID
    let grant: ObjectIdentifier?
}

/// Created by a current-project UI action, never by a saved source label or an
/// old exact search anchor. The model retains this authority until close/drain.
@MainActor struct AtlasObservedReaderSource: Identifiable {
    let id = UUID()
    let identity: AtlasObservedReaderSourceIdentity
    let access: AnyObject?
    let currentMatch: AtlasObservedReaderExternalMatch?
    var input: AtlasObservedReaderInput { identity.input }
    init(input: AtlasObservedReaderInput, project: UUID, access: AnyObject?,
         currentMatch: AtlasObservedReaderExternalMatch? = nil) {
        identity = .init(input: input, project: project, grant: access.map(ObjectIdentifier.init))
        self.access = access
        self.currentMatch = currentMatch
    }
}

/// The host can revoke an open sheet synchronously when project authority
/// changes; it does not wait for a dismissal animation or onDisappear callback.
@MainActor final class AtlasObservedReaderPresentation: Identifiable {
    let source: AtlasObservedReaderSource
    let model: AtlasObservedReaderModel
    let id: UUID
    private var started = false
    private var retired = false
    init(source: AtlasObservedReaderSource, transport: AtlasObservedReaderTransport) {
        self.source = source; id = source.id; model = AtlasObservedReaderModel(transport: transport)
    }
    func start() {
        guard !started, !retired else { return }
        started = true; model.open(source)
    }
    func close() { retired = true; model.close() }
}

/// Native access objects are retained only for lifetime, never invoked by the
/// worker. Registry retirement releases the descriptor off the interaction thread.
private final class AtlasObservedReaderLease: @unchecked Sendable {
    let access: AnyObject?
    init(_ access: AnyObject?) { self.access = access }
}

/// Admission survives failed retirement, so the quarantine is bounded even
/// when every close is refused. A fifth native owner is never created here.
private final class AtlasObservedReaderPermit: @unchecked Sendable {
    private static let state = State()
    private var acquired = false
    private final class State: @unchecked Sendable {
        let lock = NSLock()
        var count = 0
    }
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

private final class AtlasObservedReaderRetirement: @unchecked Sendable {
    static let shared = AtlasObservedReaderRetirement()
    private let queue = DispatchQueue(label: "dev.frankencode.browser.observed-reader-retirement", qos: .utility)
    private let retryLock = NSLock()
    private var retryScheduled = false // Protected by retryLock, including while the pass runs.
    private var quarantine: [Record] = [] // Accessed only on queue; at most four permits.
    final class Record: @unchecked Sendable {
        let handle: UInt64
        let transport: AtlasObservedReaderTransport
        let lease: AtlasObservedReaderLease
        let permit: AtlasObservedReaderPermit
        init(handle: UInt64, transport: AtlasObservedReaderTransport,
             lease: AtlasObservedReaderLease, permit: AtlasObservedReaderPermit) {
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
    /// One bounded retry pass in response to explicit opening/reopening or
    /// another retirement. No timer, polling loop or grant release on failure.
    func retry() {
        retryLock.lock()
        guard !retryScheduled else { retryLock.unlock(); return }
        retryScheduled = true
        retryLock.unlock()
        queue.async { [self] in
            drain()
            retryLock.lock(); retryScheduled = false; retryLock.unlock()
        }
    }
    private func drain() {
        let pending = quarantine
        quarantine = []
        for record in pending where !record.close() { quarantine.append(record) }
    }
}

private final class AtlasObservedReaderSession: @unchecked Sendable {
    let handle: UInt64
    let transport: AtlasObservedReaderTransport
    private let lease: AtlasObservedReaderLease
    private let permit: AtlasObservedReaderPermit
    init(transport: AtlasObservedReaderTransport, access: AnyObject?) throws {
        AtlasObservedReaderRetirement.shared.retry()
        guard let permit = AtlasObservedReaderPermit() else { throw AtlasObservedReaderError.unavailable }
        let handle = transport.create()
        guard handle > 0 else { throw AtlasObservedReaderError.unavailable }
        self.handle = handle; self.transport = transport; lease = AtlasObservedReaderLease(access)
        self.permit = permit
    }
    func cancel() { _ = transport.cancel(handle) }
    deinit {
        AtlasObservedReaderRetirement.shared.retire(.init(handle: handle, transport: transport,
            lease: lease, permit: permit))
    }
}

@Observable @MainActor final class AtlasObservedReaderModel {
    private(set) var info: AtlasObservedReaderInfo?
    private(set) var page: AtlasObservedReaderPage?
    private(set) var source: AtlasSource?
    private(set) var navigation = UUID()
    private(set) var busy = false
    private(set) var closed = false
    private(set) var notice = "Choose an original-byte offset to observe a bounded range of the opened file."
    private(set) var searchReport: AtlasObservedReaderSearchReport?
    private(set) var selectedSearchWitness: AtlasObservedReaderSearchWitness?
    private(set) var searchContext: AtlasObservedReaderSearchContext?
    private(set) var externalContext: AtlasObservedReaderSearchContext?
    private(set) var searchSelection: NSRange?
    private(set) var searching = false
    private(set) var searchAuthorized = false
    var searchText = "" {
        didSet { if !oldValue.utf8.elementsEqual(searchText.utf8) { revokeSearch() } }
    }
    var searchScanBytes: UInt64 = AtlasObservedReaderSearchLimits.scanBytes {
        didSet { if oldValue != searchScanBytes { revokeSearch() } }
    }
    var countAllSearchMatches = false {
        didSet { if oldValue != countAllSearchMatches { revokeSearch() } }
    }
    var searchCaseSensitive = true {
        didSet { if oldValue != searchCaseSensitive { revokeSearch() } }
    }
    var offsetInput = "0"
    var lineInput = "1" {
        didSet { if oldValue != lineInput { revokeLineRequest() } }
    }
    var lineCountInput = "20" {
        didSet { if oldValue != lineCountInput { revokeLineRequest() } }
    }
    var lineScanBytes: UInt64 = AtlasObservedReaderLimits.lineScanBytes {
        didSet { if oldValue != lineScanBytes { revokeLineRequest() } }
    }
    var encoding: AtlasObservedReaderEncoding = .utf8 {
        didSet { if oldValue != encoding { revokeSearch(); revokeLineRequest() } }
    }
    var pageBytes: UInt64 = AtlasObservedReaderLimits.pageBytes {
        didSet { if oldValue != pageBytes { revokeLineRequest() } }
    }
    @ObservationIgnored private let transport: AtlasObservedReaderTransport
    @ObservationIgnored private let io = AtlasProjectIO()
    @ObservationIgnored private var authority: AtlasObservedReaderSource?
    @ObservationIgnored private var session: AtlasObservedReaderSession?
    @ObservationIgnored private var pendingSession: AtlasObservedReaderSession?
    @ObservationIgnored private var flag: AtlasSearchCancellation?
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var operation = UUID()
    @ObservationIgnored private var searchOperation = false
    @ObservationIgnored private var lineOperation = false
    @ObservationIgnored private var lastSearchGeneration: UInt64 = 0
    @ObservationIgnored private var latestObservation: UInt64 = 0
    private var history: [AtlasObservedReaderPage] = []
    private var historyIndex = 0

    init(transport: AtlasObservedReaderTransport) { self.transport = transport }
    deinit { task?.cancel() }

    static func retryRetirements() { AtlasObservedReaderRetirement.shared.retry() }

    var canBack: Bool { !busy && historyIndex > 0 }
    var canForward: Bool { !busy && historyIndex + 1 < history.count }
    var canPrevious: Bool { !busy && (page?.visibleRange.start ?? 0) > 0 }
    var canNext: Bool {
        guard !busy, let page, let next = page.nextOffset else { return false }
        return next > page.observation.request.offset
    }
    var canObserve: Bool { !closed && session != nil }
    var canReadLines: Bool { canObserve && transport.lines != nil && (try? lineRequest()) != nil }
    var canReopen: Bool { !closed && authority != nil }
    var canCopy: Bool { !busy && page != nil && session != nil && !closed }
    var canSearch: Bool {
        !closed && session != nil && transport.search != nil && lastSearchGeneration < UInt64.max
            && (try? AtlasObservedReaderSearchRequest(generation: 1, needle: searchText,
                encoding: encoding, maximumScanBytes: searchScanBytes,
                countAllMatches: countAllSearchMatches, caseSensitive: searchCaseSensitive)) != nil
    }
    var canSelectSearchHit: Bool {
        searchAuthorized && !closed && !busy && session != nil && !(searchReport?.hits.isEmpty ?? true)
    }
    var canReadSearchContext: Bool { canSelectSearchHit && selectedSearchWitness != nil }
    var canGoToVerifiedMatch: Bool { canReadSearchContext && transport.search?.observeHit != nil }
    var searchCoverage: String? {
        guard let searchReport else { return nil }
        return (searchAuthorized ? "" : "Previous Find results are read-only. Run Find again before navigating them. ") + searchReport.coverage
    }
    var verifiedPageNotice: String? {
        guard let hit = page?.findHit else { return nil }
        guard let report = searchReport, hit.generation == report.request.generation,
              hit.searchObservation == report.observation else {
            return "Verified match retained from an earlier Find."
        }
        guard selectedSearchWitness?.target.hit.index == hit.index else {
            return "Verified match retained from another result of this Find."
        }
        return nil
    }

    var coverage: String {
        var message = "Each range is a separate observation of the opened file object, not an immutable whole-file snapshot. Other bytes remain unknown."
        if let page {
            let observation = page.observation
            if page.lineNavigation == nil && observation.consistency != "unchanged-metadata-not-atomic" {
                message += " The file changed, ended early, or its final metadata was unavailable during this observation (\(observation.consistency))."
            }
            if page.hasReplacements { message += " Decoded text contains replacements; original hex preserves the observed bytes." }
            if page.boundariesAdjusted { message += " Visible boundaries were adjusted for source scalars or CRLF." }
        }
        if page?.lineNavigation != nil {
            message += " Physical line labels and retained bytes came from the same bounded prefix scan. Matching before/after metadata does not make the scan atomic."
        } else { message += " Layout and line labels are local to this range." }
        return message + " Back/Forward and original-byte copy use retained observations without rereading. Reopen current file explicitly follows pathname replacement."
    }

    func open(_ source: AtlasObservedReaderSource) {
        close()
        authority = source; closed = false
        reopenCurrentFile()
    }

    /// Opening a new descriptor is an explicit action. Keep the old descriptor,
    /// observation and history until the candidate's first range validates.
    func reopenCurrentFile() {
        guard canReopen, let authority else { return }
        revokeSearch()
        let request: AtlasObservedReaderRequest
        do {
            if let match = authority.currentMatch {
                guard match.input == authority.input else { throw AtlasObservedReaderError.invalidInput }
                request = try match.request(); encoding = match.encoding
            } else { request = try AtlasObservedReaderRequest(offset: 0, bytes: pageBytes, encoding: encoding) }
        }
        catch { notice = AtlasObservedReaderError.invalidInput.message; return }
        let candidate: AtlasObservedReaderSession
        do { candidate = try AtlasObservedReaderSession(transport: transport, access: authority.access) }
        catch { notice = message(error); return }
        let (ticket, flag) = begin("Opening the current pathname and observing its first bounded range…")
        pendingSession = candidate
        let input = authority.input, currentMatch = authority.currentMatch
        task = Task {
            do {
                let accepted = try await io.perform(cancellation: flag, keepingAlive: [candidate]) {
                    let info = try AtlasObservedReaderInfo.decode(
                        candidate.transport.open(candidate.handle, input, flag), handle: candidate.handle, input: input, initial: true)
                    guard !flag.isCanceled else { throw AtlasObservedReaderError.canceled }
                    let page = try AtlasObservedReaderPage.decode(
                        candidate.transport.window(candidate.handle, request, flag), handle: candidate.handle,
                        input: input, request: request)
                    return (info, page, currentMatch?.verify(context: page))
                }
                guard accepts(ticket, flag), pendingSession === candidate else { return }
                session = candidate; pendingSession = nil; info = accepted.0
                history = []; historyIndex = 0; latestObservation = 0
                install(accepted.1, append: true)
                externalContext = accepted.2
                if let verification = accepted.2 { searchSelection = verification.selection }
                finish(ticket)
                notice = accepted.2.map { "Opened the current pathname as a new observation. " + $0.message }
                    ?? "Opened a new file descriptor and retained observation \(accepted.1.observation.id). No whole-file capture was created."
            } catch { fail(error, ticket: ticket) }
        }
    }

    func observeInput() {
        guard let offset = UInt64(offsetInput), String(offset) == offsetInput else {
            notice = AtlasObservedReaderError.invalidInput.message; return
        }
        observe(offset: offset)
    }
    func previousRange() {
        guard canPrevious, let page else { return }
        observe(offset: page.visibleRange.start > pageBytes ? page.visibleRange.start - pageBytes : 0)
    }
    func nextRange() {
        guard canNext, let next = page?.nextOffset else { return }
        observe(offset: next)
    }
    func observeAgain() {
        guard let page else { return }
        if let lines = page.lineNavigation {
            lineInput = String(lines.requestedFirstLine); lineCountInput = String(lines.requestedLines)
            lineScanBytes = lines.maximumScanBytes; pageBytes = lines.maximumSelectedBytes
            encoding = page.observation.request.encoding
            observeLineInput()
        } else { observe(offset: page.observation.request.offset) }
    }

    private func lineRequest() throws -> AtlasObservedReaderLineRequest {
        guard let first = UInt64(lineInput), String(first) == lineInput,
              let count = UInt64(lineCountInput), String(count) == lineCountInput else {
            throw AtlasObservedReaderError.invalidInput
        }
        return try .init(firstLine: first, lines: count, maximumSelectedBytes: pageBytes,
            maximumScanBytes: lineScanBytes, encoding: encoding)
    }

    /// Resolve and retain complete physical lines from one forward scan of the
    /// existing descriptor. The host does not reread the discovered byte range.
    /// Every request field is frozen and rechecked before native publication.
    func observeLineInput() {
        guard canObserve, let session, let input = authority?.input,
              let readLines = session.transport.lines else { return }
        let request: AtlasObservedReaderLineRequest
        do { request = try lineRequest() }
        catch { notice = "Enter a one-based line and 1–4096 lines, with a valid range and scan allowance."; return }
        let (ticket, flag) = begin("Finding line \(request.firstLine) within a \(request.maximumScanBytes)-byte scan of the opened file…")
        lineOperation = true
        task = Task {
            do {
                let page = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                    try AtlasObservedReaderPage.decode(readLines(session.handle, request, flag),
                        handle: session.handle, input: input, lines: request)
                }
                guard accepts(ticket, flag), self.session === session,
                      (try? self.lineRequest()) == request else { return }
                guard page.observation.id > latestObservation, let lines = page.lineNavigation else {
                    throw AtlasObservedReaderError.invalidResponse
                }
                install(page, append: true); finish(ticket)
                notice = "Retained physical lines \(lines.firstLine)–\(lines.lastLine) in observation \(page.observation.id). The same scan read \(lines.payloadBytesRead) bytes; earlier observations keep their original bytes."
            } catch { fail(error, ticket: ticket) }
        }
    }

    /// Latest requested window wins; one active operation and one waiting
    /// request are bounded by AtlasProjectIO. Failed/canceled reads never clear
    /// a valid page or automatically repeat I/O under an old observation ID.
    func observe(offset: UInt64) {
        guard canObserve, let session, let input = authority?.input else { return }
        let request: AtlasObservedReaderRequest
        do { request = try AtlasObservedReaderRequest(offset: offset, bytes: pageBytes, encoding: encoding) }
        catch { notice = AtlasObservedReaderError.invalidInput.message; return }
        let (ticket, flag) = begin("Reading a new bounded observation at byte \(offset)…")
        task = Task {
            do {
                let page = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                    try AtlasObservedReaderPage.decode(session.transport.window(session.handle, request, flag),
                        handle: session.handle, input: input, request: request)
                }
                guard accepts(ticket, flag), self.session === session else { return }
                // Spent/canceled IDs may leave gaps, but an accepted new read
                // cannot impersonate an earlier observation in this handle.
                guard page.observation.id > latestObservation else {
                    throw AtlasObservedReaderError.invalidResponse
                }
                install(page, append: true); finish(ticket)
                notice = "Retained observation \(page.observation.id) from the opened file. Earlier observations keep their original bytes."
            } catch { fail(error, ticket: ticket) }
        }
    }

    func back() { if canBack { restore(historyIndex - 1) } }
    func forward() { if canForward { restore(historyIndex + 1) } }
    private func restore(_ index: Int) {
        guard history.indices.contains(index), let session, let input = authority?.input else { return }
        let expected = history[index]
        let (ticket, flag) = begin("Restoring the retained observation without reading the file…")
        task = Task {
            do {
                let page = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                    try AtlasObservedReaderPage.decode(session.transport.retained(session.handle, expected.observation.id),
                        handle: session.handle, input: input, retained: expected)
                }
                guard accepts(ticket, flag), self.session === session else { return }
                historyIndex = index; install(page, append: false); finish(ticket)
                notice = "Restored observation \(page.observation.id) without reading any source bytes."
            } catch { fail(error, ticket: ticket) }
        }
    }

    func copyOriginalHex() async -> String? {
        guard canCopy, let session, let input = authority?.input, let page else { return nil }
        let (ticket, flag) = begin("Copying original bytes from this retained observation…")
        do {
            let copied = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                try AtlasObservedReaderCopy.decode(session.transport.copy(session.handle, page.observation.id,
                    page.visibleRange.start, page.visibleRange.end), handle: session.handle, input: input, page: page)
            }
            guard accepts(ticket, flag), self.session === session,
                  self.page?.observation == page.observation else { return nil }
            finish(ticket)
            notice = "Original bytes verified against retained observation \(page.observation.id); no source reread."
            return copied.originalHex
        } catch { fail(error, ticket: ticket); return nil }
    }

    /// Find shares the same owner, grant and one-active/one-pending queue as
    /// range reads. A failed same-query replacement preserves its old receipt;
    /// editing any query input revokes the receipt before another call begins.
    func find() {
        guard canSearch, let session, let input = authority?.input,
              let search = session.transport.search else { return }
        let request: AtlasObservedReaderSearchRequest
        do {
            request = try .init(generation: lastSearchGeneration + 1, needle: searchText,
                encoding: encoding, maximumScanBytes: searchScanBytes,
                countAllMatches: countAllSearchMatches, caseSensitive: searchCaseSensitive)
        } catch { notice = "Enter text of 1–4096 UTF-8 bytes without NUL, and choose a bounded scan limit."; return }
        lastSearchGeneration = request.generation
        // A replacement can commit in Rust before its reply is canceled or
        // rejected. Old literal bytes stay readable, but their query cannot
        // lend authority to more hit actions without an accepted new receipt.
        searchAuthorized = false
        let (ticket, flag) = begin("Searching up to \(request.maximumScanBytes) bytes of the opened file…")
        searchOperation = true; searching = true
        task = Task {
            do {
                let report = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                    try AtlasObservedReaderSearchReport.decode(search.find(session.handle, request, flag),
                        handle: session.handle, input: input, request: request)
                }
                guard accepts(ticket, flag), self.session === session,
                      searchText.utf8.elementsEqual(request.needle.utf8), encoding == request.encoding,
                      searchScanBytes == request.maximumScanBytes,
                      countAllSearchMatches == request.countAllMatches,
                      searchCaseSensitive == request.caseSensitive else { return }
                guard report.observation > latestObservation else { throw AtlasObservedReaderError.invalidResponse }
                latestObservation = report.observation
                searchReport = report; searchAuthorized = true; selectedSearchWitness = nil; clearSearchContext()
                finish(ticket); notice = report.countLabel + ". " + report.coverage
            } catch { fail(error, ticket: ticket) }
        }
    }

    /// Selection fetches the retained matching literal only, with zero source
    /// reads. It never invents the surrounding page at search time.
    func selectSearchHit(_ target: AtlasObservedReaderSearchTarget) {
        guard canSelectSearchHit, let report = searchReport, report.admits(target),
              let session, let input = authority?.input, let search = session.transport.search else { return }
        let (ticket, flag) = begin("Verifying the retained literal for result \(target.hit.index + 1)…")
        selectedSearchWitness = nil; clearSearchContext(); searchOperation = true
        task = Task {
            do {
                let witness = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                    try AtlasObservedReaderSearchWitness.decode(search.hit(session.handle, target.generation, target.hit.index),
                        handle: session.handle, input: input, report: report, target: target)
                }
                guard accepts(ticket, flag), self.session === session, searchReport?.admits(target) == true else { return }
                selectedSearchWitness = witness; finish(ticket)
                notice = "Retained literal verified for original bytes \(target.hit.range.start)..<\(target.hit.range.end). No source bytes were reread."
            } catch { fail(error, ticket: ticket) }
        }
    }

    func nextSearchHit() {
        guard canSelectSearchHit, let report = searchReport else { return }
        let current = selectedSearchWitness.flatMap { report.admits($0.target) ? $0.target.hit.index : nil }
        let index = current.map { ($0 + 1) % UInt64(report.hits.count) } ?? 0
        selectSearchHit(report.target(for: report.hits[Int(index)]))
    }
    func previousSearchHit() {
        guard canSelectSearchHit, let report = searchReport else { return }
        let count = UInt64(report.hits.count)
        let current = selectedSearchWitness.flatMap { report.admits($0.target) ? $0.target.hit.index : nil }
        let index = current.map { ($0 + count - 1) % count } ?? (count - 1)
        selectSearchHit(report.target(for: report.hits[Int(index)]))
    }

    /// This action deliberately reads new bytes and labels them as new context.
    /// A changed, shortened or undecodable context cannot borrow an old match's
    /// highlight. The verified literal stays separately available either way.
    func readSearchContext() {
        guard canReadSearchContext, let witness = selectedSearchWitness,
              searchReport?.admits(witness.target) == true,
              let session, let input = authority?.input else { return }
        let range = witness.target.hit.range
        let before = min(range.start, 1024)
        let request: AtlasObservedReaderRequest
        do {
            request = try .init(offset: range.start - before,
                bytes: max(pageBytes, before + range.count + AtlasObservedReaderLimits.contextBytes),
                encoding: witness.encoding)
        } catch { notice = AtlasObservedReaderError.invalidInput.message; return }
        let (ticket, flag) = begin("Reading new context for the selected retained literal…")
        clearSearchContext(); searchOperation = true
        task = Task {
            do {
                let accepted = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                    let page = try AtlasObservedReaderPage.decode(session.transport.window(session.handle, request, flag),
                        handle: session.handle, input: input, request: request)
                    return (page, witness.verify(context: page))
                }
                guard accepts(ticket, flag), self.session === session,
                      selectedSearchWitness?.target == witness.target,
                      searchReport?.admits(witness.target) == true else { return }
                guard accepted.0.observation.id > max(latestObservation, witness.target.observation) else {
                    throw AtlasObservedReaderError.invalidResponse
                }
                install(accepted.0, append: true)
                searchContext = accepted.1; searchSelection = accepted.1.selection
                finish(ticket); notice = accepted.1.message
            } catch { fail(error, ticket: ticket) }
        }
    }

    /// Only the shared engine may publish this new context after verifying
    /// the old literal and its exact decoded range. A changed hit leaves the
    /// accepted page/history untouched; ordinary current-context reading is
    /// still available as its separate explicit action above.
    func goToVerifiedMatch() {
        guard canGoToVerifiedMatch, let witness = selectedSearchWitness,
              searchReport?.admits(witness.target) == true,
              let session, let input = authority?.input,
              let observeHit = session.transport.search?.observeHit else { return }
        let range = witness.target.hit.range, before = min(witness.target.hit.range.start, 1024)
        let request: AtlasObservedReaderRequest
        do {
            request = try .init(offset: range.start - before,
                bytes: max(pageBytes, before + range.count + AtlasObservedReaderLimits.contextBytes),
                encoding: witness.encoding)
        } catch { notice = AtlasObservedReaderError.invalidInput.message; return }
        let (ticket, flag) = begin("Reading fresh context and verifying the selected match…")
        searchOperation = true
        task = Task {
            do {
                let page = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                    try AtlasObservedReaderPage.decode(observeHit(session.handle, witness, request, flag),
                        handle: session.handle, input: input, request: request, hit: witness)
                }
                guard accepts(ticket, flag), self.session === session, searchAuthorized,
                      selectedSearchWitness?.target == witness.target,
                      searchReport?.admits(witness.target) == true else { return }
                guard page.observation.id > max(latestObservation, witness.target.observation) else {
                    throw AtlasObservedReaderError.invalidResponse
                }
                install(page, append: true)
                finish(ticket)
                notice = "Match verified in new observation \(page.observation.id). Its exact selection is retained with this page; Back restores earlier bytes."
            } catch { fail(error, ticket: ticket) }
        }
    }

    private func clearSearchContext() {
        // A verified-hit page owns its selection independently of the current
        // query. Revoking query/external context cannot invalidate that retained
        // page receipt or make its exact highlight disappear during a failed
        // replacement operation.
        let retainedSelection = page?.verifiedSearchSelection
        if searchSelection != retainedSelection { navigation = UUID() }
        searchSelection = retainedSelection; searchContext = nil; externalContext = nil
    }
    private func revokeSearch() {
        if searchOperation { cancelWork() }
        searchReport = nil; searchAuthorized = false; selectedSearchWitness = nil; clearSearchContext()
    }
    private func revokeLineRequest() {
        guard lineOperation else { return }
        cancelWork()
        notice = "Line request changed. The accepted observation is unchanged; the earlier scan may still be draining."
    }

    func cancel() {
        cancelWork()
        notice = "Operation canceled. Accepted observations and retained search literals are unchanged; entered work may still be draining."
    }
    func close() {
        cancelWork(); closed = true
        session = nil; pendingSession = nil; authority = nil
        info = nil; page = nil; source = nil; history = []; historyIndex = 0
        latestObservation = 0
        searchReport = nil; searchAuthorized = false; selectedSearchWitness = nil; searchContext = nil; searchSelection = nil; externalContext = nil
        navigation = UUID()
    }
    private func begin(_ notice: String) -> (UUID, AtlasSearchCancellation) {
        cancelWork()
        let ticket = UUID(), flag = AtlasSearchCancellation()
        operation = ticket; self.flag = flag; busy = true; self.notice = notice
        return (ticket, flag)
    }
    private func cancelWork() {
        operation = UUID(); flag?.cancel(); task?.cancel(); io.cancel()
        session?.cancel(); pendingSession?.cancel()
        task = nil; flag = nil; pendingSession = nil; busy = false; searching = false; searchOperation = false; lineOperation = false
    }
    private func accepts(_ ticket: UUID, _ flag: AtlasSearchCancellation) -> Bool {
        !closed && operation == ticket && !flag.isCanceled && !Task.isCancelled
    }
    private func finish(_ ticket: UUID) {
        guard operation == ticket else { return }
        busy = false; flag = nil; task = nil; pendingSession = nil; searching = false; searchOperation = false; lineOperation = false
    }
    private func fail(_ error: Error, ticket: UUID) {
        guard operation == ticket else { return }
        finish(ticket); notice = message(error)
    }
    private func message(_ error: Error) -> String {
        if error is CancellationError { return AtlasObservedReaderError.canceled.message }
        if let error = error as? AtlasObservedReaderError {
            if case .engine(let code) = error {
                switch code {
                case "OBSERVED_READER_SEARCH_HIT_CHANGED":
                    return "The selected match changed or disappeared. The accepted page is unchanged. Run Find again, or read current context explicitly."
                case "OBSERVED_READER_LINE_MISSING":
                    return "The scan reached the end of the opened file before the requested line. The accepted page is unchanged."
                case "OBSERVED_READER_LINE_SCAN_BYTE_LIMIT":
                    return "The scan allowance ended before the requested lines and decoding context were resolved. Increase Scan up to and try again; the line is not known to be missing. The accepted page is unchanged."
                case "OBSERVED_READER_LINE_OUTPUT_BYTE_LIMIT":
                    return "The requested complete lines exceed the selected Range allowance. Request fewer lines or a larger Range; no truncated line replaced the accepted page."
                case "OBSERVED_READER_LINE_READ_CALL_LIMIT":
                    return "The scan reached its read-call limit before resolving the requested lines. The line is not known to be missing. The accepted page is unchanged."
                default: break
                }
            }
            return error.message
        }
        if let error = error as? AtlasProjectIOError, case .canceled = error { return AtlasObservedReaderError.canceled.message }
        return AtlasObservedReaderError.invalidResponse.message
    }
    private func install(_ page: AtlasObservedReaderPage, append: Bool) {
        clearSearchContext()
        // Updating controls to an accepted historical request must not revoke
        // the operation that is installing that exact immutable receipt.
        lineOperation = false
        if append {
            if historyIndex + 1 < history.count { history.removeSubrange((historyIndex + 1)..<history.count) }
            history.append(page)
            if history.count > AtlasObservedReaderLimits.retainedObservations { history.removeFirst() }
            historyIndex = history.count - 1
        }
        self.page = page
        latestObservation = max(latestObservation, page.observation.id)
        source = AtlasSource(path: page.observation.identity.displayPath, text: page.text)
        searchSelection = page.verifiedSearchSelection
        if let hit = page.findHit, let witness = selectedSearchWitness,
           page.observation.identity == witness.target.identity,
           searchReport?.admits(witness.target) == true,
           hit.generation == witness.target.generation, hit.searchObservation == witness.target.observation,
           hit.index == witness.target.hit.index, hit.occurrence == witness.target.hit.occurrence,
           hit.originalRange == witness.target.hit.range {
            searchContext = .verified(selection: searchSelection)
        }
        offsetInput = String(page.observation.request.offset)
        if let lines = page.lineNavigation {
            lineInput = String(lines.requestedFirstLine); lineCountInput = String(lines.requestedLines)
            lineScanBytes = lines.maximumScanBytes
        }
        navigation = UUID()
    }
}
