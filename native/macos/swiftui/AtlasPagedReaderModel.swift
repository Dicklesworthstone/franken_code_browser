import Foundation
import Observation

/// Presentation state for one independently opened retained capture. It never
/// accepts workspace hit coordinates or promotes a page into atlas capture
/// evidence. Only committed navigation changes the current page/history.
@Observable @MainActor final class AtlasPagedReaderModel {
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
    private var history: [AtlasReaderRequest] = []
    private var currentRequest: AtlasReaderRequest?
    @ObservationIgnored private let coordinator: AtlasReaderCoordinator
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var operation = UUID()

    init(transport: AtlasReaderTransport, search: AtlasReaderSearchTransport? = nil) {
        coordinator = AtlasReaderCoordinator(transport: transport, search: search)
    }
    deinit { task?.cancel() }

    var canGoBack: Bool { !history.isEmpty && !busy }
    var canGoNext: Bool { page?.nextOffset != nil && !busy }
    var canFind: Bool { page != nil && coordinator.supportsFind }
    var findIsCurrent: Bool { findReport?.needle.utf8.elementsEqual(fileQuery.utf8) == true }
    var canMoveHit: Bool { findIsCurrent && findReport?.hits.isEmpty == false && !busy }
    var nativeSelectionRange: NSRange? {
        guard findIsCurrent, page?.selection?.target.generation == findReport?.generation else { return nil }
        return selectedRange
    }
    var matchHex: String? {
        guard findIsCurrent, let selection = page?.selection,
              selection.target.generation == findReport?.generation else { return nil }
        return selection.target.originalHex
    }

    var coverage: String {
        guard let page else { return "Retained capture limit: \(AtlasReaderLimits.captureBytes / 1024 / 1024) MiB. Oversized sources are refused, not truncated." }
        var messages = ["Logical page; layout is local to this window."]
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
        findReport = nil; selectedHitIndex = nil; selectedRange = nil
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

    /// Explicit cancellation keeps a successfully installed page. Cancellation
    /// during initial capture instead releases its handle after work drains.
    func cancel() {
        operation = UUID()
        task?.cancel(); task = nil
        coordinator.cancel()
        if page == nil { coordinator.close() }
        busy = false
        queryWork = false
        notice = page == nil ? "Source opening canceled." : "Navigation canceled. The current retained page is unchanged."
    }
    func close() {
        operation = UUID()
        task?.cancel(); task = nil
        coordinator.close()
        page = nil; source = nil; history = []; currentRequest = nil
        findReport = nil; selectedHitIndex = nil; selectedRange = nil
        fileQuery = ""; queryWork = false
        busy = false; notice = ""
        offsetInput = "0"; lineInput = "1"
        navigation = UUID()
    }

    private func navigate(_ request: AtlasReaderRequest, goingBack: Bool = false, hitIndex: Int? = nil) {
        guard let previous = page, let previousRequest = currentRequest else { return }
        do { try request.validate(capturedBytes: previous.identity.capturedBytes) }
        catch { notice = AtlasReaderError.invalidInput.message; return }
        let ticket = begin(query: hitIndex != nil)
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
    private func begin(query: Bool = false) -> UUID {
        task?.cancel()
        coordinator.cancel()
        operation = UUID()
        busy = true
        queryWork = query
        return operation
    }
    private func install(_ page: AtlasReaderPage, request: AtlasReaderRequest) {
        // History restores the source page, never a now-replaced query's hit.
        // An explicit Back does not silently regain old exact-hit authority.
        if case .hit = request {
            currentRequest = .window(offset: page.start, bytes: max(4, min(AtlasReaderLimits.maximumPageBytes, page.end - page.start)))
        } else { currentRequest = request }
        self.page = page
        let source = AtlasSource(path: page.identity.displayPath, text: page.text)
        self.source = source
        selectedRange = page.selection.flatMap { source.utf16Range(byteStart: $0.utf8Start, byteEnd: $0.utf8End) }
        navigation = UUID()
        offsetInput = String(page.start)
        if let line = page.firstPhysicalLine { lineInput = String(line) }
        notice = ""
    }
    private func finish(_ ticket: UUID) {
        guard operation == ticket else { return }
        busy = false; task = nil; queryWork = false
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
