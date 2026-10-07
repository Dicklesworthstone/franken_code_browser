import Foundation
import Observation

/// Presentation state for one independently opened retained capture. It never
/// accepts workspace hit coordinates or promotes a page into atlas capture
/// evidence. Only committed navigation changes the current page/history.
@Observable @MainActor final class AtlasPagedReaderModel {
    let documentPreview: AtlasDocumentPreviewModel
    private(set) var page: AtlasReaderPage?
    /// Presentation-only UTF-8 of this logical window, not original file bytes.
    /// Never inserted into the atlas/source capture tables or searched as a file.
    private(set) var source: AtlasSource?
    private(set) var navigation = UUID()
    private(set) var busy = false
    private(set) var notice = ""
    var offsetInput = "0"
    var lineInput = "1"
    var fileQuery = "" {
        didSet {
            // Revoke synchronously with the edit, not in a later SwiftUI
            // onChange that could cancel a newly submitted Return/Find action.
            if !oldValue.utf8.elementsEqual(fileQuery.utf8) { clearFileSearch() }
        }
    }
    private(set) var findReport: AtlasReaderFindReport?
    private(set) var selectedHitIndex: Int?
    private var selectedRange: NSRange?
    @ObservationIgnored private var queryWork = false
    @ObservationIgnored private var outlineWork = false
    private(set) var outlinePage: AtlasOutlinePage?
    private(set) var hasOutline = false
    private(set) var outlineRowsAuthorized = false
    private var outlineHistory: [AtlasOutlinePageRequest] = []
    var outlineNeedle = "" {
        didSet { if !oldValue.utf8.elementsEqual(outlineNeedle.utf8) { clearOutlineFilter() } }
    }
    var outlineMode: AtlasOutlineNameMode = .contains {
        didSet { if oldValue != outlineMode { clearOutlineFilter() } }
    }
    var outlineLanguage: AtlasOutlineLanguage = .automatic {
        didSet { if oldValue != outlineLanguage { clearOutlineFilter(reset: true) } }
    }
    private var history: [AtlasReaderRequest] = []
    private var currentRequest: AtlasReaderRequest?
    @ObservationIgnored private let coordinator: AtlasReaderCoordinator
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var operation = UUID()

    init(transport: AtlasReaderTransport, search: AtlasReaderSearchTransport? = nil,
         outline: AtlasReaderOutlineTransport? = nil,
         document: AtlasReaderDocumentTransport? = nil) {
        let coordinator = AtlasReaderCoordinator(transport: transport, search: search,
            outline: outline, document: document)
        self.coordinator = coordinator
        documentPreview = AtlasDocumentPreviewModel(supported: document != nil,
            execute: { try await coordinator.document($0) },
            cancel: { coordinator.cancel() }, revoke: { coordinator.revokeDocument() })
    }
    deinit { task?.cancel() }

    var canGoBack: Bool { !history.isEmpty && !busy }
    var canGoNext: Bool { page?.nextOffset != nil && !busy }
    var canFind: Bool { page != nil && coordinator.supportsFind }
    var findIsCurrent: Bool { findReport?.needle.utf8.elementsEqual(fileQuery.utf8) == true }
    var canMoveHit: Bool { findIsCurrent && findReport?.hits.isEmpty == false && !busy }
    var nativeSelectionRange: NSRange? {
        if findIsCurrent, page?.selection?.target.generation == findReport?.generation,
           page?.selection != nil { return selectedRange }
        if outlineIsCurrent, let selection = page?.symbolSelection,
           selection.target.pageID == outlinePage?.id { return selectedRange }
        return nil
    }
    var matchHex: String? {
        guard findIsCurrent, let selection = page?.selection,
              selection.target.generation == findReport?.generation else { return nil }
        return selection.target.originalHex
    }

    var canBuildOutline: Bool { page != nil && coordinator.supportsOutline }
    var outlineIsCurrent: Bool {
        guard outlineRowsAuthorized, let outlinePage else { return false }
        return outlinePage.request.needle.utf8.elementsEqual(outlineNeedle.utf8)
            && (outlineNeedle.isEmpty || outlinePage.request.mode == outlineMode)
    }
    var canOutlineBack: Bool { !outlineHistory.isEmpty && !busy }
    var symbolHex: String? {
        guard outlineIsCurrent, let selection = page?.symbolSelection,
              selection.target.pageID == outlinePage?.id else { return nil }
        return selection.originalHex
    }
    var symbolSelectionLabel: String? {
        guard symbolHex != nil, let symbol = page?.symbolSelection?.target.symbol else { return nil }
        return symbol.nameRange == nil ? "Declaration evidence (no exact name span)" : "Exact identifier span"
    }

    var coverage: String {
        guard let page else { return "Retained capture limit: \(AtlasReaderLimits.captureBytes / 1024 / 1024) MiB. Oversized sources are refused, not truncated." }
        var messages = ["Logical page; layout is local to this window."]
        if let label = symbolSelectionLabel { messages.append("\(label); heuristic candidate, not semantic resolution.") }
        if matchHex != nil { messages.append("Selection belongs to this retained file query, not a workspace search capture.") }
        if page.rangeLimited { messages.append("Requested lines exceed the page byte limit. Next continues through the retained bytes.") }
        if page.boundariesAdjusted { messages.append("Page boundaries adjusted to complete source scalars or CRLF.") }
        if page.hasReplacements { messages.append("Display contains replacement characters. Original hex preserves the captured bytes.") }
        return messages.joined(separator: " ")
    }

    func open(root: String, path: String, accessLease: AnyObject?) {
        close()
        let input: AtlasReaderInput
        do { input = try AtlasReaderInput(root: root, path: path) }
        catch { notice = AtlasReaderError.invalidInput.message; return }
        let ticket = begin()
        notice = "Capturing source for bounded page navigation…"
        task = Task {
            do {
                let page = try await coordinator.open(input: input, accessLease: accessLease)
                guard operation == ticket, !Task.isCancelled else { return }
                install(page, request: .firstPage)
                finish(ticket)
            } catch { fail(error, ticket: ticket) }
        }
    }

    func next() {
        guard let offset = page?.nextOffset else { return }
        navigate(.window(offset: offset, bytes: AtlasReaderLimits.pageBytes))
    }
    func back() {
        guard let request = history.last else { return }
        navigate(request, goingBack: true)
    }
    func goToByte() {
        guard let offset = Self.number(offsetInput) else { notice = AtlasReaderError.invalidInput.message; return }
        navigate(.window(offset: offset, bytes: AtlasReaderLimits.pageBytes))
    }
    func goToLine() {
        guard let line = Self.number(lineInput), line > 0 else { notice = AtlasReaderError.invalidInput.message; return }
        navigate(.lines(first: line, count: 64, bytes: AtlasReaderLimits.pageBytes))
    }

    func find() {
        guard canFind else { return }
        let needle = fileQuery
        do { try AtlasReaderFindReport.validateNeedle(needle) }
        catch { notice = AtlasReaderError.invalidInput.message; return }
        clearFileSearch()
        let ticket = begin(query: true)
        notice = "Searching the retained file…"
        task = Task {
            do {
                let report = try await coordinator.find(needle)
                guard operation == ticket, !Task.isCancelled, fileQuery.utf8.elementsEqual(needle.utf8) else { return }
                findReport = report
                finish(ticket)
                notice = report.summary
                if !report.hits.isEmpty { moveHit(backwards: false) }
            } catch { fail(error, ticket: ticket) }
        }
    }
    func clearFileSearch() {
        if queryWork { cancel() }
        findReport = nil; selectedHitIndex = nil
        if page?.selection != nil { selectedRange = nil }
        coordinator.clearFind()
    }
    func moveHit(backwards: Bool) {
        guard canMoveHit, let report = findReport else { return }
        let count = report.hits.count
        let index: Int
        if let selected = selectedHitIndex {
            index = backwards ? (selected == 0 ? count - 1 : selected - 1) : (selected == count - 1 ? 0 : selected + 1)
        } else { index = backwards ? count - 1 : 0 }
        guard let target = report.target(at: index) else { return }
        navigate(.hit(target), hitIndex: index)
    }

    /// Extract once, independently of literal search. Unsupported or oversized
    /// source stays readable; engine diagnostics remain visible in the browser.
    func buildOutline() {
        guard canBuildOutline else { return }
        clearOutlineFilter(reset: true)
        let language = outlineLanguage
        let ticket = begin(outline: true)
        notice = "Extracting source outline…"
        task = Task {
            do {
                let page = try await coordinator.prepareOutline(language: language)
                guard operation == ticket, !Task.isCancelled, outlineLanguage == language else { return }
                outlinePage = page
                hasOutline = true
                outlineRowsAuthorized = true
                finish(ticket)
                notice = page.summary
                if !outlineNeedle.isEmpty { filterOutline() }
            } catch { fail(error, ticket: ticket) }
        }
    }

    func filterOutline() {
        guard hasOutline else { return }
        loadOutlinePage(AtlasOutlinePageRequest(needle: outlineNeedle, mode: outlineMode, start: 0, limit: 128), filtering: true)
    }
    func nextOutlinePage() {
        guard let page = outlinePage, let next = page.nextOffset else { return }
        loadOutlinePage(AtlasOutlinePageRequest(needle: page.request.needle,
            mode: page.request.mode, start: next, limit: 128))
    }
    func previousOutlinePage() {
        guard let request = outlineHistory.last else { return }
        loadOutlinePage(request, goingBack: true)
    }
    func openSymbol(_ target: AtlasOutlineTarget) {
        guard outlineIsCurrent, outlinePage?.target(id: target.symbol.id) == target else { return }
        navigate(.symbol(target))
    }

    /// Query edits invalidate the UI rows before a new asynchronous filter can
    /// return. They never remove a literal hit's independent activation/copy.
    private func clearOutlineFilter(reset: Bool = false) {
        if outlineWork { cancel() }
        outlinePage = nil
        outlineHistory = []
        outlineRowsAuthorized = false
        if page?.symbolSelection != nil { selectedRange = nil }
        if reset { coordinator.resetOutline(); hasOutline = false }
        else { coordinator.revokeOutlinePage() }
    }
    private func loadOutlinePage(_ request: AtlasOutlinePageRequest,
        goingBack: Bool = false, filtering: Bool = false) {
        do { try request.validate() }
        catch { notice = AtlasReaderError.invalidInput.message; return }
        let previous = outlinePage?.request
        let ticket = begin(outline: true)
        outlineRowsAuthorized = false
        notice = "Filtering retained outline…"
        task = Task {
            do {
                let result = try await coordinator.outlineSymbols(request)
                guard operation == ticket, !Task.isCancelled else { return }
                if goingBack { _ = outlineHistory.popLast() }
                else if filtering { outlineHistory = [] }
                else if let previous, previous != request {
                    outlineHistory.append(previous)
                    if outlineHistory.count > 64 { outlineHistory.removeFirst() }
                }
                outlinePage = result
                outlineRowsAuthorized = true
                finish(ticket)
                notice = result.summary
            } catch { fail(error, ticket: ticket) }
        }
    }

    /// Explicit cancellation keeps a successfully installed page. Cancellation
    /// during initial capture instead releases its handle after work drains.
    func cancel() {
        documentPreview.cancel(silent: true)
        operation = UUID()
        task?.cancel(); task = nil
        coordinator.cancel()
        if page == nil { coordinator.close() }
        busy = false
        queryWork = false
        outlineWork = false
        notice = page == nil ? "Source opening canceled." : "Navigation canceled. The current retained page is unchanged."
    }
    func close() {
        documentPreview.close()
        operation = UUID()
        task?.cancel(); task = nil
        coordinator.close()
        page = nil; source = nil; history = []; currentRequest = nil
        findReport = nil; selectedHitIndex = nil; selectedRange = nil
        queryWork = false; outlineWork = false
        fileQuery = ""
        outlinePage = nil; hasOutline = false; outlineRowsAuthorized = false
        outlineHistory = []; outlineNeedle = ""; outlineMode = .contains; outlineLanguage = .automatic
        busy = false; notice = ""
        offsetInput = "0"; lineInput = "1"
        navigation = UUID()
    }

    private func navigate(_ request: AtlasReaderRequest, goingBack: Bool = false, hitIndex: Int? = nil) {
        guard let previous = page, let previousRequest = currentRequest else { return }
        do { try request.validate(capturedBytes: previous.identity.capturedBytes) }
        catch { notice = AtlasReaderError.invalidInput.message; return }
        let isSymbol: Bool
        if case .symbol = request { isSymbol = true } else { isSymbol = false }
        let ticket = begin(query: hitIndex != nil, outline: isSymbol)
        notice = "Reading retained source…"
        task = Task {
            do {
                let next = try await coordinator.read(request)
                guard operation == ticket, !Task.isCancelled else { return }
                if goingBack { _ = history.popLast() }
                else if request != previousRequest {
                    history.append(previousRequest)
                    if history.count > 128 { history.removeFirst(history.count - 128) }
                }
                install(next, request: request)
                if let hitIndex { selectedHitIndex = hitIndex }
                finish(ticket)
            } catch { fail(error, ticket: ticket) }
        }
    }
    private func begin(query: Bool = false, outline: Bool = false) -> UUID {
        documentPreview.cancel(silent: true)
        task?.cancel()
        coordinator.cancel()
        operation = UUID()
        busy = true
        queryWork = query
        outlineWork = outline
        return operation
    }
    private func install(_ page: AtlasReaderPage, request: AtlasReaderRequest) {
        // History restores the source page, never a now-replaced query's hit.
        // An explicit Back does not silently regain old exact-hit authority.
        switch request {
        case .hit, .symbol:
            currentRequest = .window(offset: page.start, bytes: max(4, min(AtlasReaderLimits.maximumPageBytes, page.end - page.start)))
        default: currentRequest = request
        }
        self.page = page
        documentPreview.bind(identity: page.identity, sourceOffset: page.start)
        let source = AtlasSource(path: page.identity.displayPath, text: page.text)
        self.source = source
        selectedRange = page.selection.flatMap { source.utf16Range(byteStart: $0.utf8Start, byteEnd: $0.utf8End) }
            ?? page.symbolSelection.flatMap { source.utf16Range(byteStart: $0.utf8Start, byteEnd: $0.utf8End) }
        navigation = UUID()
        offsetInput = String(page.start)
        if let line = page.firstPhysicalLine { lineInput = String(line) }
        notice = ""
    }
    private func finish(_ ticket: UUID) {
        guard operation == ticket else { return }
        busy = false; task = nil; queryWork = false; outlineWork = false
    }
    private func fail(_ error: Error, ticket: UUID) {
        guard operation == ticket, !Task.isCancelled else { return }
        notice = (error as? AtlasReaderError)?.message ?? AtlasReaderError.unavailable.message
        finish(ticket)
    }
    private static func number(_ text: String) -> UInt64? {
        let value = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let number = UInt64(value), String(number) == value else { return nil }
        return number
    }
}
