// Presentation tests use the actual preview model and production value types.
// The injected coordinator operation returns typed fixtures, never fake proof
// of Markdown parsing, Swift-to-Rust ABI execution, or native presentation.
import Foundation

@MainActor private final class Fixture {
    var requests: [AtlasReaderDocumentRequest] = []
    var cancels = 0
    var revocations = 0
    var held = false
    var failure: AtlasReaderError?
    var pending: [(CheckedContinuation<AtlasReaderDocumentResult, Error>, AtlasReaderDocumentResult)] = []
    var generation: UInt64 = 0
    var identity = Fixture.identity()
    var inventory: AtlasReaderDocumentInventory?

    static func identity(owner: UInt64 = 1, revision: UInt64 = 1,
                         bytes: UInt64 = 64000, encoding: String = "utf8", path: String = "/repo/readme.md") -> AtlasReaderIdentity {
        .init(owner: owner, file: 1, revision: revision, capturedBytes: bytes,
              pathBytes: Array(path.utf8), encoding: encoding, displayPath: path)
    }
    func model(supported: Bool = true) -> AtlasDocumentPreviewModel {
        AtlasDocumentPreviewModel(supported: supported, execute: { try await self.execute($0) },
            cancel: { self.cancels += 1 }, revoke: { self.revocations += 1 })
    }
    func execute(_ request: AtlasReaderDocumentRequest) async throws -> AtlasReaderDocumentResult {
        requests.append(request)
        if let failure { self.failure = nil; throw failure }
        let value = try response(request)
        if held {
            return try await withCheckedThrowingContinuation { pending.append(($0, value)) }
        }
        return value
    }
    func release(_ index: Int = 0, replacement: AtlasReaderDocumentResult? = nil) {
        let (continuation, value) = pending.remove(at: index)
        continuation.resume(returning: replacement ?? value)
    }
    func response(_ request: AtlasReaderDocumentRequest) throws -> AtlasReaderDocumentResult {
        if case .prepare(let width) = request {
            generation += 1
            inventory = .init(identity: identity, generation: generation, width: width,
                totalLines: 1024, totalHeadings: 130, renderedBytes: 16384, sourceBase: 3)
            return .prepared(page(first: 0), headings(first: 0))
        }
        switch request {
        case .window(let first): return .page(page(first: first))
        case .headings(let first): return .headings(headings(first: first))
        case .heading(let target): return .page(page(first: target.heading.renderedOffset / 16))
        case .fromSource(let offset): return .page(page(first: offset == 3 ? 0 : 128))
        case .source(let target):
            return .source(.init(target: target, visible: .init(start: 3, end: 8), text: "# hi\n",
                originalHex: "232068690a", enclosingOriginal: .init(start: 3, end: 7),
                selectionUTF8: .init(start: 0, end: 4), selectedHex: "23206869"))
        case .copy(let target, let mode):
            return .copy(.init(target: target, enclosingOriginal: .init(start: 3, end: 7),
                mode: mode, value: mode == .renderedText ? target.line.text : "23206869"))
        case .prepare: throw AtlasReaderError.invalidInput
        }
    }
    func page(first: UInt64) -> AtlasReaderDocumentPage {
        let inventory = inventory!
        let end = min(inventory.totalLines, first + 64)
        var lines: [AtlasReaderDocumentLine] = []
        for row in first..<end {
            let rendered = AtlasDocumentByteRange(start: row * 16, end: row * 16 + 15)
            let original = AtlasDocumentByteRange(start: 3 + row * 8, end: 7 + row * 8)
            lines.append(AtlasReaderDocumentLine(id: row, text: "Row \(row)",
                rendered: rendered, enclosingOriginal: original))
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
        print("AtlasDocumentPreviewTests: \(checks) checks passed")
    }
}
