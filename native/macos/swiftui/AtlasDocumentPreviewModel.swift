import Foundation
import Observation

/// User-facing Markdown navigation over the reader's existing immutable capture.
/// The injected operation is the retained reader coordinator, not a parser or a
/// new source provider. Source-only hosts do not prepare a document implicitly.
@Observable @MainActor final class AtlasDocumentPreviewModel {
    typealias Execute = @MainActor (AtlasReaderDocumentRequest) async throws -> AtlasReaderDocumentResult
    typealias Clipboard = @MainActor (String) -> Bool
    typealias SourceNavigation = @MainActor (AtlasReaderDocumentTarget) -> Void
    /// Only an enclosing host with an independently qualified project/capture
    /// route may install this callback. Destination labels never create grants.
    typealias LocalNavigation = @MainActor (AtlasReaderDocumentLinkTarget) -> Void

    private(set) var page: AtlasReaderDocumentPage?
    private(set) var headings: AtlasReaderDocumentHeadings?
    private(set) var links: AtlasReaderDocumentLinks?
    private(set) var source: AtlasReaderDocumentSource?
    private(set) var displayPage: AtlasDocumentDisplayPage?
    private(set) var displayFonts: [AtlasDocumentDisplayFont] = []
    private(set) var showsStyled = false
    private var prefersStyled = true
    private var displayHistory: [UInt64] = []
    private(set) var busy = false
    private(set) var authorized = false
    private(set) var notice = ""
    var widthInput = "96"
    var lineInput = "1"
    private var identity: AtlasReaderIdentity?
    private var sourceOffset: UInt64 = 0
    private var history = AtlasNavigationHistory<UInt64>()
    private var headingHistory: [UInt64] = []
    private var linkHistory: [UInt64] = []
    @ObservationIgnored private let supported: Bool
    let richSupported: Bool
    @ObservationIgnored private let execute: Execute
    @ObservationIgnored private let cancelWork: @MainActor () -> Void
    @ObservationIgnored private let revoke: @MainActor () -> Void
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var operation = UUID()

    init(supported: Bool, richSupported: Bool = false, execute: @escaping Execute,
         cancel: @escaping @MainActor () -> Void, revoke: @escaping @MainActor () -> Void) {
        self.supported = supported
        self.richSupported = richSupported
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
    var canBack: Bool { canNavigate && history.canBack }
    var canForward: Bool { canNavigate && history.canForward }
    var canNext: Bool { canNavigate && page?.next != nil }
    var canHeadingsBack: Bool { canNavigate && !headingHistory.isEmpty }
    var canHeadingsNext: Bool { canNavigate && headings?.next != nil }
    var canLinksBack: Bool { canNavigate && !linkHistory.isEmpty }
    var canLinksNext: Bool { canNavigate && links?.next != nil }
    var canDisplayBack: Bool { canNavigate && !displayHistory.isEmpty }
    var canDisplayNext: Bool { canNavigate && displayPage?.next != nil }
    var linkCoverage: String {
        guard let links else { return "Load the links parsed from this retained Markdown capture." }
        return "\(links.rows.count) of \(links.total) retained links shown; \(links.seen) parsed occurrences. "
            + (links.complete ? "Link inventory complete. " : "Link inventory incomplete. ")
            + (links.truncated ? "Some links were omitted by the admission limit. " : "")
            + (links.diagnostics > 0 ? "\(links.diagnostics) parser diagnostics. " : "")
            + "Evidence identifies the complete original document, not an inline span."
    }
    var coverage: String {
        guard let page else { return unavailableReason ?? "Prepare a logical FrankenMarkdown preview from the retained source." }
        let inventory = page.inventory
        let status = authorized ? "" : "Previous preview is read-only; prepare again before navigating or copying. "
        if showsStyled, let display = displayPage {
            return status + "FrankenMarkdown typography with bundled fonts. "
                + "\(display.items.count) of \(display.inventory.totalItems) display elements shown. "
                + (display.inventory.unresolvedImages > 0
                    ? "\(display.inventory.unresolvedImages) images use placeholders; no assets are fetched. " : "")
                + (display.inventory.qualifiedSources
                    ? "Source actions select enclosing Markdown blocks."
                    : "Source regions could not be qualified for this layout; use Text or the source reader.")
        }
        return status + "Logical FrankenMarkdown flow, not native shaped document layout. "
            + "\(page.lines.count) of \(inventory.totalLines) flow rows shown at \(inventory.width) columns. "
            + "Source links select enclosing Markdown regions, not glyph-exact spans. "
            + "Width changes return to the start of the first mapped Markdown region shown. No external links or assets are fetched."
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
        guard let identity else { return }
        let initial = page == nil
        let anchor: UInt64
        if showsStyled, let displayPage {
            guard displayPage.inventory.document.identity.matches(identity),
                  let original = displaySourceAnchor(displayPage) else {
                notice = "This styled page has no mapped source region to preserve. Switch to Text or move to a mapped page before changing width."
                return
            }
            anchor = original
        } else if let page {
            guard page.inventory.identity.matches(identity) else {
                notice = AtlasReaderError.invalidResponse.message
                return
            }
            if page.lines.isEmpty && page.first == page.inventory.totalLines {
                anchor = identity.capturedBytes
            } else if let region = page.lines.lazy.compactMap(\.enclosingOriginal).first(where: {
                $0.valid(upTo: identity.capturedBytes, nonempty: true) && $0.start >= page.inventory.sourceBase
            }) {
                // Generated rows have no anchor. The first mapped row restores
                // its enclosing block, not an invented glyph/intra-block offset.
                anchor = region.start
            } else {
                notice = "This preview page has no mapped source region to preserve. Use From source page or move to a mapped preview page before changing width."
                return
            }
        } else {
            anchor = sourceOffset
        }
        // Reflow may commit in Rust before cancellation reaches its response.
        // Keep old pixels readable but revoke their activation/copy immediately.
        authorized = false
        source = nil
        links = nil; linkHistory = []
        displayPage = nil; displayFonts = []; displayHistory = []; showsStyled = false
        revoke()
        perform(.prepare(width: width), notice: "Preparing retained Markdown…", resolve: { result, execute in
            guard case .prepared(let firstPage, let headings) = result,
                  firstPage.inventory.identity.matches(identity), firstPage.inventory.width == width,
                  firstPage.inventory.sourceBase <= identity.capturedBytes,
                  firstPage.first == 0, headings.first == 0,
                  firstPage.inventory.matches(headings.inventory) else { throw AtlasReaderError.invalidResponse }
            let offset = min(identity.capturedBytes, max(firstPage.inventory.sourceBase, anchor))
            // An initial request at the source start already has its page. In
            // particular, leading unmapped syntax need not have a flow anchor.
            if initial && offset == firstPage.inventory.sourceBase { return result }
            let restored = try await execute(.fromSource(offset: offset))
            guard case .page(let page) = restored, page.inventory.matches(firstPage.inventory) else {
                throw AtlasReaderError.invalidResponse
            }
            return .prepared(page, headings)
        }, then: { model in
            if model.richSupported && model.prefersStyled { model.prepareDisplay() }
        }) { model, result in
            guard case .prepared(let page, let headings) = result, let identity = model.identity,
                  page.inventory.identity.matches(identity), page.inventory.width == width,
                  page.inventory.matches(headings.inventory) else { throw AtlasReaderError.invalidResponse }
            // Publish only after anchor restoration succeeds. Until then the
            // accepted page, line field and navigation history remain intact.
            model.page = page
            model.headings = headings
            model.history.reset(to: page.first); model.headingHistory = []
            model.lineInput = String(page.first + 1)
            model.authorized = true
        }
    }

    /// Rich display has its own coordinate system and pagination. It never
    /// borrows logical row numbers or their rendered UTF-8 selection offsets.
    func selectStyled(_ styled: Bool) {
        prefersStyled = styled
        guard !busy else { return }
        if !styled {
            guard showsStyled, let displayPage else { showsStyled = false; return }
            if let anchor = displaySourceAnchor(displayPage) {
                navigate(.fromSource(offset: anchor))
            } else {
                showsStyled = false
                notice = "This styled page has no mapped source region. Text resumes at its previous location."
            }
            return
        }
        if let displayPage, page?.inventory.matches(displayPage.inventory.document) == true {
            showsStyled = true
        } else { prepareDisplay() }
    }
    private func displaySourceAnchor(_ display: AtlasDocumentDisplayPage) -> UInt64? {
        if display.first == display.inventory.totalItems {
            return display.inventory.document.identity.capturedBytes
        }
        guard display.inventory.qualifiedSources else { return nil }
        return display.items.lazy.compactMap(\.original).first?.start
    }
    private func retireDisplayLocation() {
        displayPage = nil; displayHistory = []; showsStyled = false
    }
    func prepareDisplay() {
        guard richSupported, canNavigate, let page else { return }
        let inventory = page.inventory
        let width = max(160, min(4096, inventory.width * 8))
        let anchor: UInt64?
        if page.first == 0 { anchor = nil }
        else if page.first == inventory.totalLines { anchor = inventory.identity.capturedBytes }
        else if let original = page.lines.lazy.compactMap(\.enclosingOriginal).first {
            anchor = original.start
        } else {
            notice = "This page has no source region to preserve in the styled preview. Text preview remains available."
            return
        }
        perform(.prepareDisplay(width: width), notice: "Preparing document typography…",
            resolve: { result, execute in
                guard case .display(let first, let fonts?) = result,
                      first.inventory.document.matches(inventory), first.first == 0 else {
                    throw AtlasReaderError.invalidResponse
                }
                guard let anchor else { return result }
                let restored = try await execute(.displayFromSource(offset: anchor))
                guard case .display(let page, nil) = restored, page.inventory.matches(first.inventory) else {
                    throw AtlasReaderError.invalidResponse
                }
                return .display(page, fonts: fonts)
            }) { model, result in
            guard case .display(let display, let fonts?) = result,
                  display.inventory.document.matches(inventory), display.inventory.width == width else {
                throw AtlasReaderError.invalidResponse
            }
            try display.validateFonts(fonts)
            model.displayPage = display; model.displayFonts = fonts; model.displayHistory = []
            model.showsStyled = model.prefersStyled
        }
    }
    func nextDisplay() {
        guard let next = displayPage?.next else { return }
        loadDisplay(first: next, goingBack: false)
    }
    func previousDisplay() {
        guard let first = displayHistory.last else { return }
        loadDisplay(first: first, goingBack: true)
    }
    private func loadDisplay(first: UInt64, goingBack: Bool) {
        guard canNavigate, let previous = displayPage else { return }
        perform(.displayPage(first: first), notice: "Reading document typography…") { model, result in
            guard case .display(let display, nil) = result,
                  display.inventory.matches(previous.inventory), display.first == first else {
                throw AtlasReaderError.invalidResponse
            }
            try display.validateFonts(model.displayFonts)
            if goingBack { _ = model.displayHistory.popLast() }
            else {
                model.displayHistory.append(previous.first)
                if model.displayHistory.count > 128 { model.displayHistory.removeFirst() }
            }
            model.displayPage = display
            model.showsStyled = model.prefersStyled
        }
    }
    func allowsDisplayItem(_ item: AtlasDocumentDisplayItem, pageID: UUID) -> Bool {
        canNavigate && displayPage?.id == pageID && item.original != nil
            && displayPage?.items.contains(where: { $0.id == item.id && $0.original == item.original }) == true
    }
    func showDisplaySource(_ item: AtlasDocumentDisplayItem, pageID: UUID,
                           navigate: SourceNavigation? = nil) {
        guard allowsDisplayItem(item, pageID: pageID), let original = item.original,
              let previous = page else { return }
        let move = history.visit(previous.first)
        perform(.fromSource(offset: original.start), notice: "Locating the captured Markdown block…",
            then: { model in
                guard let page = model.page, let target = page.target(at: page.first),
                      target.line.enclosingOriginal == original else {
                    model.notice = "This display element has no matching logical source region. The source reader remains available."
                    return
                }
                model.showSource(target, navigate: navigate)
            }) { model, result in
                guard case .page(let page) = result, page.inventory.matches(previous.inventory),
                      model.history.commit(move, resolved: page.first) else { throw AtlasReaderError.invalidResponse }
                model.page = page; model.lineInput = String(page.first + 1)
                model.source = nil
            }
    }

    func next() { if let next = page?.next { navigate(first: next) } }
    func back() {
        guard canBack, let move = history.back() else { return }
        navigate(.window(first: move.target), historyMove: move)
    }
    func forward() {
        guard canForward, let move = history.forward() else { return }
        navigate(.window(first: move.target), historyMove: move)
    }
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

    func prepareLinks() {
        guard canNavigate, let inventory = page?.inventory else { return }
        links = nil; linkHistory = []
        perform(.prepareLinks, notice: "Reading links from retained Markdown…") { model, result in
            guard case .links(let links) = result, links.inventory.matches(inventory), links.first == 0 else {
                throw AtlasReaderError.invalidResponse
            }
            model.links = links
        }
    }
    func nextLinks() { if let next = links?.next { loadLinks(first: next) } }
    func previousLinks() { if let first = linkHistory.last { loadLinks(first: first, goingBack: true) } }
    func allows(_ target: AtlasReaderDocumentLinkTarget) -> Bool {
        canNavigate && links?.target(id: target.link.id) == target
    }
    func openLink(_ target: AtlasReaderDocumentLinkTarget, openLocal: LocalNavigation? = nil) {
        guard allows(target), let previous = page else { return }
        let move = history.visit(previous.first)
        perform(.link(target), notice: "Resolving retained document link…", then: { model in
            // Recheck after the worker has drained and busy state is released.
            // The host callback may synchronously close or replace this reader.
            guard target.link.kind == .local, model.allows(target) else { return }
            openLocal?(target)
        }) { model, result in
            guard case .link(let activation) = result, activation.target == target,
                  model.links?.target(id: target.link.id) == target else {
                throw AtlasReaderError.invalidResponse
            }
            if let page = activation.page {
                guard target.link.kind.navigatesWithinCapture, page.inventory.matches(previous.inventory),
                      model.history.commit(move, resolved: page.first) else { throw AtlasReaderError.invalidResponse }
                model.page = page; model.source = nil
                model.lineInput = String(page.first + 1)
                model.retireDisplayLocation()
            } else {
                guard !target.link.kind.navigatesWithinCapture else { throw AtlasReaderError.invalidResponse }
                switch target.link.kind {
                case .local:
                    model.notice = openLocal == nil
                        ? "This local link needs a current project capture route. No file was opened."
                        : "The current project will resolve this retained link against its admitted files."
                case .external: model.notice = "External destination. No browser or network request was opened."
                case .unsupported: model.notice = "This destination is unsupported by the shared document resolver."
                case .heading, .start: throw AtlasReaderError.invalidResponse
                }
            }
        }
    }

    func showSource(_ target: AtlasReaderDocumentTarget, navigate: SourceNavigation? = nil) {
        guard allows(target) else { return }
        // Deliver only after perform has committed the response and released its
        // busy/task state. The host may synchronously start source navigation,
        // cancel this preview, close its sheet or replace the whole reader.
        perform(.source(target), notice: "Locating enclosing captured Markdown…", then: { model in
            guard let identity = model.identity,
                  model.sourceOffset(for: target, reader: identity) != nil else { return }
            navigate?(target)
        }) { model, result in
            guard case .source(let source) = result, source.target == target,
                  model.page?.target(at: target.line.id) == target,
                  source.enclosingOriginal == target.line.enclosingOriginal,
                  source.enclosingOriginal.valid(upTo: target.inventory.identity.capturedBytes, nonempty: true) else {
                throw AtlasReaderError.invalidResponse
            }
            model.source = source
        }
    }

    /// Resolve a CURRENT mapped row for the CURRENT retained reader, never a
    /// filename reopen or an offset into rendered text. A context response alone
    /// is insufficient after reflow/page replacement. The result is an enclosing
    /// region's original start, not an invented glyph-exact selection.
    func sourceOffset(for target: AtlasReaderDocumentTarget, reader: AtlasReaderIdentity) -> UInt64? {
        guard allows(target), identity?.matches(reader) == true,
              target.inventory.identity.matches(reader),
              let source, source.target == target,
              let region = target.line.enclosingOriginal,
              source.enclosingOriginal == region,
              region.valid(upTo: reader.capturedBytes, nonempty: true) else { return nil }
        return region.start
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
        identity = nil; page = nil; headings = nil; links = nil; source = nil
        displayPage = nil; displayFonts = []; displayHistory = []; showsStyled = false
        authorized = false; history.reset(); headingHistory = []; linkHistory = []
        sourceOffset = 0; lineInput = "1"; notice = ""
    }

    private func navigate(first: UInt64) {
        guard let page, first < page.inventory.totalLines else { return }
        navigate(.window(first: first))
    }
    private func navigate(_ request: AtlasReaderDocumentRequest,
                          historyMove: AtlasNavigationHistory<UInt64>.Move? = nil) {
        guard canNavigate, let previous = page else { return }
        // Heading/source requests resolve their flow row in the shared engine;
        // only that accepted row, never the transient target, enters history.
        let move = historyMove ?? history.visit(previous.first)
        perform(request, notice: "Reading retained Markdown…") { model, result in
            guard case .page(let page) = result, page.inventory.matches(previous.inventory),
                  model.history.commit(move, resolved: page.first) else {
                throw AtlasReaderError.invalidResponse
            }
            model.page = page
            model.source = nil
            model.lineInput = String(page.first + 1)
            model.retireDisplayLocation()
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
    private func loadLinks(first: UInt64, goingBack: Bool = false) {
        guard canNavigate, let previous = links, first < previous.total else { return }
        perform(.links(first: first), notice: "Reading retained link occurrences…") { model, result in
            guard case .links(let links) = result, links.matchesInventory(previous), links.first == first else {
                throw AtlasReaderError.invalidResponse
            }
            if goingBack { _ = model.linkHistory.popLast() }
            else if first != previous.first {
                model.linkHistory.append(previous.first)
                if model.linkHistory.count > 128 { model.linkHistory.removeFirst() }
            }
            model.links = links
        }
    }
    private func perform(_ request: AtlasReaderDocumentRequest, notice: String,
                         resolve: (@MainActor (AtlasReaderDocumentResult, Execute) async throws -> AtlasReaderDocumentResult)? = nil,
                         then: (@MainActor (AtlasDocumentPreviewModel) -> Void)? = nil,
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
                var result = try await execute(request)
                if let resolve {
                    // Do not retain the model across another worker await. A
                    // dismissed/replaced preview cannot launch a follow-up.
                    guard self?.operation == ticket, !Task.isCancelled,
                          self?.identity?.matches(identity) == true else { return }
                    result = try await resolve(result, execute)
                }
                guard let self, self.operation == ticket, !Task.isCancelled,
                      self.identity?.matches(identity) == true else { return }
                self.notice = ""
                try accept(self, result)
                if self.operation == ticket {
                    self.busy = false; self.task = nil
                    then?(self)
                    // No mutation after calling the reentrant host callback.
                }
            } catch {
                guard let self, self.operation == ticket, !Task.isCancelled else { return }
                self.notice = (error as? AtlasReaderError)?.message ?? AtlasReaderError.unavailable.message
                if case .prepareDisplay = request {
                    self.showsStyled = false
                    if let readerError = error as? AtlasReaderError, case .engine(let code) = readerError,
                       code == "DOCUMENT_DISPLAY_SHAPING_UNSUPPORTED" {
                        self.notice = "Bundled typography does not support all the text in this document. Text preview and the exact source remain available."
                    } else {
                        self.notice += " Text preview remains available."
                    }
                }
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
