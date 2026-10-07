import Foundation
import Observation

/// User-facing Markdown navigation over the reader's existing immutable capture.
/// The injected operation is the retained reader coordinator, not a parser or a
/// new source provider. Source-only hosts do not prepare a document implicitly.
@Observable @MainActor final class AtlasDocumentPreviewModel {
    typealias Execute = @MainActor (AtlasReaderDocumentRequest) async throws -> AtlasReaderDocumentResult
    typealias Clipboard = @MainActor (String) -> Bool

    private(set) var page: AtlasReaderDocumentPage?
    private(set) var headings: AtlasReaderDocumentHeadings?
    private(set) var source: AtlasReaderDocumentSource?
    private(set) var busy = false
    private(set) var authorized = false
    private(set) var notice = ""
    var widthInput = "96"
    var lineInput = "1"
    private var identity: AtlasReaderIdentity?
    private var sourceOffset: UInt64 = 0
    private var history: [UInt64] = []
    private var headingHistory: [UInt64] = []
    @ObservationIgnored private let supported: Bool
    @ObservationIgnored private let execute: Execute
    @ObservationIgnored private let cancelWork: @MainActor () -> Void
    @ObservationIgnored private let revoke: @MainActor () -> Void
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var operation = UUID()

    init(supported: Bool, execute: @escaping Execute,
         cancel: @escaping @MainActor () -> Void, revoke: @escaping @MainActor () -> Void) {
        self.supported = supported
        self.execute = execute
        cancelWork = cancel
        self.revoke = revoke
    }
    deinit { task?.cancel() }

    var unavailableReason: String? {
        guard supported else { return "This host did not enable Markdown preview." }
        guard let identity else { return "Open a retained source capture first." }
        guard identity.encoding == "utf8" else { return "Markdown preview requires UTF-8; the source reader remains available." }
        guard identity.capturedBytes <= AtlasReaderDocumentLimits.sourceBytes else {
            return "Markdown preview admits at most 64 KiB of source. The larger retained file remains readable."
        }
        return nil
    }
    var canPrepare: Bool { unavailableReason == nil }
    var canNavigate: Bool { authorized && page != nil && !busy }
    var canBack: Bool { canNavigate && !history.isEmpty }
    var canNext: Bool { canNavigate && page?.next != nil }
    var canHeadingsBack: Bool { canNavigate && !headingHistory.isEmpty }
    var canHeadingsNext: Bool { canNavigate && headings?.next != nil }
    var coverage: String {
        guard let page else { return unavailableReason ?? "Prepare a logical FrankenMarkdown preview from the retained source." }
        let inventory = page.inventory
        let status = authorized ? "" : "Previous preview is read-only; prepare again before navigating or copying. "
        return status + "Logical FrankenMarkdown flow, not native shaped document layout. "
            + "\(page.lines.count) of \(inventory.totalLines) flow rows shown at \(inventory.width) columns. "
            + "Source links select enclosing Markdown regions, not glyph-exact spans. No external links or assets are fetched."
    }

    /// Same-capture source paging changes only the synchronization anchor.
    /// Opening another file/capture revokes every old page and heading target.
    func bind(identity next: AtlasReaderIdentity, sourceOffset: UInt64) {
        if identity?.matches(next) != true {
            close()
            identity = next
        }
        self.sourceOffset = min(sourceOffset, next.capturedBytes)
    }

    func prepare() {
        guard canPrepare else { notice = unavailableReason ?? "Markdown preview unavailable."; return }
        guard let width = Self.number(widthInput), (4...512).contains(width) else {
            notice = "Choose a preview width from 4 to 512 columns."
            return
        }
        // Reflow may commit in Rust before cancellation reaches its response.
        // Keep old pixels readable but revoke their activation/copy immediately.
        authorized = false
        source = nil
        revoke()
        perform(.prepare(width: width), notice: "Preparing retained Markdown…") { model, result in
            guard case .prepared(let page, let headings) = result, let identity = model.identity,
                  page.inventory.identity.matches(identity), page.inventory.width == width,
                  page.inventory.matches(headings.inventory) else { throw AtlasReaderError.invalidResponse }
            model.page = page
            model.headings = headings
            model.history = []; model.headingHistory = []
            model.lineInput = String(page.first + 1)
            model.authorized = true
        }
    }

    func next() { if let next = page?.next { navigate(first: next) } }
    func back() { if let first = history.last { navigate(first: first, goingBack: true) } }
    func goToLine() {
        guard let line = Self.number(lineInput), line > 0, let page,
              line - 1 < page.inventory.totalLines else {
            notice = "Choose a one-based rendered flow row within this document."
            return
        }
        navigate(first: line - 1)
    }
    func fromSource() {
        guard canNavigate, let inventory = page?.inventory else { return }
        // A UTF-8 BOM precedes the parser's source domain.
        let offset = max(inventory.sourceBase, sourceOffset)
        navigate(.fromSource(offset: offset))
    }
    func openHeading(_ target: AtlasReaderDocumentHeadingTarget) {
        guard canNavigate, headings?.target(slug: target.heading.slug) == target else { return }
        navigate(.heading(target))
    }
    func nextHeadings() { if let next = headings?.next { loadHeadings(first: next) } }
    func previousHeadings() { if let first = headingHistory.last { loadHeadings(first: first, goingBack: true) } }

    func showSource(_ target: AtlasReaderDocumentTarget) {
        guard allows(target) else { return }
        perform(.source(target), notice: "Locating enclosing captured Markdown…") { model, result in
            guard case .source(let source) = result, source.target == target,
                  model.page?.target(at: target.line.id) == target else { throw AtlasReaderError.invalidResponse }
            model.source = source
        }
    }

    /// Clipboard access occurs only in the explicit UI action's synchronous
    /// callback, after response and current-page validation. Stale/canceled work
    /// cannot write later. Original Markdown is exposed as lossless hex, not as
    /// an invented decoded selection or the complete current disk file.
    func copy(_ target: AtlasReaderDocumentTarget, mode: AtlasReaderDocumentCopyMode,
              deliver: @escaping Clipboard) {
        guard allows(target) else { return }
        perform(.copy(target, mode), notice: "Preparing verified copy…") { model, result in
            guard case .copy(let copy) = result, copy.target == target, copy.mode == mode,
                  model.page?.target(at: target.line.id) == target else { throw AtlasReaderError.invalidResponse }
            let ticket = model.operation
            let success = deliver(copy.value)
            // A host clipboard adapter may synchronously close/replace a view.
            guard model.operation == ticket else { return }
            model.notice = success
                ? (mode == .renderedText ? "Copied rendered row text." : "Copied enclosing original Markdown bytes as hex.")
                : "Clipboard write failed."
        }
    }
    func allows(_ target: AtlasReaderDocumentTarget) -> Bool {
        canNavigate && page?.target(at: target.line.id) == target
    }

    /// Source navigation shares the same worker; interrupt only unfinished
    /// preview work, retaining accepted preview/heading authority for this file.
    func cancel(silent: Bool = false) {
        operation = UUID()
        if task != nil { task?.cancel(); cancelWork() }
        task = nil
        busy = false
        if !silent { notice = "Preview operation canceled. Accepted source and search state are unchanged." }
    }
    func close() {
        cancel(silent: true)
        revoke()
        identity = nil; page = nil; headings = nil; source = nil
        authorized = false; history = []; headingHistory = []
        sourceOffset = 0; lineInput = "1"; notice = ""
    }

    private func navigate(first: UInt64, goingBack: Bool = false) {
        guard let page, first < page.inventory.totalLines else { return }
        navigate(.window(first: first), goingBack: goingBack)
    }
    private func navigate(_ request: AtlasReaderDocumentRequest, goingBack: Bool = false) {
        guard canNavigate, let previous = page else { return }
        perform(request, notice: "Reading retained Markdown…") { model, result in
            guard case .page(let page) = result, page.inventory.matches(previous.inventory) else {
                throw AtlasReaderError.invalidResponse
            }
            if goingBack { _ = model.history.popLast() }
            else if page.first != previous.first {
                model.history.append(previous.first)
                if model.history.count > 128 { model.history.removeFirst() }
            }
            model.page = page
            model.source = nil
            model.lineInput = String(page.first + 1)
        }
    }
    private func loadHeadings(first: UInt64, goingBack: Bool = false) {
        guard canNavigate, let previous = headings, first < previous.inventory.totalHeadings else { return }
        perform(.headings(first: first), notice: "Reading retained headings…") { model, result in
            guard case .headings(let headings) = result, headings.inventory.matches(previous.inventory) else {
                throw AtlasReaderError.invalidResponse
            }
            if goingBack { _ = model.headingHistory.popLast() }
            else if first != previous.first {
                model.headingHistory.append(previous.first)
                if model.headingHistory.count > 128 { model.headingHistory.removeFirst() }
            }
            model.headings = headings
        }
    }
    private func perform(_ request: AtlasReaderDocumentRequest, notice: String,
                         accept: @escaping @MainActor (AtlasDocumentPreviewModel, AtlasReaderDocumentResult) throws -> Void) {
        guard let identity else { return }
        cancel(silent: true)
        let ticket = UUID()
        operation = ticket
        busy = true
        self.notice = notice
        let execute = self.execute
        task = Task { [weak self] in
            do {
                let result = try await execute(request)
                guard let self, self.operation == ticket, !Task.isCancelled,
                      self.identity?.matches(identity) == true else { return }
                self.notice = ""
                try accept(self, result)
                if self.operation == ticket { self.busy = false; self.task = nil }
            } catch {
                guard let self, self.operation == ticket, !Task.isCancelled else { return }
                self.notice = (error as? AtlasReaderError)?.message ?? AtlasReaderError.unavailable.message
                self.busy = false; self.task = nil
            }
        }
    }
    private static func number(_ text: String) -> UInt64? {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let value = UInt64(text), String(value) == text else { return nil }
        return value
    }
}
