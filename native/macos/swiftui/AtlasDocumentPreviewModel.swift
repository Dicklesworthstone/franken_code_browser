import Foundation
import Observation

/// A web destination admitted for an explicit OS handoff. Parsing this value is
/// inert. Keep percent escapes in the URL passed to the browser; decode once
/// only to reject hidden controls, bidi formatting and backslash ambiguity.
/// Native paths, custom schemes and credentials never enter this route.
struct AtlasExternalWebLink: Sendable {
    let url: URL
    var display: String { url.absoluteString }

    init?(_ destination: String) {
        guard !destination.isEmpty, destination.utf8.count <= AtlasReaderDocumentLimits.destinationBytes,
              !destination.unicodeScalars.contains(where: {
                  CharacterSet.whitespacesAndNewlines.contains($0) || Self.forbidden($0)
              }), !destination.contains("\\"),
              let decoded = destination.removingPercentEncoding,
              !decoded.unicodeScalars.contains(where: Self.forbidden), !decoded.contains("\\"),
              let components = URLComponents(string: destination),
              let scheme = components.scheme?.lowercased(), ["http", "https"].contains(scheme),
              destination.lowercased().hasPrefix(scheme + "://"),
              components.user == nil, components.password == nil,
              let host = components.host, !host.isEmpty,
              components.port.map({ (1...65535).contains($0) }) ?? true else { return nil }

        // Do not reinterpret percent-encoded or Unicode authority delimiters.
        // ASCII DNS names, punycode names and bracketed IPv6 remain supported;
        // Unicode is allowed in the path, query and fragment.
        let authority = destination.dropFirst(scheme.count + 3).prefix { !"/?#".contains($0) }
        guard !authority.isEmpty, authority.last != ":",
              authority.utf8.allSatisfy({ byte in
                  (48...57).contains(byte) || (65...90).contains(byte) || (97...122).contains(byte)
                      || [45, 46, 58, 91, 93, 95].contains(byte)
              }), let url = components.url, !url.isFileURL,
              url.scheme?.lowercased() == scheme, url.host != nil else { return nil }
        self.url = url
    }

    private static func forbidden(_ scalar: Unicode.Scalar) -> Bool {
        switch scalar.value {
        case 0...0x1f, 0x7f...0x9f, 0x061c, 0x200e...0x200f,
             0x2028...0x202e, 0x2066...0x2069, 0xfeff: true
        default: false
        }
    }
}

/// One window's optional Markdown default. A visit and an explicit preference
/// change each get a distinct request; repainting or rebuilding a reader does
/// not. Raw path bytes choose only presentation, never a source-access grant.
struct AtlasMarkdownPreviewPreference {
    private(set) var enabled = false
    private var visit: UUID?
    private var generation = UUID()

    mutating func toggle() {
        enabled.toggle()
        generation = UUID()
    }
    mutating func showSource() {
        guard enabled else { return }
        enabled = false
        generation = UUID()
    }
    mutating func activate(_ next: UUID?) {
        guard visit != next else { return }
        visit = next
        generation = UUID()
    }
    func request(for visit: UUID, path: [UInt8], active: Bool) -> UUID? {
        guard enabled, active, self.visit == visit, Self.isMarkdown(path) else { return nil }
        return generation
    }
    static func isMarkdown(_ path: [UInt8]) -> Bool {
        guard !path.isEmpty, path.count <= 16_384, let dot = path.lastIndex(of: 46),
              path.lastIndex(of: 47).map({ $0 < dot }) ?? true else { return false }
        let suffix = path.suffix(from: dot + 1)
        guard (2...8).contains(suffix.count) else { return false }
        let lower = suffix.map { (65...90).contains($0) ? $0 + 32 : $0 }
        return lower.elementsEqual("md".utf8) || lower.elementsEqual("markdown".utf8)
            || lower.elementsEqual("mdown".utf8)
    }
}

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
    /// Called only after an explicit web-link action and retained-target
    /// verification. The native host supplies its configured OS browser route.
    typealias ExternalNavigation = @MainActor (AtlasExternalWebLink) -> Bool

    private(set) var page: AtlasReaderDocumentPage?
    private(set) var headings: AtlasReaderDocumentHeadings?
    private(set) var links: AtlasReaderDocumentLinks?
    private(set) var findResults: AtlasReaderDocumentMatches?
    private(set) var selectedFindID: UInt64?
    var findInput = "" {
        didSet {
            guard !oldValue.utf8.elementsEqual(findInput.utf8) else { return }
            // Revoke synchronously with the edit, before a delayed button or
            // worker completion can apply the previous query's match token.
            findResults = nil; selectedFindID = nil
            if finding { cancel(silent: true); notice = "" }
        }
    }
    private(set) var source: AtlasReaderDocumentSource?
    private(set) var displayPage: AtlasDocumentDisplayPage?
    private(set) var displayFonts: [AtlasDocumentDisplayFont] = []
    private(set) var showsStyled = false
    private(set) var followsSource = false
    private var prefersStyled = true
    private var displayHistory: [UInt64] = []
    private(set) var busy = false
    private(set) var authorized = false
    private(set) var notice = ""
    var widthInput = "96"
    var lineInput = "1"
    private var identity: AtlasReaderIdentity?
    private var sourceOffset: UInt64 = 0
    private var pendingSourceFollow: UInt64?
    private var sourceNavigationEcho: UInt64?
    private var followingSource = false
    private var canRestoreNavigation = false
    private var history = AtlasNavigationHistory<UInt64>()
    private var headingHistory: [UInt64] = []
    private var linkHistory: [UInt64] = []
    @ObservationIgnored private let supported: Bool
    let richSupported: Bool
    let findSupported: Bool
    private var finding = false
    @ObservationIgnored private let execute: Execute
    @ObservationIgnored private let cancelWork: @MainActor () -> Void
    @ObservationIgnored private let revoke: @MainActor () -> Void
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var operation = UUID()
    @ObservationIgnored private var preferredPresentation: UUID?
    /// The source owner arbitrates the shared worker. A preview completion may
    /// wake that owner, but cannot decide that source/selection work is idle.
    @ObservationIgnored var didBecomeIdle: (@MainActor () -> Void)?

    init(supported: Bool, richSupported: Bool = false, findSupported: Bool = false, execute: @escaping Execute,
         cancel: @escaping @MainActor () -> Void, revoke: @escaping @MainActor () -> Void) {
        self.supported = supported
        self.richSupported = richSupported
        self.findSupported = findSupported
        self.execute = execute
        cancelWork = cancel
        self.revoke = revoke
    }
    deinit { task?.cancel() }

    var unavailableReason: String? {
        guard supported else { return "This host did not enable Markdown preview." }
        guard let identity else { return "Open a retained source capture first." }
        guard ["utf8", "utf16le", "utf16be"].contains(identity.encoding) else {
            return "Markdown preview supports UTF-8 and BOM-marked UTF-16. The source reader remains available for other encodings."
        }
        guard identity.capturedBytes <= AtlasReaderDocumentLimits.sourceBytes else {
            return "Markdown preview admits at most 256 KiB of source. The larger retained file remains readable."
        }
        return nil
    }
    private var styledSourceLimitReason: String? {
        guard richSupported, let identity,
              identity.capturedBytes > AtlasReaderDocumentLimits.styledSourceBytes else { return nil }
        return "This document uses Text preview because Styled preview is limited to 128 KiB of original source and decoded UTF-8. Headings, links, and source navigation remain available."
    }
    var canPrepare: Bool { unavailableReason == nil }
    var canNavigate: Bool { authorized && page != nil && !busy }
    /// A canceled navigation may have replaced the worker's logical
    /// page token. Source anchoring needs only the retained inventory, so it
    /// can recover without reflow while old row/copy actions remain read-only.
    var canLocateSource: Bool { (authorized || canRestoreNavigation) && page != nil && !busy }
    var canBack: Bool { canNavigate && history.canBack }
    var canForward: Bool { canNavigate && history.canForward }
    var canNext: Bool { canNavigate && page?.next != nil }
    var canHeadingsBack: Bool { canNavigate && !headingHistory.isEmpty }
    var canHeadingsNext: Bool { canNavigate && headings?.next != nil }
    var canLinksBack: Bool { canNavigate && !linkHistory.isEmpty }
    var canLinksNext: Bool { canNavigate && links?.next != nil }
    var canDisplayBack: Bool { canNavigate && !displayHistory.isEmpty }
    var canDisplayNext: Bool { canNavigate && displayPage?.next != nil }
    var canFind: Bool {
        findSupported && canLocateSource && AtlasReaderDocumentLimits.validFindNeedle(findInput)
            && (page?.inventory.renderedBytes ?? UInt64.max) <= AtlasReaderDocumentLimits.findScanBytes
    }
    var canPreviousFind: Bool {
        canNavigate && currentFindResults != nil && (selectedFindID ?? 0) > 1
    }
    var canNextFind: Bool {
        guard canNavigate, let results = currentFindResults, let selectedFindID else { return false }
        return selectedFindID < results.total
    }
    private var currentFindResults: AtlasReaderDocumentMatches? {
        guard let results = findResults, let page, results.inventory.matches(page.inventory),
              results.needle.utf8.elementsEqual(findInput.utf8) else { return nil }
        return results
    }
    var findStatus: String {
        if let results = currentFindResults {
            if results.total == 0 { return "No matches in the rendered document." }
            if let selectedFindID, let target = results.target(id: selectedFindID) {
                let context = results.inventory.totalLines == 0
                    ? "rendered text has no displayed flow row"
                    : "flow row \(target.match.firstLine + 1)"
                return "Match \(selectedFindID) of \(results.total) · \(context)"
            }
            return "\(results.total) matches in the rendered document."
        }
        if let page, page.inventory.renderedBytes > AtlasReaderDocumentLimits.findScanBytes {
            return "Rendered Find admits up to 16 MiB. This preview and its source remain readable."
        }
        return "Exact, case-sensitive Find in rendered text, including text joined by Markdown formatting."
    }

    var linkCoverage: String {
        guard let links else { return "Load the links parsed from this retained Markdown capture." }
        return "\(links.rows.count) of \(links.total) retained links shown; \(links.seen) parsed occurrences. "
            + (links.complete ? "Link inventory complete. " : "Link inventory incomplete. ")
            + (links.truncated ? "Some links were omitted by the admission limit. " : "")
            + (links.diagnostics > 0 ? "\(links.diagnostics) parser diagnostics. " : "")
            + "Evidence identifies the complete original document, not an inline span."
    }
    var coverage: String {
        guard let page else {
            return unavailableReason ?? styledSourceLimitReason
                ?? "Prepare a logical FrankenMarkdown preview from the retained source."
        }
        let inventory = page.inventory
        let status = authorized ? "" : (canRestoreNavigation
            ? "Previous preview is read-only; use From source page to retry navigation, or prepare again. "
            : "Previous preview is read-only; prepare again before navigating or copying. ")
        if showsStyled, let display = displayPage {
            let imageSlots = display.items.filter { if case .image = $0.content { return true }; return false }.count
            let imageCoverage = imageSlots == 0 ? "" :
                "\(display.images.count) local images loaded, \(imageSlots - display.images.count) placeholders on this page. "
            return status + "FrankenMarkdown typography with bundled fonts. "
                + "\(display.items.count) of \(display.inventory.totalItems) display elements shown. "
                + imageCoverage + (display.imageNotice.map { $0 + " " } ?? "")
                + (display.inventory.qualifiedSources
                    ? "Source actions select enclosing Markdown blocks."
                    : "Source regions could not be qualified for this layout; use Text or the source reader.")
        }
        return status + (styledSourceLimitReason.map { $0 + " " } ?? "")
            + "Logical FrankenMarkdown flow, not native shaped document layout. "
            + "\(page.lines.count) of \(inventory.totalLines) flow rows shown at \(inventory.width) columns. "
            + "Source links select enclosing Markdown regions, not glyph-exact spans. "
            + "Width changes return to the start of the first mapped Markdown region shown. Web links open only through Open in browser. Styled mode can load local images from the current project or captured assets in the opened saved archive."
    }

    /// Same-capture source paging changes only the synchronization anchor.
    /// Opening another file/capture revokes every old page and heading target.
    func bind(identity next: AtlasReaderIdentity, sourceOffset: UInt64) {
        let sameCapture = identity?.matches(next) == true
        if !sameCapture {
            let following = followsSource
            close()
            identity = next
            followsSource = following
        }
        let nextOffset = min(sourceOffset, next.capturedBytes)
        self.sourceOffset = nextOffset
        // New accepted source navigation supersedes an automatic remap. It
        // never interrupts a deliberate preview action or starts worker work.
        if followingSource { cancel(silent: true) }
        if sourceNavigationEcho == nextOffset {
            sourceNavigationEcho = nil
            pendingSourceFollow = nil
        } else {
            sourceNavigationEcho = nil
            pendingSourceFollow = followsSource ? nextOffset : nil
        }
    }

    /// A split-pane preference queues one latest capture-bound anchor. The
    /// enclosing reader calls followSourceIfNeeded only after its worker drains.
    func setSourceFollowing(_ enabled: Bool) {
        guard followsSource != enabled else { return }
        followsSource = enabled
        if enabled {
            pendingSourceFollow = identity == nil ? nil : sourceOffset
        } else {
            pendingSourceFollow = nil
            if followingSource { cancel(silent: true) }
        }
    }
    func followSourceIfNeeded() {
        guard followsSource, canLocateSource, let pendingSourceFollow,
              let inventory = page?.inventory else { return }
        // Consume before submission: refusal/cancellation requires another
        // source navigation or an explicit From source page action, not a loop.
        self.pendingSourceFollow = nil
        navigate(.fromSource(offset: max(inventory.sourceBase, pendingSourceFollow)), automaticSourceFollow: true)
    }

    /// Called only by the active reader after an explicit window preference or
    /// source activation. Consume before starting work so SwiftUI recreation,
    /// same-capture paging and cancellation cannot restart a pending request.
    /// Accepted document/layout pages are reused without reflow or file reads.
    @discardableResult
    func presentPreferred(_ request: UUID, alreadyPresented: Bool = false) -> Bool {
        guard identity != nil, preferredPresentation != request else { return false }
        preferredPresentation = request
        // An existing sheet or split already satisfies this visit's default.
        // Do not restart its canceled work or interrupt its pending operation.
        if !alreadyPresented && !authorized { prepare() }
        return true
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
        canRestoreNavigation = false
        source = nil
        findResults = nil; selectedFindID = nil
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
            if model.richSupported && model.prefersStyled { model.prepareDisplay(preservingSourceFollow: true) }
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
    /// Navigation reuses the accepted shaped layout and bundled fonts. Both
    /// coordinate domains publish together only after the source remap validates.
    private static func restoredDisplay(for page: AtlasReaderDocumentPage,
        inventory: AtlasDocumentDisplayInventory, anchor: UInt64?,
        fonts: [AtlasDocumentDisplayFont], execute: Execute) async throws -> AtlasDocumentDisplayPage {
        guard inventory.qualifiedSources, page.inventory.matches(inventory.document) else {
            throw AtlasReaderError.invalidResponse
        }
        let original: UInt64?
        if let anchor { original = anchor }
        else if page.first == page.inventory.totalLines { original = page.inventory.identity.capturedBytes }
        else { original = page.lines.lazy.compactMap(\.enclosingOriginal).first?.start }
        guard let original, original >= page.inventory.sourceBase,
              original <= page.inventory.identity.capturedBytes else { throw AtlasReaderError.unavailable }
        let result = try await execute(.displayFromSource(offset: original))
        guard case .display(let display, nil) = result, display.inventory.matches(inventory) else {
            throw AtlasReaderError.invalidResponse
        }
        try display.validateFonts(fonts)
        return display
    }
    private func installNavigationPage(_ page: AtlasReaderDocumentPage, display: AtlasDocumentDisplayPage?) {
        self.page = page; source = nil; lineInput = String(page.first + 1)
        if let display {
            displayPage = display; displayHistory = []; showsStyled = prefersStyled
        } else { retireDisplayLocation() }
    }
    func prepareDisplay(preservingSourceFollow: Bool = false) {
        guard richSupported, canNavigate, let page else { return }
        if let reason = styledSourceLimitReason {
            // Keep the accepted logical page and its source/heading authority.
            // No doomed typography request or font/image acquisition is queued.
            retireDisplayLocation()
            notice = reason
            return
        }
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
            preservingSourceFollow: preservingSourceFollow,
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
        guard canLocateSource, let inventory = page?.inventory else { return }
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

    func find() {
        guard canFind else {
            if !AtlasReaderDocumentLimits.validFindNeedle(findInput) {
                notice = "Enter 1–1024 UTF-8 bytes of text without NUL."
            }
            return
        }
        loadFind(first: 0, selected: 1)
    }
    func previousFind() {
        guard canPreviousFind, let selectedFindID else { return }
        openFind(id: selectedFindID - 1)
    }
    func nextFind() {
        guard canNextFind, let selectedFindID else { return }
        openFind(id: selectedFindID + 1)
    }
    private func openFind(id: UInt64) {
        guard canNavigate, let results = currentFindResults, id > 0, id <= results.total else { return }
        if let target = results.target(id: id) { openFindMatch(target) }
        else {
            let first = ((id - 1) / AtlasReaderDocumentLimits.findMatches) * AtlasReaderDocumentLimits.findMatches
            loadFind(first: first, selected: id)
        }
    }
    func allowsFind(_ target: AtlasReaderDocumentMatchTarget) -> Bool {
        canNavigate && currentFindResults?.target(id: target.match.id) == target
    }
    func openFindMatch(_ target: AtlasReaderDocumentMatchTarget) {
        guard allowsFind(target), let previous = page else { return }
        let move = history.visit(previous.first)
        // Logical navigation can be accepted by the worker before a canceled
        // delivery. Keep the old page visible, with its old row actions retired.
        authorized = false; canRestoreNavigation = true; source = nil
        perform(.findMatch(target), notice: "Opening rendered match…") { model, result in
            guard case .page(let page) = result, page.inventory.matches(previous.inventory),
                  page.first == target.match.firstLine,
                  model.currentFindResults?.target(id: target.match.id) == target,
                  model.page?.id == previous.id,
                  model.history.commit(move, resolved: page.first) else { throw AtlasReaderError.invalidResponse }
            model.installNavigationPage(page, display: nil)
            model.prefersStyled = false
            model.selectedFindID = target.match.id
            model.authorized = true; model.canRestoreNavigation = false
        }
    }
    private func loadFind(first: UInt64, selected: UInt64) {
        guard canFind, let previous = page else { return }
        let needle = findInput, previousResults = currentFindResults
        let move = history.visit(previous.first)
        var restored: AtlasReaderDocumentPage?
        var selectedTarget: AtlasReaderDocumentMatchTarget?
        findResults = nil; selectedFindID = nil
        perform(.find(needle: needle, first: first), notice: "Finding rendered text…",
            resolve: { [weak self] result, execute in
                guard case .matches(let matches) = result,
                      matches.inventory.matches(previous.inventory),
                      matches.needle.utf8.elementsEqual(needle.utf8), matches.first == first,
                      self?.findInput.utf8.elementsEqual(needle.utf8) == true,
                      self?.page?.id == previous.id else { throw AtlasReaderError.invalidResponse }
                if let previousResults, first > 0, !previousResults.matchesSearch(matches) {
                    throw AtlasReaderError.invalidResponse
                }
                guard matches.total > 0 else { return result }
                guard let target = matches.target(id: selected) else { throw AtlasReaderError.invalidResponse }
                selectedTarget = target
                // A match range may cross rows or name renderer separators.
                // Open the engine's context row; never synthesize a highlight
                // or an original-source selection from these rendered offsets.
                self?.authorized = false; self?.canRestoreNavigation = true; self?.source = nil
                let opened = try await execute(.findMatch(target))
                guard case .page(let page) = opened, page.inventory.matches(matches.inventory),
                      page.first == target.match.firstLine else { throw AtlasReaderError.invalidResponse }
                restored = page
                return result
            }) { model, result in
                guard case .matches(let matches) = result,
                      matches.inventory.matches(previous.inventory), matches.first == first,
                      matches.needle.utf8.elementsEqual(model.findInput.utf8),
                      model.page?.id == previous.id else { throw AtlasReaderError.invalidResponse }
                if let restored, let selectedTarget {
                    guard matches.target(id: selectedTarget.match.id) == selectedTarget,
                          model.history.commit(move, resolved: restored.first) else { throw AtlasReaderError.invalidResponse }
                    model.installNavigationPage(restored, display: nil)
                    model.prefersStyled = false
                    model.selectedFindID = selectedTarget.match.id
                    model.authorized = true; model.canRestoreNavigation = false
                } else {
                    guard matches.total == 0 else { throw AtlasReaderError.invalidResponse }
                    model.selectedFindID = nil
                }
                model.findResults = matches
            }
    }

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
    func canOpenExternalLink(_ target: AtlasReaderDocumentLinkTarget) -> Bool {
        allows(target) && target.link.kind == .external && AtlasExternalWebLink(target.link.destination) != nil
    }

    /// Deliberate browser action, separate from inert link classification and
    /// local-source navigation. The worker confirms the complete retained row;
    /// cancellation, reflow, page replacement or close withdraws the action.
    func openExternalLink(_ target: AtlasReaderDocumentLinkTarget, open: @escaping ExternalNavigation) {
        guard allows(target), target.link.kind == .external else { return }
        guard let destination = AtlasExternalWebLink(target.link.destination) else {
            notice = "Only absolute HTTP or HTTPS links without credentials, hidden controls or ambiguous hosts can open in the browser."
            return
        }
        perform(.link(target), notice: "Verifying retained web link…", then: { model in
            guard model.canOpenExternalLink(target) else { return }
            let ticket = model.operation
            let opened = open(destination)
            // The OS adapter or an embedding host can synchronously replace or
            // close the reader. Do not revive its state after that handoff.
            guard model.operation == ticket, model.allows(target) else { return }
            model.notice = opened ? "Sent the link to your default browser."
                : "The default browser could not open this link."
        }) { model, result in
            guard case .link(let activation) = result, activation.target == target,
                  activation.page == nil, model.links?.target(id: target.link.id) == target else {
                throw AtlasReaderError.invalidResponse
            }
        }
    }
    func openLink(_ target: AtlasReaderDocumentLinkTarget, openLocal: LocalNavigation? = nil) {
        guard allows(target), let previous = page else { return }
        let styled = showsStyled && prefersStyled ? displayPage?.inventory : nil
        if target.link.kind.navigatesWithinCapture, styled?.qualifiedSources == false {
            notice = "This styled layout has no qualified source mapping. Switch to Text to navigate within the document."
            return
        }
        let fonts = displayFonts
        var restored: AtlasDocumentDisplayPage?
        let move = history.visit(previous.first)
        if target.link.kind.navigatesWithinCapture, styled != nil {
            // Submission can replace the coordinator's logical page before
            // this task resumes. Keep old pixels but withdraw their actions
            // until both coordinate domains are accepted together.
            authorized = false; canRestoreNavigation = true; source = nil
        }
        perform(.link(target), notice: "Resolving retained document link…", resolve: { result, execute in
            guard case .link(let activation) = result, activation.target == target else {
                throw AtlasReaderError.invalidResponse
            }
            if let page = activation.page, let styled {
                restored = try await Self.restoredDisplay(for: page, inventory: styled,
                    anchor: target.link.kind == .start ? page.inventory.sourceBase : nil, fonts: fonts, execute: execute)
            }
            return result
        }, then: { model in
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
                model.installNavigationPage(page, display: restored)
                model.authorized = true
                model.canRestoreNavigation = false
            } else {
                guard !target.link.kind.navigatesWithinCapture else { throw AtlasReaderError.invalidResponse }
                switch target.link.kind {
                case .local:
                    model.notice = openLocal == nil
                        ? "This local link needs an authorized capture route. No file was opened."
                        : "The enclosing source provider will resolve this retained link against its admitted files."
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
                  let offset = model.sourceOffset(for: target, reader: identity) else { return }
            if navigate != nil { model.sourceNavigationEcho = offset }
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
    /// preview work. Accepted pixels remain readable; a submitted source remap
    /// keeps its retired logical tokens read-only until a fresh page validates.
    func cancel(silent: Bool = false) {
        pendingSourceFollow = nil
        cancelOperation(silent: silent)
    }
    private func cancelOperation(silent: Bool) {
        operation = UUID()
        if task != nil { task?.cancel(); cancelWork() }
        task = nil
        busy = false
        followingSource = false
        finding = false
        if !silent { notice = "Preview operation canceled. Accepted source and search state are unchanged." }
    }
    func close() {
        cancel(silent: true)
        revoke()
        identity = nil; page = nil; headings = nil; links = nil; source = nil
        findResults = nil; selectedFindID = nil; findInput = ""
        preferredPresentation = nil
        followsSource = false; pendingSourceFollow = nil; sourceNavigationEcho = nil
        canRestoreNavigation = false
        displayPage = nil; displayFonts = []; displayHistory = []; showsStyled = false
        authorized = false; history.reset(); headingHistory = []; linkHistory = []
        sourceOffset = 0; lineInput = "1"; notice = ""
    }

    private func navigate(first: UInt64) {
        guard let page, first < page.inventory.totalLines else { return }
        navigate(.window(first: first))
    }
    private func navigate(_ request: AtlasReaderDocumentRequest,
                          historyMove: AtlasNavigationHistory<UInt64>.Move? = nil,
                          automaticSourceFollow: Bool = false) {
        let restoring: Bool
        if case .fromSource = request { restoring = canLocateSource } else { restoring = false }
        guard canNavigate || restoring, let previous = page else { return }
        let styled = showsStyled && prefersStyled ? displayPage?.inventory : nil
        if styled?.qualifiedSources == false {
            notice = "This styled layout has no qualified source mapping. Switch to Text to navigate within the document."
            return
        }
        let anchor: UInt64?
        switch request {
        case .fromSource(let offset): anchor = offset
        case .heading(let target): anchor = target.heading.original.start
        default: anchor = nil
        }
        let fonts = displayFonts
        var restored: AtlasDocumentDisplayPage?
        // Heading/source requests resolve their flow row in the shared engine;
        // only that accepted row, never the transient target, enters history.
        let move = historyMove ?? history.visit(previous.first)
        if styled != nil || automaticSourceFollow {
            authorized = false; canRestoreNavigation = true; source = nil
        }
        perform(request, notice: automaticSourceFollow ? "Following the retained source location…" : "Reading retained Markdown…",
            automaticSourceFollow: automaticSourceFollow, resolve: { result, execute in
                guard case .page(let page) = result, page.inventory.matches(previous.inventory) else {
                    throw AtlasReaderError.invalidResponse
                }
                if let styled {
                    restored = try await Self.restoredDisplay(for: page, inventory: styled,
                        anchor: anchor, fonts: fonts, execute: execute)
                }
                return result
            }) { model, result in
            guard case .page(let page) = result, page.inventory.matches(previous.inventory),
                  model.history.commit(move, resolved: page.first) else {
                throw AtlasReaderError.invalidResponse
            }
            model.installNavigationPage(page, display: restored)
            model.authorized = true
            model.canRestoreNavigation = false
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
                         automaticSourceFollow: Bool = false,
                         preservingSourceFollow: Bool = false,
                         resolve: (@MainActor (AtlasReaderDocumentResult, Execute) async throws -> AtlasReaderDocumentResult)? = nil,
                         then: (@MainActor (AtlasDocumentPreviewModel) -> Void)? = nil,
                         accept: @escaping @MainActor (AtlasDocumentPreviewModel, AtlasReaderDocumentResult) throws -> Void) {
        guard let identity else { return }
        cancelOperation(silent: true)
        // A deliberate preview action takes precedence over an older source
        // follow request. New source navigation can queue a newer anchor while
        // this operation is running.
        if !preservingSourceFollow { pendingSourceFollow = nil }
        sourceNavigationEcho = nil
        let ticket = UUID()
        operation = ticket
        busy = true
        followingSource = automaticSourceFollow
        switch request {
        case .find, .findMatch: finding = true
        default: finding = false
        }
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
                    self.followingSource = false
                    self.finding = false
                    then?(self)
                    // A reentrant source callback may have started another
                    // operation or closed this reader. Only its owner can
                    // decide whether the shared worker is available again.
                    if self.operation == ticket { self.didBecomeIdle?() }
                }
            } catch {
                guard let self, self.operation == ticket, !Task.isCancelled else { return }
                self.notice = (error as? AtlasReaderError)?.message ?? AtlasReaderError.unavailable.message
                if case .prepareDisplay = request {
                    self.showsStyled = false
                    if let readerError = error as? AtlasReaderError, case .engine(let code) = readerError {
                        switch code {
                        case "DOCUMENT_DISPLAY_SHAPING_UNSUPPORTED":
                            self.notice = "Bundled typography does not support all the text in this document. Text preview and the exact source remain available."
                        case "DOCUMENT_DISPLAY_SOURCE_LIMIT":
                            self.notice = "Styled preview admits up to 128 KiB of original source and decoded UTF-8. This capture exceeds a source limit; Text preview and source navigation remain available."
                        default:
                            self.notice += " Text preview remains available."
                        }
                    } else {
                        self.notice += " Text preview remains available."
                    }
                }
                self.busy = false; self.task = nil
                self.followingSource = false
                self.finding = false
                self.didBecomeIdle?()
            }
        }
    }
    private static func number(_ text: String) -> UInt64? {
        let text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let value = UInt64(text), String(value) == text else { return nil }
        return value
    }
}
