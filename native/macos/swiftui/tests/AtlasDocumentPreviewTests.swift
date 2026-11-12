// Presentation tests use the actual preview model and production value types.
// The injected coordinator operation returns typed fixtures, never fake proof
// of Markdown parsing, Swift-to-Rust ABI execution, or native presentation.
import Foundation

@MainActor private final class Fixture {
    var requests: [AtlasReaderDocumentRequest] = []
    var cancels = 0
    var revocations = 0
    var held = false
    var holdDisplay = false
    var exactSourceRows = false
    var externalDestination = "https://example.invalid/docs?q=a%20b#section"
    var failure: AtlasReaderError?
    var pending: [(CheckedContinuation<AtlasReaderDocumentResult, Error>, AtlasReaderDocumentResult)] = []
    var generation: UInt64 = 0
    var identity = Fixture.identity()
    var inventory: AtlasReaderDocumentInventory?
    var displayInventory: AtlasDocumentDisplayInventory?
    var totalDisplayItems: UInt64 = 385
    var unmappedDisplayItems: Set<UInt64> = []
    var totalLines: UInt64 = 1024
    var unmappedRows: Set<UInt64> = []
    var lastSourceAnchor: UInt64? {
        guard let request = requests.last, case .fromSource(let offset) = request else { return nil }
        return offset
    }

    static func identity(owner: UInt64 = 1, revision: UInt64 = 1,
                         bytes: UInt64 = 64000, encoding: String = "utf8", path: String = "/repo/readme.md") -> AtlasReaderIdentity {
        .init(owner: owner, file: 1, revision: revision, capturedBytes: bytes,
              pathBytes: Array(path.utf8), encoding: encoding, displayPath: path)
    }
    func model(supported: Bool = true, richSupported: Bool = false) -> AtlasDocumentPreviewModel {
        AtlasDocumentPreviewModel(supported: supported, richSupported: richSupported, execute: { try await self.execute($0) },
            cancel: { self.cancels += 1 }, revoke: { self.revocations += 1 })
    }
    func execute(_ request: AtlasReaderDocumentRequest) async throws -> AtlasReaderDocumentResult {
        requests.append(request)
        if let failure { self.failure = nil; throw failure }
        let value = try response(request)
        let displayRequest: Bool
        switch request {
        case .prepareDisplay, .displayPage, .displayFromSource: displayRequest = true
        default: displayRequest = false
        }
        if held || (holdDisplay && displayRequest) {
            return try await withCheckedThrowingContinuation { pending.append(($0, value)) }
        }
        return value
    }
    func release(_ index: Int = 0, replacement: AtlasReaderDocumentResult? = nil) {
        let (continuation, value) = pending.remove(at: index)
        continuation.resume(returning: replacement ?? value)
    }
    func reject(_ error: AtlasReaderError, at index: Int = 0) {
        let (continuation, _) = pending.remove(at: index)
        continuation.resume(throwing: error)
    }
    func response(_ request: AtlasReaderDocumentRequest) throws -> AtlasReaderDocumentResult {
        if case .prepare(let width) = request {
            generation += 1
            displayInventory = nil
            inventory = .init(identity: identity, generation: generation, width: width,
                totalLines: totalLines, totalHeadings: 130, renderedBytes: 16384, sourceBase: 3)
            return .prepared(page(first: 0), headings(first: 0))
        }
        switch request {
        case .window(let first): return .page(page(first: first))
        case .headings(let first): return .headings(headings(first: first))
        case .heading(let target): return .page(page(first: target.heading.renderedOffset / 16))
        case .fromSource(let offset):
            if exactSourceRows {
                return .page(page(first: offset == identity.capturedBytes ? totalLines : min(totalLines, (max(3, offset) - 3) / 8)))
            }
            return .page(page(first: offset == identity.capturedBytes ? totalLines : (offset == 3 ? 0 : 128)))
        case .source(let target):
            return .source(.init(target: target, visible: .init(start: 3, end: 8), text: "# hi\n",
                originalHex: "232068690a", enclosingOriginal: .init(start: 3, end: 7),
                selectionUTF8: .init(start: 0, end: 4), selectedHex: "23206869"))
        case .copy(let target, let mode):
            return .copy(.init(target: target, enclosingOriginal: .init(start: 3, end: 7),
                mode: mode, value: mode == .renderedText ? target.line.text : "23206869"))
        case .prepareLinks: return .links(links(first: 0))
        case .links(let first): return .links(links(first: first))
        case .link(let target):
            return .link(.init(target: target, page: target.link.kind.navigatesWithinCapture
                ? page(first: target.link.kind == .start ? 0 : 256) : nil))
        case .prepareDisplay(let width):
            guard let inventory else { throw AtlasReaderError.invalidInput }
            displayInventory = .init(document: inventory, width: width,
                bounds: .init(x: 0, y: 0, width: Double(width), height: Double(totalDisplayItems) * 20),
                totalItems: totalDisplayItems, unresolvedImages: (totalDisplayItems + 1) / 2,
                qualifiedSources: true, fonts: [])
            return .display(display(first: 0), fonts: [])
        case .displayPage(let first): return .display(display(first: first), fonts: nil)
        case .displayFromSource(let offset):
            let first = offset == identity.capturedBytes ? totalDisplayItems : min(totalDisplayItems, (max(3, offset) - 3) / 16)
            return .display(display(first: first), fonts: nil)
        case .prepare: throw AtlasReaderError.invalidInput
        }
    }
    /// Typed lifecycle fixtures contain only images/vectors and zero font
    /// resources. They exercise publication and source anchors without claiming
    /// to supply native fonts, shaped text, parser output or rendering proof.
    func display(first: UInt64) -> AtlasDocumentDisplayPage {
        let inventory = displayInventory!, end = min(totalDisplayItems, first + AtlasDocumentDisplayLimits.pageItems)
        let items = (first..<end).map { index -> AtlasDocumentDisplayItem in
            let original = unmappedDisplayItems.contains(index) ? nil : AtlasDocumentByteRange(start: 3 + index * 16, end: 7 + index * 16)
            let content: AtlasDocumentDisplayItem.Content = index.isMultiple(of: 2)
                ? .image(alt: "Retained image \(index)") : .vector(shape: "horizontal-rule", role: "rule", stroke: 1)
            return .init(id: index, bounds: .init(x: 0, y: Double(index) * 20, width: 120, height: 18),
                original: original, content: content)
        }
        return .init(id: UUID(), inventory: inventory, first: first, items: items,
            next: end < totalDisplayItems ? end : nil)
    }
    func links(first: UInt64) -> AtlasReaderDocumentLinks {
        let inventory = inventory!, total: UInt64 = 130, end = min(total, first + 64)
        let rows = (first..<end).map { index -> AtlasReaderDocumentLink in
            let kind: AtlasReaderDocumentLinkKind
            switch index {
            case 1: kind = .start
            case 2: kind = .local
            case 3: kind = .external
            case 4: kind = .unsupported
            default: kind = .heading
            }
            let destination: String
            switch kind {
            case .heading: destination = "#heading"
            case .start: destination = "#"
            case .local: destination = "guide.md#setup"
            case .external: destination = externalDestination
            case .unsupported: destination = "javascript:alert(1)"
            }
            return .init(id: index + 1, blockIndex: 0, evidence: .init(start: 0, end: identity.capturedBytes),
                destination: destination, display: destination, kind: kind, fragment: kind == .heading ? "heading" : nil)
        }
        return .init(id: UUID(), inventory: inventory, first: first, rows: rows, next: end < total ? end : nil,
            total: total, seen: total, diagnostics: 0, truncated: false, complete: true)
    }
    func page(first: UInt64) -> AtlasReaderDocumentPage {
        let inventory = inventory!
        let end = min(inventory.totalLines, first + 64)
        var lines: [AtlasReaderDocumentLine] = []
        for row in first..<end {
            let rendered = AtlasDocumentByteRange(start: row * 16, end: row * 16 + 15)
            let original = AtlasDocumentByteRange(start: 3 + row * 8, end: 7 + row * 8)
            lines.append(AtlasReaderDocumentLine(id: row, text: "Row \(row)",
                rendered: rendered, enclosingOriginal: unmappedRows.contains(row) ? nil : original))
        }
        return .init(id: UUID(), inventory: inventory, first: first, lines: lines,
            next: end < inventory.totalLines ? end : nil, wholeDocumentVisible: first == 0 && end == inventory.totalLines)
    }
    func headings(first: UInt64) -> AtlasReaderDocumentHeadings {
        let inventory = inventory!, end = min(inventory.totalHeadings, first + 64)
        let rows = (first..<end).map { row in
            AtlasReaderDocumentHeading(slug: "section-\(row)", title: "Heading \(row)",
                renderedOffset: row * 16, original: .init(start: 3 + row * 8, end: 7 + row * 8))
        }
        return .init(id: UUID(), inventory: inventory, first: first, rows: rows,
            next: end < inventory.totalHeadings ? end : nil)
    }
}

@main private enum AtlasDocumentPreviewTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ value: @autoclosure () -> Bool, _ message: String) {
        precondition(value(), message)
        checks += 1
    }
    @MainActor private static func wait(_ condition: () -> Bool) async {
        for _ in 0..<1000 {
            if condition() { return }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        preconditionFailure("Timed out waiting for model operation")
    }
    @MainActor private static func settle() async {
        for _ in 0..<10 { await Task.yield() }
        try? await Task.sleep(nanoseconds: 2_000_000)
    }
    @MainActor private static func ready(_ fixture: Fixture, _ model: AtlasDocumentPreviewModel) async {
        model.bind(identity: fixture.identity, sourceOffset: 0)
        model.prepare()
        await wait { !model.busy }
        check(model.authorized && model.page != nil, "Preparation publishes the retained document")
    }

    @MainActor static func main() async throws {
        do {
            let f = Fixture(), m = f.model()
            check(f.requests.isEmpty && !m.canPrepare, "Construction is inert without a capture")
            m.bind(identity: f.identity, sourceOffset: 0)
            check(f.requests.isEmpty && m.canPrepare, "Binding source does not parse it")
            m.widthInput = "03"; m.prepare()
            check(f.requests.isEmpty && !m.busy, "Noncanonical width is refused before execution")
            m.widthInput = "513"; m.prepare()
            check(f.requests.isEmpty && !m.busy, "Width cap is enforced")
            m.widthInput = "96"; await ready(f, m)
            check(m.page?.lines.count == 64 && m.headings?.rows.count == 64, "Only bounded flow/heading pages retained")
            check(m.coverage.contains("not native shaped") && m.coverage.contains("enclosing Markdown"), "Coverage states actual representation")
            let target = m.page!.target(at: 0)!
            m.next(); await wait { !m.busy }
            check(m.page?.first == 64 && m.canBack, "Next follows engine pagination")
            check(!m.allows(target), "Old row cannot activate in a replacement page")
            m.back(); await wait { !m.busy }
            check(m.page?.first == 0 && !m.canBack, "Back restores prior flow location")
            check(!m.allows(target), "Back does not resurrect an old page token")
            m.lineInput = "129"; m.goToLine(); await wait { !m.busy }
            check(m.page?.first == 128, "Human flow rows are one-based")
            let requests = f.requests.count
            for invalid in ["0", "0129", "-1", "1025", "18446744073709551616"] {
                m.lineInput = invalid; m.goToLine()
            }
            check(f.requests.count == requests && m.page?.first == 128, "Invalid row input cannot disturb accepted page")
            m.bind(identity: f.identity, sourceOffset: 0); m.fromSource(); await wait { !m.busy }
            if case .fromSource(let offset) = f.requests.last! { check(offset == 3, "Source synchronization skips the UTF-8 BOM") }
            else { preconditionFailure("Missing source anchor") }
            m.bind(identity: f.identity, sourceOffset: 512); m.fromSource(); await wait { !m.busy }
            if case .fromSource(let offset) = f.requests.last! { check(offset == 512, "Same capture paging updates synchronization anchor") }
            else { preconditionFailure("Missing updated source anchor") }
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            let old = m.headings!.target(slug: "section-0")!
            m.nextHeadings(); await wait { !m.busy }
            check(m.headings?.first == 64 && m.canHeadingsBack, "Headings page without source recapture")
            let count = f.requests.count
            m.openHeading(old)
            check(f.requests.count == count, "Old heading page cannot authorize a jump")
            let target = m.headings!.target(slug: "section-70")!
            m.openHeading(target); await wait { !m.busy }
            check(m.page?.first == 70, "Canonical upstream heading target drives navigation")
            m.nextHeadings(); await wait { !m.busy }
            check(m.headings?.rows.count == 2 && !m.canHeadingsNext, "Final short heading page is not treated as missing content")
            m.previousHeadings(); await wait { !m.busy }
            check(m.headings?.first == 64, "Heading history is independent of document history")
            m.back(); await wait { !m.busy }
            check(m.page?.first == 0, "Heading jumps participate in flow history")
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            let target = m.page!.target(at: 0)!
            m.showSource(target); await wait { !m.busy }
            check(m.source?.selectedHex == "23206869", "Source action keeps original Markdown evidence")
            check(m.source?.selectionUTF8 == .init(start: 0, end: 4), "Source context retains its independent selection map")
            var copies: [String] = []
            m.copy(target, mode: .renderedText) { copies.append($0); return true }
            await wait { !m.busy }
            check(copies == ["Row 0"] && m.notice == "Copied rendered row text.", "Rendered copy delivers text, not markup")
            m.copy(target, mode: .enclosingMarkdown) { copies.append($0); return true }
            await wait { !m.busy }
            check(copies == ["Row 0", "23206869"] && m.notice.contains("original Markdown bytes as hex"), "Original copy is explicit lossless hex")
            m.copy(target, mode: .renderedText) { _ in false }; await wait { !m.busy }
            check(m.notice == "Clipboard write failed.", "Failed clipboard writes are not success")
            m.next(); await wait { !m.busy }
            check(m.source == nil, "New flow page retires prior source context")
            let count = f.requests.count
            m.copy(target, mode: .renderedText) { _ in preconditionFailure("Stale row copied") }
            check(f.requests.count == count, "Stale copy is refused before worker submission")
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            let page = m.page!, target = page.target(at: 0)!
            f.held = true
            m.widthInput = "80"; m.prepare(); await wait { f.pending.count == 1 }
            check(!m.authorized && m.page?.id == page.id && !m.allows(target), "Pending reflow leaves old text read-only")
            let cancellations = f.cancels
            m.widthInput = "0"; m.prepare()
            check(f.cancels == cancellations && m.busy, "Invalid replacement cannot cancel an admitted reflow")
            m.cancel(); f.release(); await settle()
            check(!m.busy && !m.authorized && m.page?.id == page.id, "Canceled reflow cannot reactivate old document")
            f.held = false; m.widthInput = "72"; m.prepare(); await wait { !m.busy }
            check(m.authorized && m.page?.inventory.width == 72, "Retry reflow establishes a fresh generation")
            f.failure = .engine("DOCUMENT_SOURCE_LIMIT")
            m.prepare(); await wait { !m.busy }
            check(!m.authorized && m.notice.contains("DOCUMENT_SOURCE_LIMIT"), "Engine refusal is visible and old preview stays read-only")
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            f.held = true
            let before = m.page!.id
            m.next(); await wait { f.pending.count == 1 }
            m.cancel(silent: true); f.release(); await settle()
            check(m.page?.id == before && !m.busy, "Canceled page cannot replace current flow")
            m.next(); await wait { f.pending.count == 1 }
            f.release(); await wait { !m.busy }
            check(m.page?.first == 64, "Canceled navigation can be retried")
            var writes = 0
            let target = m.page!.target(at: 64)!
            m.copy(target, mode: .renderedText) { _ in writes += 1; return true }
            await wait { f.pending.count == 1 }
            m.close(); f.release(); await settle()
            check(writes == 0 && m.page == nil, "Close suppresses delayed clipboard side effects")
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            f.held = true
            let old = m.page!.target(at: 0)!
            m.showSource(old); await wait { f.pending.count == 1 }
            m.bind(identity: Fixture.identity(revision: 2), sourceOffset: 0)
            f.release(); await settle()
            check(m.page == nil && m.source == nil && !m.authorized, "Replaced capture cannot receive old source mapping")
            check(m.canPrepare && !m.allows(old), "New capture admits preparation, not old target authority")
            f.held = false; f.identity = Fixture.identity(revision: 2)
            m.prepare(); await wait { !m.busy }
            check(m.page?.inventory.identity.revision == 2, "New capture is used by subsequent preview")
            m.bind(identity: Fixture.identity(revision: 2, path: "/repo/re\u{301}adme.md"), sourceOffset: 0)
            check(m.page == nil, "Changed raw path revokes document even with equal counters")
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            f.held = true
            m.widthInput = "80"; m.prepare(); await wait { f.pending.count == 1 }
            m.widthInput = "100"; m.prepare(); await wait { f.pending.count == 2 }
            f.held = false
            f.release(1); await wait { !m.busy }
            let accepted = m.page!.id
            f.release(); await settle()
            check(m.page?.id == accepted && m.page?.inventory.width == 100, "Out-of-order old reflow cannot replace newest page")
        }
        do {
            for (encoding, bytes) in [("utf16le", UInt64(100)), ("unsupported", 100), ("utf8", 65537)] {
                let f = Fixture(), m = f.model()
                m.bind(identity: Fixture.identity(bytes: bytes, encoding: encoding), sourceOffset: 0)
                m.prepare()
                check(!m.canPrepare && !m.busy && f.requests.isEmpty, "Unsupported preview does not read/parse or destroy source")
                check(!m.notice.isEmpty, "Preview admission failure has a visible reason")
            }
            let f = Fixture(), m = f.model(supported: false)
            m.bind(identity: f.identity, sourceOffset: 0); m.prepare()
            check(!m.canPrepare && f.requests.isEmpty, "Source-only host remains inert")
        }
        do {
            let f = Fixture()
            var model: AtlasDocumentPreviewModel? = f.model()
            await ready(f, model!)
            f.held = true
            model!.next(); await wait { f.pending.count == 1 }
            weak var weakModel = model
            model = nil
            check(weakModel == nil, "Pending task must not keep dismissed preview alive")
            f.release(); await settle()
        }
        do {
            let f1 = Fixture(), f2 = Fixture(), m1 = f1.model(), m2 = f2.model()
            f2.identity = Fixture.identity(owner: 2)
            await ready(f1, m1); await ready(f2, m2)
            let second = m2.page!.id
            m1.close()
            check(m2.page?.id == second && m2.authorized, "Closing one preview leaves another host independent")
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            let target = m.page!.target(at: 0)!
            f.held = true
            var writes = 0
            m.copy(target, mode: .renderedText) { _ in writes += 1; return true }
            await wait { f.pending.count == 1 }
            let wrong = AtlasReaderDocumentCopy(target: target, enclosingOriginal: .init(start: 3, end: 7),
                mode: .enclosingMarkdown, value: "23206869")
            f.release(replacement: .copy(wrong)); await wait { !m.busy }
            check(writes == 0 && m.notice == AtlasReaderError.invalidResponse.message,
                  "Wrong copy domain cannot reach the clipboard callback")
            f.held = false
            m.copy(target, mode: .renderedText) { _ in m.close(); return true }
            await wait { !m.busy }; await settle()
            check(m.page == nil && m.notice.isEmpty, "Reentrant clipboard close cannot republish an old success message")
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            for line in 2...131 {
                m.lineInput = String(line); m.goToLine(); await wait { !m.busy }
            }
            for _ in 0..<128 { m.back(); await wait { !m.busy } }
            check(!m.canBack && m.page?.first == 2, "Flow history retains at most 128 locations")
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            let page = m.page!, target = page.target(at: 0)!
            var navigations = 0
            f.held = true
            m.showSource(target, navigate: { _ in navigations += 1 })
            await wait { f.pending.count == 1 }
            // This current-capture slice is internally coherent, but names
            // only "hi" while the row advertised the enclosing "# hi" block.
            // The production model must retain its exact region equality gate.
            let differentRegion = AtlasReaderDocumentSource(target: target,
                visible: .init(start: 3, end: 8), text: "# hi\n", originalHex: "232068690a",
                enclosingOriginal: .init(start: 5, end: 7),
                selectionUTF8: .init(start: 2, end: 4), selectedHex: "6869")
            f.release(replacement: .source(differentRegion)); await wait { !m.busy }
            check(navigations == 0 && m.source == nil && m.sourceOffset(for: target, reader: f.identity) == nil,
                  "A different enclosing region cannot reach source navigation")
            check(m.page?.id == page.id && m.authorized && m.notice == AtlasReaderError.invalidResponse.message,
                  "Rejected source correspondence leaves rendered reading available")
            m.showSource(target, navigate: { received in
                check(received == target, "Source callback retains its exact current row target")
                navigations += 1
            })
            await wait { f.pending.count == 1 }
            f.release(); await wait { !m.busy }
            check(navigations == 1 && m.source?.enclosingOriginal == target.line.enclosingOriginal
                  && m.sourceOffset(for: target, reader: f.identity) == 3,
                  "The matching enclosing-block receipt enables source navigation")
        }
        await anchoredPreparation()
        await documentLinks()
        await externalWebLinks()
        await styledLifecycle()
        await styledDeliveries()
        print("AtlasDocumentPreviewTests: \(checks) checks passed")
    }

    @MainActor private static func styledLifecycle() async {
        do {
            let f = Fixture(), m = f.model(richSupported: true)
            f.exactSourceRows = true; f.holdDisplay = true
            m.bind(identity: f.identity, sourceOffset: 1027)
            m.prepare(); await wait { f.pending.count == 1 }
            let logical = m.page!
            check(logical.first == 128 && m.authorized && m.busy && m.displayPage == nil,
                  "Automatic typography preparation keeps the resolved logical source page available")
            if case .prepareDisplay(let width) = f.requests.last! { check(width == 768, "Styled width derives from accepted logical width") }
            else { preconditionFailure("Missing automatic display preparation") }
            f.release(); await wait { f.pending.count == 1 }
            if case .displayFromSource(let offset) = f.requests.last! { check(offset == 1027, "Styled preparation restores the accepted source block") }
            else { preconditionFailure("Missing styled source restoration") }
            check(m.displayPage == nil && !m.showsStyled && m.page?.id == logical.id,
                  "The styled start page cannot flash before source restoration succeeds")
            f.release(); await wait { !m.busy }
            check(m.showsStyled && m.displayPage?.first == 64 && m.displayPage?.items.count == 128
                  && m.displayFonts.isEmpty && !m.canDisplayBack && m.canDisplayNext,
                  "A bounded image/vector page publishes after restoration without synthetic font resources")
            check(m.coverage.contains("placeholders") && m.coverage.contains("enclosing Markdown blocks"),
                  "Styled coverage keeps image and source-mapping limits explicit")
            f.holdDisplay = false
            let oldDisplay = m.displayPage!, oldItem = oldDisplay.items[0]
            m.nextDisplay(); await wait { !m.busy }
            check(m.displayPage?.first == 192 && m.canDisplayBack && m.page?.id == logical.id && !m.canBack,
                  "Styled pagination follows item cursors without borrowing logical row history")
            check(!m.allowsDisplayItem(oldItem, pageID: oldDisplay.id), "A prior styled page cannot authorize source actions")
            m.previousDisplay(); await wait { !m.busy }
            check(m.displayPage?.first == 64 && !m.canDisplayBack
                  && !m.allowsDisplayItem(oldItem, pageID: oldDisplay.id),
                  "Styled back restores location without reviving an old page token")
            m.nextDisplay(); await wait { !m.busy }
            m.bind(identity: f.identity, sourceOffset: 9000)
            m.widthInput = "80"; m.prepare(); await wait { !m.busy }
            let sourceAnchors = f.requests.compactMap { request -> UInt64? in
                if case .fromSource(let offset) = request { return offset }; return nil
            }
            check(sourceAnchors.last == 3075 && m.page?.first == 384 && m.displayPage?.first == 192,
                  "Width reflow follows the visible styled block rather than the old logical row or another pane's anchor")
            check(m.showsStyled && m.page?.inventory.width == 80 && m.displayPage?.inventory.width == 640
                  && m.displayPage?.inventory.document.generation == m.page?.inventory.generation && !m.canDisplayBack,
                  "Reflow replaces both coordinate domains together and retires old styled history")
            m.selectStyled(false); await wait { !m.busy }
            check(!m.showsStyled && m.page?.first == 384 && m.displayPage == nil && !m.canDisplayBack,
                  "Switching to Text resolves the visible styled source block and retires its stale display location")
            m.selectStyled(true); await wait { !m.busy }
            check(m.showsStyled && m.displayPage?.first == 192,
                  "Switching back to Styled restores the current Text source block")
            m.lineInput = "257"; m.goToLine(); await wait { !m.busy }
            check(m.page?.first == 256 && !m.showsStyled && m.displayPage == nil,
                  "Logical row navigation cannot leave an unrelated styled page visible")
            m.selectStyled(true); await wait { !m.busy }
            check(m.displayPage?.first == 128, "Styled retry follows the newly accepted logical row")
            m.prepareLinks(); await wait { !m.busy }
            m.openLink(m.links!.target(id: 2)!); await wait { !m.busy }
            check(m.page?.first == 0 && m.displayPage == nil && !m.showsStyled && !m.canDisplayBack,
                  "Accepted document-link navigation retires old styled content and its history")
            m.selectStyled(true); await wait { !m.busy }
            let sourcePage = m.displayPage!, sourceItem = sourcePage.items[0]
            var sourceTargets: [AtlasReaderDocumentTarget] = []
            m.showDisplaySource(sourceItem, pageID: sourcePage.id) { sourceTargets.append($0) }
            await wait { !m.busy }
            check(sourceTargets.count == 1 && sourceTargets[0].line.enclosingOriginal == sourceItem.original
                  && m.sourceOffset(for: sourceTargets[0], reader: f.identity) == 3,
                  "A current styled element resolves through the logical source map before source navigation")
            for _ in 0..<3 { m.nextDisplay(); await wait { !m.busy } }
            check(m.displayPage?.first == 384 && m.displayPage?.items.count == 1 && !m.canDisplayNext,
                  "The final short styled page is bounded and ends item navigation")
            let calls = f.requests.count
            m.showDisplaySource(sourceItem, pageID: sourcePage.id) { _ in preconditionFailure("Old styled item navigated") }
            check(f.requests.count == calls, "An obsolete styled page cannot submit source work")
        }
        do {
            let f = Fixture(), m = f.model(richSupported: true)
            f.unmappedDisplayItems = Set(0..<128)
            await ready(f, m)
            let page = m.page!, calls = f.requests.count, revocations = f.revocations
            m.widthInput = "80"; m.prepare()
            check(m.showsStyled && m.authorized && !m.busy && f.requests.count == calls && f.revocations == revocations,
                  "Unmapped styled content refuses width reflow before revoking the accepted document")
            m.selectStyled(false)
            check(!m.showsStyled && m.page?.id == page.id && m.canNavigate && f.requests.count == calls
                  && m.notice.contains("no mapped source region"),
                  "Unmapped Styled-to-Text navigation keeps the prior logical location and explains the fallback")
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            let calls = f.requests.count
            m.selectStyled(true)
            check(!m.showsStyled && m.displayPage == nil && m.authorized && f.requests.count == calls,
                  "A host without rich transport remains a working logical preview without rich requests")
        }
        do {
            let f = Fixture(), m = f.model(richSupported: true)
            f.holdDisplay = true
            m.bind(identity: f.identity, sourceOffset: 0); m.prepare()
            await wait { f.pending.count == 1 }
            f.reject(.engine("DOCUMENT_DISPLAY_SHAPING_UNSUPPORTED")); await wait { !m.busy }
            check(m.authorized && m.page?.first == 0 && m.displayPage == nil && !m.showsStyled
                  && m.notice.contains("Text preview and the exact source remain available"),
                  "Unsupported typography falls back to the accepted logical document without losing source access")
            m.next(); await wait { !m.busy }
            f.holdDisplay = false; m.selectStyled(true); await wait { !m.busy }
            check(m.page?.first == 64 && m.showsStyled && m.displayPage?.first == 32,
                  "Logical navigation and an explicit styled retry still work after a typography refusal")
        }
    }

    @MainActor private static func styledDeliveries() async {
        for resolving in [false, true] {
            let f = Fixture(), m = f.model(richSupported: true)
            f.exactSourceRows = true; f.holdDisplay = true
            m.bind(identity: f.identity, sourceOffset: 1027); m.prepare()
            await wait { f.pending.count == 1 }
            if resolving { f.release(); await wait { f.pending.count == 1 } }
            let calls = f.requests.count, logical = m.page!.id
            m.cancel(); f.release(); await settle()
            check(m.page?.id == logical && m.authorized && !m.busy && m.displayPage == nil && !m.showsStyled
                  && f.requests.count == calls,
                  "Cancellation at either rich preparation stage suppresses delayed publication and unstarted restoration")
        }
        do {
            let f = Fixture(), m = f.model(richSupported: true)
            f.holdDisplay = true
            m.bind(identity: f.identity, sourceOffset: 0); m.prepare()
            await wait { f.pending.count == 1 }
            m.cancel(); m.selectStyled(true); await wait { f.pending.count == 2 }
            f.release(1); await wait { !m.busy }
            let accepted = m.displayPage!
            f.release(); await settle()
            check(m.displayPage?.id == accepted.id && m.showsStyled,
                  "A late canceled rich preparation cannot replace a newer accepted display")
            m.nextDisplay(); await wait { f.pending.count == 1 }
            m.cancel(); f.release(); await settle()
            check(m.displayPage?.id == accepted.id && !m.canDisplayBack && m.canDisplayNext,
                  "Canceled styled pagination preserves both accepted content and item history")
        }
        do {
            let f = Fixture(), m = f.model(richSupported: true)
            await ready(f, m)
            f.holdDisplay = true
            m.nextDisplay(); await wait { f.pending.count == 1 }
            m.bind(identity: Fixture.identity(revision: 2), sourceOffset: 512)
            f.release(); await settle()
            check(m.page == nil && m.displayPage == nil && m.displayFonts.isEmpty && !m.authorized && !m.busy,
                  "A delayed rich page cannot revive a replaced capture")
        }
        for foreignCapture in [false, true] {
            let f = Fixture(), m = f.model(richSupported: true), foreign = Fixture()
            await ready(f, m)
            let accepted = m.displayPage!, logical = m.page!.id
            foreign.identity = foreignCapture ? Fixture.identity(owner: 99) : f.identity
            foreign.generation = foreignCapture ? 0 : 10
            _ = try! foreign.response(.prepare(width: 96))
            _ = try! foreign.response(.prepareDisplay(width: 768))
            f.holdDisplay = true
            m.nextDisplay(); await wait { f.pending.count == 1 }
            f.release(replacement: .display(foreign.display(first: 128), fonts: nil))
            await wait { !m.busy }
            check(m.displayPage?.id == accepted.id && m.page?.id == logical && m.authorized
                  && !m.canDisplayBack && m.notice == AtlasReaderError.invalidResponse.message,
                  "A foreign capture or generation cannot replace a styled page or advance its history")
        }
        do {
            let f = Fixture(), m = f.model(richSupported: true), foreign = Fixture()
            f.exactSourceRows = true; f.holdDisplay = true
            m.bind(identity: f.identity, sourceOffset: 1027); m.prepare()
            await wait { f.pending.count == 1 }
            f.release(); await wait { f.pending.count == 1 }
            foreign.generation = 10
            _ = try! foreign.response(.prepare(width: 96))
            _ = try! foreign.response(.prepareDisplay(width: 768))
            f.release(replacement: .display(foreign.display(first: 64), fonts: nil))
            await wait { !m.busy }
            check(m.page?.first == 128 && m.authorized && m.displayPage == nil && !m.showsStyled
                  && m.notice.contains(AtlasReaderError.invalidResponse.message),
                  "A foreign source-restoration generation cannot publish a rich page after a valid preparation")
        }
    }

    @MainActor private static func anchoredPreparation() async {
        do {
            let f = Fixture(), m = f.model()
            m.bind(identity: f.identity, sourceOffset: 512)
            f.held = true
            m.prepare(); await wait { f.pending.count == 1 }
            f.release(); await wait { f.pending.count == 1 && f.lastSourceAnchor == 512 }
            check(m.page == nil && m.headings == nil && m.busy && !m.authorized,
                  "Initial preparation cannot publish the first page before resolving the source reader anchor")
            f.release(); await wait { !m.busy }
            check(m.page?.first == 128 && m.lineInput == "129" && m.authorized && !m.canBack,
                  "Initial preview follows the accepted source anchor without adding a spurious start-page history entry")
        }
        do {
            let f = Fixture(), m = f.model()
            f.unmappedRows = [64, 65]
            await ready(f, m)
            m.next(); await wait { !m.busy }
            let previous = m.page!, headings = m.headings!, target = previous.target(at: 66)!
            m.bind(identity: f.identity, sourceOffset: 5000)
            f.held = true
            m.widthInput = "80"; m.prepare(); await wait { f.pending.count == 1 }
            f.release(); await wait { f.pending.count == 1 && f.lastSourceAnchor == 531 }
            check(m.page?.id == previous.id && m.headings?.id == headings.id && m.lineInput == "65"
                  && !m.authorized && !m.allows(target),
                  "Pending restoration retains the old page and skips generated rows when selecting its source anchor")
            f.release(); await wait { !m.busy }
            check(m.page?.first == 128 && m.page?.inventory.width == 80 && m.lineInput == "129"
                  && m.authorized && !m.canBack && !m.canForward,
                  "Reflow publishes the engine-resolved block position and retires obsolete flow-row history")
            check(!m.allows(target), "Old mapped rows cannot regain authority after source-anchored reflow")
        }
        do {
            let f = Fixture(), m = f.model()
            f.unmappedRows = Set(0..<64)
            m.bind(identity: f.identity, sourceOffset: 2)
            m.prepare(); await wait { !m.busy }
            check(m.authorized && f.requests.count == 1,
                  "An initial source start or BOM uses the known first page even when leading rows are unmapped")
            let before = m.page!, requests = f.requests.count, revocations = f.revocations
            m.widthInput = "80"; m.prepare()
            check(m.page?.id == before.id && m.authorized && !m.busy && f.requests.count == requests
                  && f.revocations == revocations && m.notice.contains("no mapped source region"),
                  "An all-generated page refuses unanchored reflow before disturbing accepted state")
            m.next(); await wait { !m.busy }
            m.prepare(); await wait { !m.busy }
            check(m.authorized && f.lastSourceAnchor == 515 && m.page?.inventory.width == 80,
                  "Moving to a mapped preview page makes anchored reflow available")
        }
        do {
            let f = Fixture(), m = f.model()
            m.bind(identity: f.identity, sourceOffset: UInt64.max)
            m.prepare(); await wait { !m.busy }
            check(f.lastSourceAnchor == f.identity.capturedBytes && m.page?.first == 1024 && m.page?.lines.isEmpty == true,
                  "A source EOF anchor is clamped to the retained capture and preserves the valid empty window")
            m.bind(identity: f.identity, sourceOffset: 0)
            f.totalLines = 2048
            m.widthInput = "80"; m.prepare(); await wait { !m.busy }
            check(f.lastSourceAnchor == f.identity.capturedBytes && m.page?.first == 2048 && m.authorized,
                  "Reflow restores EOF using source bytes rather than the old row count or another source-pane location")
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            m.next(); await wait { !m.busy }
            let previous = m.page!, target = previous.target(at: 64)!
            f.held = true
            m.widthInput = "80"; m.prepare(); await wait { f.pending.count == 1 }
            f.release(); await wait { f.pending.count == 1 && f.lastSourceAnchor == 515 }
            m.cancel()
            let requests = f.requests.count
            m.copy(target, mode: .renderedText) { _ in preconditionFailure("Canceled reflow copied an old row") }
            m.next(); m.back(); m.forward()
            f.release(); await settle()
            check(m.page?.id == previous.id && m.lineInput == "65" && !m.authorized && !m.busy
                  && f.requests.count == requests,
                  "Canceled anchor restoration preserves the accepted page and refuses its old navigation/copy authority")
            f.held = false
            m.bind(identity: f.identity, sourceOffset: 9000)
            m.widthInput = "72"; m.prepare(); await wait { !m.busy }
            check(m.authorized && f.lastSourceAnchor == 515 && m.page?.inventory.width == 72,
                  "Retry derives its anchor from the last accepted preview even while that page is read-only")
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            m.next(); await wait { !m.busy }
            let previous = m.page!, headings = m.headings!
            f.held = true
            m.widthInput = "80"; m.prepare(); await wait { f.pending.count == 1 }
            f.release(); await wait { f.pending.count == 1 && f.lastSourceAnchor == 515 }
            f.reject(.engine("DOCUMENT_INVALID_RANGE")); await wait { !m.busy }
            check(m.page?.id == previous.id && m.headings?.id == headings.id && m.lineInput == "65"
                  && !m.authorized && m.notice.contains("DOCUMENT_INVALID_RANGE"),
                  "An inaccessible source anchor reports failure without publishing the prepared start page")
        }
        for foreignCapture in [false, true] {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            m.next(); await wait { !m.busy }
            let previous = m.page!
            let foreign = Fixture()
            foreign.identity = Fixture.identity(owner: 99)
            foreign.generation = 1
            _ = try! foreign.response(.prepare(width: 96))
            f.held = true
            m.prepare(); await wait { f.pending.count == 1 }
            f.release(); await wait { f.pending.count == 1 && f.lastSourceAnchor == 515 }
            f.release(replacement: .page(foreignCapture ? foreign.page(first: 128) : previous))
            await wait { !m.busy }
            check(m.page?.id == previous.id && m.lineInput == "65" && !m.authorized
                  && m.notice == AtlasReaderError.invalidResponse.message,
                  "Restoration rejects a wrong capture or old generation even when its page is otherwise coherent")
        }
        do {
            let f = Fixture(), m = f.model(), foreign = Fixture()
            foreign.identity = Fixture.identity(owner: 99)
            let response = try! foreign.response(.prepare(width: 96))
            m.bind(identity: f.identity, sourceOffset: 512)
            f.held = true
            m.prepare(); await wait { f.pending.count == 1 }
            f.release(replacement: response); await wait { !m.busy }
            check(m.page == nil && !m.authorized && f.requests.count == 1
                  && m.notice == AtlasReaderError.invalidResponse.message,
                  "A mismatched prepared capture is refused before submitting its source anchor")
        }
        for resolving in [false, true] {
            let f = Fixture(), m = f.model()
            m.bind(identity: f.identity, sourceOffset: 512)
            f.held = true
            m.prepare(); await wait { f.pending.count == 1 }
            if resolving {
                f.release(); await wait { f.pending.count == 1 && f.lastSourceAnchor == 512 }
            }
            let replacement = Fixture.identity(revision: 2)
            m.bind(identity: replacement, sourceOffset: 777)
            f.release(); await settle()
            check(m.page == nil && m.headings == nil && !m.authorized && !m.busy
                  && f.requests.count == (resolving ? 2 : 1),
                  "Replacing the capture at either stage suppresses stale publication and any unstarted anchor request")
            f.identity = replacement; f.held = false
            m.prepare(); await wait { !m.busy }
            check(m.authorized && m.page?.inventory.identity.revision == 2 && f.lastSourceAnchor == 777,
                  "A replacement capture follows its own source anchor")
        }
        do {
            let f = Fixture(), m = f.model()
            m.bind(identity: f.identity, sourceOffset: 512)
            f.held = true
            m.prepare(); await wait { f.pending.count == 1 }
            m.bind(identity: f.identity, sourceOffset: 9000)
            m.prepare(); await wait { f.pending.count == 2 }
            f.release(); await settle()
            check(f.requests.count == 2, "A superseded initial preparation cannot start its source lookup")
            f.release(); await wait { f.pending.count == 1 && f.lastSourceAnchor == 9000 }
            f.release(); await wait { !m.busy }
            check(m.authorized && m.page?.inventory.generation == 2,
                  "A replacement initial preparation captures the current source-reader position")
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            m.next(); await wait { !m.busy }
            f.held = true
            m.widthInput = "80"; m.prepare(); await wait { f.pending.count == 1 }
            f.release(); await wait { f.pending.count == 1 && f.lastSourceAnchor == 515 }
            m.bind(identity: f.identity, sourceOffset: 9000)
            m.widthInput = "100"; m.prepare(); await wait { f.pending.count == 2 }
            f.release(1); await wait { f.pending.count == 2 && f.lastSourceAnchor == 515 }
            f.release(1); await wait { !m.busy }
            let accepted = m.page!
            f.release(); await settle()
            check(m.authorized && m.page?.id == accepted.id && accepted.inventory.width == 100,
                  "A late first restoration cannot replace the latest width or displace the accepted source anchor")
        }
        do {
            let f = Fixture()
            var model: AtlasDocumentPreviewModel? = f.model()
            await ready(f, model!)
            f.held = true
            model!.widthInput = "80"; model!.prepare(); await wait { f.pending.count == 1 }
            f.release(); await wait { f.pending.count == 1 && f.lastSourceAnchor == 3 }
            weak var weakModel = model
            model = nil
            check(weakModel == nil, "The second-stage source lookup cannot retain a dismissed preview model")
            f.release(); await settle()
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            m.next(); await wait { !m.busy }
            m.next(); await wait { !m.busy }
            m.back(); await wait { !m.busy }
            let previous = m.page!
            f.failure = .engine("DOCUMENT_INVALID_RANGE")
            m.forward(); await wait { !m.busy }
            check(m.page?.id == previous.id && m.canBack && m.canForward,
                  "A failed navigation leaves both directions of accepted history intact")
            f.held = true
            m.back(); await wait { f.pending.count == 1 }
            m.cancel(); f.release(); await settle()
            check(m.page?.id == previous.id && m.canBack && m.canForward,
                  "A canceled navigation leaves both directions of accepted history intact")
        }
    }

    @MainActor private static func externalWebLinks() async {
        // The real URL policy is exercised without opening a browser or making
        // a network request. Native OS handoff remains a physical-Mac lane.
        let allowed = [
            "https://example.invalid/docs?q=a%20b#section", "http://localhost:8080/",
            "HTTPS://example.invalid/path", "https://[::1]:8443/guide",
            "https://xn--bcher-kva.example/guide", "https://example.invalid/文档?q=é",
            "https://example.invalid/%252e%252e?value=%252f",
        ]
        for destination in allowed {
            check(AtlasExternalWebLink(destination) != nil, "A supported web destination was refused: \(destination)")
        }
        let prefix = "https://example.invalid/"
        let boundary = prefix + String(repeating: "a", count: AtlasReaderDocumentLimits.destinationBytes - prefix.utf8.count)
        check(AtlasExternalWebLink(boundary) != nil && AtlasExternalWebLink(boundary + "a") == nil,
              "The URL byte allowance must include the complete destination")
        let escaped = "https://example.invalid/a%2Fb?q=%252f#part%202"
        check(AtlasExternalWebLink(escaped)?.url.absoluteString == escaped,
              "The OS must receive original percent escapes, not a second decoded interpretation")
        for destination in [
            "", "../guide.md", "file:///tmp/source", "file://remote/path", "javascript:alert(1)",
            "mailto:test@example.invalid", "ftp://example.invalid/file", "//example.invalid/path",
            "https:example.invalid/path", "https:///path", "https://", "https://example.invalid:",
            "https://user@example.invalid/", "https://user:secret@example.invalid/", "https://@example.invalid/",
            "https://example.invalid%40other.invalid/", "https://%65xample.invalid/", "https://例子.invalid/",
            "https://example.invalid:65536/", "https://example.invalid:0/", "https://example.invalid:-1/",
            " https://example.invalid", "https://example.invalid ", "https://example.invalid/a b",
            "https://example.invalid/\nforged", "https://example.invalid/%0a", "https://example.invalid/%0D%0a",
            "https://example.invalid/%00", "https://example.invalid/\u{202e}txt", "https://example.invalid/%E2%80%AEtxt",
            "https://example.invalid/\u{2066}x", "https://example.invalid/%C2%85", "https://example.invalid/%E2%80%A8",
            "https://example.invalid/\\other", "https://example.invalid/%5cother", "https://example.invalid/%",
            "https://example.invalid/%0", "https://example.invalid/%GG", "https://example.invalid/%FF",
        ] {
            check(AtlasExternalWebLink(destination) == nil, "An unsafe or ambiguous destination reached URL admission")
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            m.prepareLinks(); await wait { !m.busy }
            let target = m.links!.target(id: 4)!, previous = m.page!
            var opened: [String] = []
            m.openLink(target); await wait { !m.busy }
            check(opened.isEmpty && m.notice.contains("No browser"), "Ordinary classification must remain inert")
            check(m.canOpenExternalLink(target), "A retained HTTP(S) target should enable the explicit action")
            f.held = true
            m.openExternalLink(target) { link in
                check(!m.busy && m.allows(target), "Browser callback ran before worker drain or after revocation")
                opened.append(link.url.absoluteString); return true
            }
            await wait { f.pending.count == 1 }
            m.openExternalLink(target) { _ in opened.append("duplicate"); return true }
            await settle()
            check(opened.isEmpty && f.pending.count == 1, "Handoff preceded verification or a second click queued a duplicate")
            f.release(); await wait { !m.busy }; await settle()
            check(opened == [f.externalDestination] && m.page?.id == previous.id && !m.canBack,
                  "Explicit web handoff must preserve source and navigation while passing the verified URL once")
            check(m.notice == "Sent the link to your default browser.", "Successful OS acceptance was not reported")
            f.held = false
            f.failure = .engine("DOCUMENT_STALE_GENERATION")
            m.openExternalLink(target) { _ in opened.append("failed verification"); return true }
            await wait { !m.busy }
            check(opened == [f.externalDestination] && m.notice.contains("DOCUMENT_STALE_GENERATION"),
                  "Failed retained-link verification still invoked the browser adapter")
            m.openExternalLink(target) { _ in false }; await wait { !m.busy }
            check(m.notice.contains("could not open") && m.page?.id == previous.id,
                  "OS refusal must remain visible without replacing the source")
            m.openExternalLink(target) { _ in m.close(); return true }; await wait { !m.busy }; await settle()
            check(m.page == nil && m.links == nil && m.notice.isEmpty,
                  "A reentrant browser adapter revived a closed preview")
        }
        for interruption in ["cancel", "reflow", "rebind", "close", "reload"] {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            m.prepareLinks(); await wait { !m.busy }
            let target = m.links!.target(id: 4)!
            var opens = 0
            f.held = true
            m.openExternalLink(target) { _ in opens += 1; return true }
            await wait { f.pending.count == 1 }
            switch interruption {
            case "cancel": m.cancel()
            case "reflow": m.widthInput = "80"; m.prepare(); await wait { f.pending.count == 2 }
            case "rebind": m.bind(identity: Fixture.identity(revision: 2), sourceOffset: 0)
            case "reload": m.cancel(silent: true); m.prepareLinks(); await wait { f.pending.count == 2 }
            default: m.close()
            }
            f.held = false
            while !f.pending.isEmpty { f.release() }
            await settle(); await wait { !m.busy }
            check(opens == 0, "A \(interruption) preview still delivered a delayed browser action")
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            m.prepareLinks(); await wait { !m.busy }
            let target = m.links!.target(id: 4)!, requests = f.requests.count
            var opens = 0
            for id: UInt64 in [1, 2, 3, 5] {
                m.openExternalLink(m.links!.target(id: id)!) { _ in opens += 1; return true }
            }
            check(f.requests.count == requests && opens == 0, "Another link classification acquired web authority")
            m.nextLinks(); await wait { !m.busy }
            let pagedRequests = f.requests.count
            m.openExternalLink(target) { _ in opens += 1; return true }
            check(f.requests.count == pagedRequests && opens == 0, "An obsolete link-page target reached the worker")
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            m.prepareLinks(); await wait { !m.busy }
            let target = m.links!.target(id: 4)!, before = m.page!
            var opens = 0
            f.held = true
            m.openExternalLink(target) { _ in opens += 1; return true }
            await wait { f.pending.count == 1 }
            let substituted = AtlasReaderDocumentLinkTarget(inventory: target.inventory,
                pageID: UUID(), link: target.link)
            f.release(replacement: .link(.init(target: substituted, page: nil)))
            await wait { !m.busy }
            check(opens == 0 && m.page?.id == before.id && m.notice == AtlasReaderError.invalidResponse.message,
                  "A mismatched verified response reached the browser")
            m.openExternalLink(target) { _ in opens += 1; return true }
            await wait { f.pending.count == 1 }
            f.release(replacement: .link(.init(target: target, page: f.page(first: 256))))
            await wait { !m.busy }
            check(opens == 0 && m.page?.id == before.id,
                  "A source-navigation response acquired external handoff authority")
        }
        for destination in ["javascript:alert(1)", "https://user:secret@example.invalid/", "https://example.invalid/%0a"] {
            let f = Fixture(), m = f.model()
            f.externalDestination = destination
            await ready(f, m)
            m.prepareLinks(); await wait { !m.busy }
            let target = m.links!.target(id: 4)!, requests = f.requests.count
            var opens = 0
            check(!m.canOpenExternalLink(target), "Classification alone enabled an unsafe external action")
            m.openExternalLink(target) { _ in opens += 1; return true }
            check(opens == 0 && f.requests.count == requests && m.notice.contains("Only absolute HTTP"),
                  "A disallowed address reached the worker or OS adapter")
        }
    }

    @MainActor private static func documentLinks() async {
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            let page = m.page!
            m.prepareLinks(); await wait { !m.busy }
            check(m.links?.rows.count == 64 && m.page?.id == page.id && m.canLinksNext,
                  "Loading links does not replace the flow or claim full list visibility")
            check(m.linkCoverage.contains("130 parsed occurrences") && m.linkCoverage.contains("not an inline span"),
                  "Link evidence and completeness are explicit")
            let old = m.links!.target(id: 1)!
            m.nextLinks(); await wait { !m.busy }
            check(m.links?.first == 64 && m.canLinksBack, "Link occurrences have independent pagination")
            let calls = f.requests.count
            m.openLink(old)
            check(f.requests.count == calls, "A stale link-list row reached the worker")
            m.nextLinks(); await wait { !m.busy }
            check(m.links?.rows.count == 2 && !m.canLinksNext, "Short final link page remains complete")
            m.previousLinks(); await wait { !m.busy }
            m.previousLinks(); await wait { !m.busy }
            check(m.links?.first == 0 && !m.allows(old), "Back does not resurrect an old link-page token")
            let heading = m.links!.target(id: 1)!
            m.openLink(heading); await wait { !m.busy }
            check(m.page?.first == 256 && m.canBack, "Parsed heading activation enters navigation history")
            m.back(); await wait { !m.busy }
            check(m.page?.first == 0 && m.canForward, "Back restores the page before a heading link")
            m.forward(); await wait { !m.busy }
            let start = m.links!.target(id: 2)!
            m.openLink(start); await wait { !m.busy }
            check(m.page?.first == 0 && m.canBack, "The document-start link navigates without a heading slug")
            m.widthInput = "80"; m.prepare(); await wait { !m.busy }
            check(m.links == nil && !m.allows(heading), "Reflow revokes link targets together with rendered rows")
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            m.prepareLinks(); await wait { !m.busy }
            let before = m.page!, target = m.links!.target(id: 1)!
            f.failure = .engine("DOCUMENT_HEADING_MISSING")
            m.openLink(target); await wait { !m.busy }
            check(m.page?.id == before.id && !m.canBack && m.notice.contains("DOCUMENT_HEADING_MISSING"),
                  "A missing heading preserves the current page and history")
            for id: UInt64 in [3, 4, 5] {
                m.openLink(m.links!.target(id: id)!); await wait { !m.busy }
                check(m.page?.id == before.id && !m.canBack && !m.notice.isEmpty,
                      "Local/external/unsupported outcomes cannot open or replace source")
            }
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            m.prepareLinks(); await wait { !m.busy }
            let target = m.links!.target(id: 3)!
            var navigations = 0
            f.held = true
            m.openLink(target, openLocal: { _ in navigations += 1 })
            await wait { f.pending.count == 1 }
            m.cancel(); f.release(); await settle()
            check(navigations == 0 && !m.busy, "Canceled local-link classification delivered a later host callback")
            m.openLink(target, openLocal: { received in
                check(received == target && !m.busy, "Host callback lost the qualified row or ran before worker release")
                navigations += 1
                m.close()
            })
            await wait { f.pending.count == 1 }
            f.release(); await wait { !m.busy }; await settle()
            check(navigations == 1 && m.page == nil && m.links == nil && m.notice.isEmpty,
                  "Reentrant local navigation republished closed preview state")
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            m.prepareLinks(); await wait { !m.busy }
            let target = m.links!.target(id: 1)!, previous = m.page!
            f.held = true
            m.openLink(target); await wait { f.pending.count == 1 }
            let foreign = Fixture()
            foreign.identity = Fixture.identity(owner: 99)
            _ = try! foreign.response(.prepare(width: 96))
            f.release(replacement: .link(.init(target: target, page: foreign.page(first: 256))))
            await wait { !m.busy }
            check(m.page?.id == previous.id && !m.canBack && m.notice == AtlasReaderError.invalidResponse.message,
                  "A foreign capture replaced source through link activation")
            m.openLink(target); await wait { f.pending.count == 1 }
            m.bind(identity: Fixture.identity(owner: 2), sourceOffset: 0)
            f.release(); await settle()
            check(m.page == nil && m.links == nil && !m.authorized,
                  "A delayed link revived an old capture after replacement")
        }
        do {
            let f = Fixture(), m = f.model()
            await ready(f, m)
            f.held = true
            m.prepareLinks(); await wait { f.pending.count == 1 }
            let complete = f.links(first: 0)
            let partial = AtlasReaderDocumentLinks(id: UUID(), inventory: complete.inventory, first: 0,
                rows: complete.rows, next: complete.next, total: complete.total, seen: 150,
                diagnostics: 2, truncated: true, complete: false)
            f.release(replacement: .links(partial)); await wait { !m.busy }
            check(m.linkCoverage.contains("incomplete") && m.linkCoverage.contains("2 parser diagnostics")
                  && m.linkCoverage.contains("omitted"), "Partial extraction was presented as complete navigation")
            let target = m.links!.target(id: 1)!
            m.prepareLinks(); await wait { f.pending.count == 1 }
            check(m.links == nil && !m.allows(target), "Link reprepare left old rows active while foreign work ran")
            m.cancel(); f.release(); await settle()
            check(m.links == nil, "Canceled link inventory revived its prior authority")
        }
    }
}
