// Production document decoder/admission and preview-model regressions. Fixed
// foreign responses qualify host behavior, not Markdown parsing or Mac drawing.
// The companion Rust host suite exercises the extended styled source allowance.
import Foundation

private enum LargeDocumentWire {
    static let originalText = "## Café 🦀 tail\r\n"
    static let rendered = "Café 🦀 tail"

    static func encoded(_ text: String, as encoding: String) -> [UInt8] {
        if encoding == "utf8" { return Array(text.utf8) }
        precondition(["utf16le", "utf16be"].contains(encoding))
        let codec: String.Encoding = encoding == "utf16le" ? .utf16LittleEndian : .utf16BigEndian
        return Array(text.data(using: codec)!)
    }
    static func original(_ encoding: String) -> [UInt8] { encoded(originalText, as: encoding) }
    static func info(bytes: UInt64, encoding: String = "utf8", revision: UInt64 = 1) throws -> AtlasReaderInfo {
        let value = header("info", bytes: bytes, encoding: encoding, revision: revision)
        return try AtlasReaderInfo.decode(DocumentWireFixture.json(value), handle: 7, path: DocumentWireFixture.path)
    }
    static func header(_ command: String, bytes: UInt64, encoding: String = "utf8",
                       revision: UInt64 = 1) -> [String: Any] {
        var value = DocumentWireFixture.header(command, owner: 7)
        value["captured_bytes"] = String(bytes); value["source_revision"] = String(revision)
        value["encoding"] = encoding
        return value
    }
    static func originalRange(_ identity: AtlasReaderIdentity) -> AtlasDocumentByteRange {
        .init(start: identity.capturedBytes - UInt64(original(identity.encoding).count), end: identity.capturedBytes)
    }
    static func page(_ command: String, info: AtlasReaderInfo, generation: UInt64,
                     width: UInt64, first: UInt64 = 0) -> [String: Any] {
        var value = DocumentWireFixture.page(command, owner: 7, generation: generation, width: width, first: first)
        let identity = info.identity, original = originalRange(identity)
        value["captured_bytes"] = String(identity.capturedBytes); value["source_revision"] = String(identity.revision)
        value["encoding"] = identity.encoding
        value["parser_source_base"] = identity.encoding == "utf8" ? "3" : "2"
        value["flow_lines"] = (first..<min(first + 64, 70)).map { row in
            ["flow_line": String(row), "text": rendered,
             "rendered_utf8_range": DocumentWireFixture.range(row * 20, row * 20 + 16),
             "enclosing_original_range": DocumentWireFixture.range(original.start, original.end)] as [String: Any]
        }
        value["total_headings"] = "1"
        value["headings"] = [["slug": "café-tail", "title": rendered,
            "rendered_utf8_offset": "1280", "original_range": DocumentWireFixture.range(original.start, original.end)]]
        value["next_heading"] = NSNull()
        return value
    }
    static func selection(_ target: AtlasReaderDocumentTarget) -> [String: Any] {
        DocumentWireFixture.selection(generation: target.inventory.generation,
            start: target.rendered.start, end: target.rendered.end)
    }
    static func copy(_ target: AtlasReaderDocumentTarget, mode: AtlasReaderDocumentCopyMode) -> [String: Any] {
        let identity = target.inventory.identity, region = originalRange(identity)
        var value = header("document-copy", bytes: identity.capturedBytes,
            encoding: identity.encoding, revision: identity.revision)
        value.merge(selection(target)) { _, next in next }
        value["enclosing_original_range"] = DocumentWireFixture.range(region.start, region.end)
        value["copy_domain"] = mode == .renderedText ? "rendered-text-utf8" : "enclosing-original-markdown"
        if mode == .renderedText { value["text"] = rendered }
        else { value["original_hex"] = DocumentWireFixture.hex(original(identity.encoding)) }
        return value
    }
    static func source(_ target: AtlasReaderDocumentTarget) -> [String: Any] {
        let identity = target.inventory.identity, region = originalRange(identity)
        let padding = AtlasReaderDocumentLimits.context + 4
        let visible = AtlasDocumentByteRange(start: region.start - padding, end: region.end)
        let prefix = String(repeating: "x", count: Int(padding) / (identity.encoding == "utf8" ? 1 : 2))
        let text = prefix + originalText, context = encoded(text, as: identity.encoding)
        let selectedStart = UInt64(prefix.utf8.count)
        var value = header("document-source", bytes: identity.capturedBytes,
            encoding: identity.encoding, revision: identity.revision)
        value["text_kind"] = "logical-captured-text-not-shaped"
        value["requested_original_range"] = DocumentWireFixture.range(visible.start, visible.end)
        value["visible_range"] = DocumentWireFixture.range(visible.start, visible.end)
        value["range_limited"] = false; value["boundaries_adjusted"] = false; value["has_replacements"] = false
        value["first_physical_line"] = NSNull(); value["next_offset"] = NSNull()
        value["text"] = text; value["original_hex"] = DocumentWireFixture.hex(context)
        var selected = selection(target)
        selected["original_range"] = DocumentWireFixture.range(region.start, region.end)
        selected["window_utf8_range"] = DocumentWireFixture.range(selectedStart, selectedStart + UInt64(originalText.utf8.count))
        selected["original_hex"] = DocumentWireFixture.hex(original(identity.encoding))
        value["selection"] = selected
        return value
    }
}

@MainActor private final class LargePreviewFixture {
    let info: AtlasReaderInfo
    let state = AtlasReaderDocumentState()
    var requests: [AtlasReaderDocumentRequest] = []
    var width: UInt64 = 96
    var styledError: AtlasReaderError?
    var hold = false
    var pending: [(CheckedContinuation<AtlasReaderDocumentResult, Error>, AtlasReaderDocumentResult)] = []
    var styledCalls: Int { requests.filter { if case .prepareDisplay = $0 { true } else { false } }.count }
    init(bytes: UInt64, encoding: String = "utf8") throws { info = try LargeDocumentWire.info(bytes: bytes, encoding: encoding) }
    func model() -> AtlasDocumentPreviewModel {
        AtlasDocumentPreviewModel(supported: true, richSupported: true,
            execute: { try await self.execute($0) }, cancel: {}, revoke: { self.state.revoke() })
    }
    func release() {
        let (continuation, result) = pending.removeFirst()
        continuation.resume(returning: result)
    }
    func execute(_ request: AtlasReaderDocumentRequest) async throws -> AtlasReaderDocumentResult {
        requests.append(request)
        let job = try state.admit(request, info: info)
        let result: AtlasReaderDocumentResult
        if case .prepareDisplay(let points) = request {
            if let styledError { throw styledError }
            guard let inventory = state.inventory else { throw AtlasReaderError.invalidInput }
            let display = AtlasDocumentDisplayInventory(document: inventory, width: points,
                bounds: .init(x: 0, y: 0, width: Double(points), height: 20), totalItems: 0,
                unresolvedImages: 0, qualifiedSources: true, fonts: [])
            result = .display(.init(id: UUID(), inventory: display, first: 0, items: [], next: nil), fonts: [])
        } else {
            var wire: [String: Any]
            switch request {
            case .prepare(let columns):
                width = columns
                wire = LargeDocumentWire.page(request.command, info: info, generation: job.generation, width: width)
            case .window(let first):
                wire = LargeDocumentWire.page(request.command, info: info, generation: job.generation, width: width, first: first)
            case .fromSource(let offset):
                let base: UInt64 = info.identity.encoding == "utf8" ? 3 : 2
                let first: UInt64 = offset == info.identity.capturedBytes ? 70 : (offset > base ? 64 : 0)
                wire = LargeDocumentWire.page(request.command, info: info, generation: job.generation, width: width, first: first)
                wire["original_anchor"] = String(offset)
            case .heading(let target):
                wire = LargeDocumentWire.page(request.command, info: info, generation: job.generation, width: width, first: 64)
                wire["heading_slug"] = target.heading.slug
                wire["heading_original_range"] = DocumentWireFixture.range(target.heading.original.start, target.heading.original.end)
            case .source(let target): wire = LargeDocumentWire.source(target)
            case .copy(let target, let mode): wire = LargeDocumentWire.copy(target, mode: mode)
            default: throw AtlasReaderError.invalidInput
            }
            result = try job.decode(DocumentWireFixture.json(wire))
        }
        try state.accept(result, job: job)
        if hold { return try await withCheckedThrowingContinuation { pending.append(($0, result)) } }
        return result
    }
}

@main private enum AtlasLargeDocumentPreviewTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ condition: @autoclosure () -> Bool, _ reason: String) {
        precondition(condition(), reason); checks += 1
    }
    @MainActor private static func wait(_ condition: () -> Bool) async {
        for _ in 0..<1000 {
            if condition() { return }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        preconditionFailure("Timed out waiting for document operation")
    }
    @MainActor private static func settle() async {
        for _ in 0..<10 { await Task.yield() }
        try? await Task.sleep(nanoseconds: 2_000_000)
    }
    @MainActor private static func rejects(_ reason: String, _ work: () throws -> Void) {
        do { try work(); preconditionFailure(reason) } catch { checks += 1 }
    }
    @MainActor static func main() async throws {
        for encoding in ["utf8", "utf16le", "utf16be"] {
            let step: UInt64 = encoding == "utf8" ? 1 : 2
            for bytes in [UInt64(64 * 1024), 64 * 1024 + step, 100 * 1024,
                          128 * 1024, 128 * 1024 + step, 256 * 1024] {
                let fixture = try LargePreviewFixture(bytes: bytes, encoding: encoding), model = fixture.model()
                model.bind(identity: fixture.info.identity, sourceOffset: 0)
                check(model.canPrepare && fixture.requests.isEmpty, "Binding an admitted capture remains inert")
                model.prepare(); await wait { !model.busy }
                check(model.authorized && model.page != nil, "Logical preview supports the full public source range")
                if bytes <= 128 * 1024 {
                    check(model.showsStyled && fixture.styledCalls == 1,
                        "Styled preview admits the extended source range through the inclusive 128 KiB boundary")
                } else {
                    check(!model.showsStyled && model.displayPage == nil && fixture.styledCalls == 0,
                        "Larger Markdown skips typography and font/image work while retaining logical preview")
                    check(model.coverage.contains("limited to 128 KiB") && model.notice.contains("Text preview"),
                        "Automatic fallback explains the distinct presentation bound")
                }
            }
        }
        for (bytes, encoding) in [(UInt64(256 * 1024 + 1), "utf8"),
                                  (UInt64(256 * 1024 + 2), "utf16le"),
                                  (UInt64(256 * 1024 + 2), "utf16be"),
                                  (UInt64(96 * 1024), "unsupported")] {
            let fixture = try LargePreviewFixture(bytes: bytes, encoding: encoding), model = fixture.model()
            model.bind(identity: fixture.info.identity, sourceOffset: 0); model.prepare()
            check(!model.canPrepare && !model.busy && fixture.requests.isEmpty,
                "Oversized or unsupported source is refused without submitting document work")
            check(!model.notice.isEmpty, "Refusal preserves a visible source-reader fallback")
        }
        for encoding in ["utf8", "utf16le", "utf16be"] {
            try await navigationAndCopy(encoding: encoding)
        }
        for bytes in [UInt64(100 * 1024), 192 * 1024] {
            try await canceledAndReplaced(bytes: bytes)
        }
        try await decodedLimitKeepsText()
        try malformedReplies()
        print("AtlasLargeDocumentPreviewTests: \(checks) checks passed")
    }
    @MainActor private static func navigationAndCopy(encoding: String) async throws {
        let fixture = try LargePreviewFixture(bytes: 256 * 1024, encoding: encoding), model = fixture.model()
        let original = LargeDocumentWire.originalRange(fixture.info.identity)
        let originalHex = DocumentWireFixture.hex(LargeDocumentWire.original(encoding))
        model.bind(identity: fixture.info.identity, sourceOffset: original.start)
        model.prepare(); await wait { !model.busy }
        check(model.page?.first == 64 && model.page?.lines.count == 6 && model.headings?.rows.count == 1,
            "Initial source navigation beyond the styled ceiling restores its bounded logical page")
        let page = model.page!, target = page.target(at: 64)!
        var sourceAnchor: UInt64?
        model.showSource(target) { target in sourceAnchor = model.sourceOffset(for: target, reader: fixture.info.identity) }
        await wait { !model.busy }
        check(sourceAnchor == original.start && model.source?.selectedHex == originalHex,
            "The late UTF8 or UTF16 region roundtrips accented text, surrogate pairs and CRLF through original-byte mapping")
        var copied = ""
        model.copy(target, mode: .enclosingMarkdown) { copied = $0; return true }; await wait { !model.busy }
        check(copied == originalHex, "Exact Markdown copy retains encoded source bytes, delimiters and CRLF")
        model.copy(target, mode: .renderedText) { copied = $0; return true }; await wait { !model.busy }
        check(copied == LargeDocumentWire.rendered, "Rendered copy remains a separate declared domain")
        let requests = fixture.requests.count
        model.selectStyled(true)
        check(fixture.requests.count == requests && model.page?.id == page.id && model.authorized && !model.showsStyled,
            "Explicit Styled keeps the accepted logical location without a doomed worker call")
        check(model.notice.contains("limited to 128 KiB"), "Explicit fallback explains its limit")
        model.widthInput = "80"; model.prepare(); await wait { !model.busy }
        check(model.page?.inventory.width == 80 && model.page?.first == 64 && !model.allows(target),
            "Reflow keeps the original anchor and revokes the old page token")
        check(fixture.styledCalls == 0 && model.coverage.contains("Text preview"), "Reflow preserves the logical fallback")
        rejects("A direct rich request must preserve its independent source bound") {
            _ = try fixture.state.admit(.prepareDisplay(width: 640), info: fixture.info)
        }
        check(fixture.state.page?.inventory.matches(model.page!.inventory) == true,
            "A refused rich request preserves accepted logical authority")
    }
    @MainActor private static func canceledAndReplaced(bytes: UInt64) async throws {
        let fixture = try LargePreviewFixture(bytes: bytes), model = fixture.model()
        model.bind(identity: fixture.info.identity, sourceOffset: 0)
        fixture.hold = true; model.prepare(); await wait { fixture.pending.count == 1 }
        model.cancel(); fixture.release(); await settle()
        check(model.page == nil && !model.authorized && fixture.styledCalls == 0,
            "Canceled late preparation cannot publish a logical page or schedule rich work")
        fixture.hold = false; model.prepare(); await wait { !model.busy }
        check(model.authorized && model.page != nil, "A later explicit preparation recovers after cancellation")
        let styledCalls = fixture.styledCalls
        check(styledCalls == (bytes <= 128 * 1024 ? 1 : 0),
            "Recovery respects the independent styled source allowance")
        fixture.hold = true; model.widthInput = "80"; model.prepare(); await wait { fixture.pending.count == 1 }
        let replacement = try LargeDocumentWire.info(bytes: 32 * 1024, revision: 2)
        model.bind(identity: replacement.identity, sourceOffset: 0)
        fixture.release(); await settle()
        check(model.page == nil && !model.authorized && model.canPrepare && fixture.styledCalls == styledCalls,
            "Capture replacement suppresses old large-document responses and their follow-up work")
    }
    @MainActor private static func decodedLimitKeepsText() async throws {
        for encoding in ["utf16le", "utf16be"] {
            // A raw-admitted UTF16 capture can exceed the decoded UTF8 allowance.
            // The foreign engine owns that decision; this only verifies its UI fallback.
            let fixture = try LargePreviewFixture(bytes: 100 * 1024, encoding: encoding), model = fixture.model()
            fixture.styledError = .engine("DOCUMENT_DISPLAY_SOURCE_LIMIT")
            model.bind(identity: fixture.info.identity, sourceOffset: 0)
            model.prepare(); await wait { !model.busy }
            check(model.authorized && model.page != nil && !model.showsStyled && fixture.styledCalls == 1,
                "A decoded-source refusal preserves the accepted logical preview")
            check(model.notice.contains("128 KiB") && model.notice.contains("decoded UTF-8")
                    && model.notice.contains("Text preview"),
                "A decoded-source refusal explains both source domains and the usable fallback")
            let target = model.page!.target(at: 0)!
            model.showSource(target); await wait { !model.busy }
            check(model.source?.selectedHex == DocumentWireFixture.hex(LargeDocumentWire.original(encoding))
                    && model.authorized,
                "A refused styled layout leaves exact source navigation available")
        }
    }
    @MainActor private static func malformedReplies() throws {
        for encoding in ["utf8", "utf16le", "utf16be"] {
            for bytes in [UInt64(100 * 1024), 128 * 1024, 256 * 1024] {
                let info = try LargeDocumentWire.info(bytes: bytes, encoding: encoding)
                let job = AtlasReaderDocumentJob(id: UUID(), info: info, generation: 1, inventory: nil, request: .prepare(width: 96))
                let valid = LargeDocumentWire.page("document-prepare", info: info, generation: 1, width: 96)
                _ = try job.decode(DocumentWireFixture.json(valid))
                var bad = valid; bad["document_generation"] = "2"
                rejects("Large source cannot weaken document generation checks") { _ = try job.decode(DocumentWireFixture.json(bad)) }
                bad = valid; bad["layout_complete"] = false
                rejects("A partial layout cannot masquerade as a completed logical preview") { _ = try job.decode(DocumentWireFixture.json(bad)) }
                bad = valid; var rows = bad["flow_lines"] as! [[String: Any]]
                rows[0]["enclosing_original_range"] = DocumentWireFixture.range(bytes - 2, bytes + 1); bad["flow_lines"] = rows
                rejects("The source ceiling cannot admit an out-of-capture anchor") { _ = try job.decode(DocumentWireFixture.json(bad)) }
                if encoding != "utf8" {
                    bad = valid; bad["parser_source_base"] = "3"
                    rejects("UTF16 preview must retain its two-byte BOM source base") { _ = try job.decode(DocumentWireFixture.json(bad)) }
                }
            }
        }
        let info = try LargeDocumentWire.info(bytes: 256 * 1024 + 1)
        let job = AtlasReaderDocumentJob(id: UUID(), info: info, generation: 1, inventory: nil, request: .prepare(width: 96))
        rejects("Wire admission retains the inclusive 256 KiB source ceiling") {
            _ = try job.decode(DocumentWireFixture.json(LargeDocumentWire.page("document-prepare", info: info, generation: 1, width: 96)))
        }
    }
}
