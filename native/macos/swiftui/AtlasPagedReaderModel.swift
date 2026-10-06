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
    private var history: [AtlasReaderRequest] = []
    private var currentRequest: AtlasReaderRequest?
    @ObservationIgnored private let coordinator: AtlasReaderCoordinator
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var operation = UUID()

    init(transport: AtlasReaderTransport) { coordinator = AtlasReaderCoordinator(transport: transport) }
    deinit { task?.cancel() }

    var canGoBack: Bool { !history.isEmpty && !busy }
    var canGoNext: Bool { page?.nextOffset != nil && !busy }
    var coverage: String {
        guard let page else { return "Retained capture limit: \(AtlasReaderLimits.captureBytes / 1024 / 1024) MiB. Oversized sources are refused, not truncated." }
        var messages = ["Logical page; layout is local to this window, not an exact search location."]
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

    /// Explicit cancellation keeps a successfully installed page. Cancellation
    /// during initial capture instead releases its handle after work drains.
    func cancel() {
        operation = UUID()
        task?.cancel(); task = nil
        coordinator.cancel()
        if page == nil { coordinator.close() }
        busy = false
        notice = page == nil ? "Source opening canceled." : "Navigation canceled. The current retained page is unchanged."
    }
    func close() {
        operation = UUID()
        task?.cancel(); task = nil
        coordinator.close()
        page = nil; source = nil; history = []; currentRequest = nil
        busy = false; notice = ""
        offsetInput = "0"; lineInput = "1"
        navigation = UUID()
    }

    private func navigate(_ request: AtlasReaderRequest, goingBack: Bool = false) {
        guard let previous = page, let previousRequest = currentRequest else { return }
        do { try request.validate(capturedBytes: previous.identity.capturedBytes) }
        catch { notice = AtlasReaderError.invalidInput.message; return }
        let ticket = begin()
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
                finish(ticket)
            } catch { fail(error, ticket: ticket) }
        }
    }
    private func begin() -> UUID {
        task?.cancel()
        coordinator.cancel()
        operation = UUID()
        busy = true
        return operation
    }
    private func install(_ page: AtlasReaderPage, request: AtlasReaderRequest) {
        currentRequest = request
        self.page = page
        source = AtlasSource(path: page.identity.displayPath, text: page.text)
        navigation = UUID()
        offsetInput = String(page.start)
        if let line = page.firstPhysicalLine { lineInput = String(line) }
        notice = ""
    }
    private func finish(_ ticket: UUID) {
        guard operation == ticket else { return }
        busy = false; task = nil
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
