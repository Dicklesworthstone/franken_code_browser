// Portable production document admission/decoder tests. These fixed foreign
// responses test the host contract, not an imitation Markdown parser.
// swiftc -swift-version 6 -warnings-as-errors AtlasReader.swift \
//   AtlasReaderSearch.swift AtlasReaderOutline.swift AtlasReaderDocument.swift \
//   tests/AtlasReaderDocumentTests.swift -o /tmp/fcb-document-tests
import Foundation

private enum Fixture {
    static let path = "/project/README.md"
    static let source = "\u{feff}# Café\n\nA **bold** line.\n\n## Next\n\nTail.\n"
    static let raw = Array(source.utf8)
    static let paragraph: AtlasDocumentByteRange = {
        let bytes = Array("A **bold** line.\n".utf8)
        let start = raw.firstIndex(of: 65)!
        return .init(start: UInt64(start), end: UInt64(start + bytes.count))
    }()
    static func hex(_ bytes: some Sequence<UInt8>) -> String { bytes.map { String(format: "%02x", $0) }.joined() }
    static func json(_ data: [String: Any]) -> String { String(data: try! JSONSerialization.data(withJSONObject: data), encoding: .utf8)! }
    static func range(_ start: UInt64, _ end: UInt64) -> [String: String] { ["start": String(start), "end": String(end)] }
    static func header(_ command: String, owner: UInt64 = 7) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": command, "owner": String(owner),
         "file_id": "1", "source_revision": "1", "captured_bytes": String(raw.count), "encoding": "utf8",
         "additional_source_bytes_read": "0", "native_presented": false, "capture_origin": "host-supplied",
         "path": ["encoding": "unix-bytes", "hex": hex(path.utf8), "display": path]]
    }
    static func page(command: String = "document-prepare", owner: UInt64 = 7, generation: UInt64 = 1,
        width: UInt64 = 100, first: UInt64 = 0, total: UInt64 = 3, headingFirst: UInt64 = 0,
        headings: UInt64 = 2) -> [String: Any] {
        var out = header(command, owner: owner)
        out["document_generation"] = String(generation); out["document_ready"] = true
        out["layout_complete"] = true; out["rendering"] = "logical-frankenmarkdown-flow"
        out["native_shaped"] = false; out["source_mapping"] = "enclosing-regions-not-glyph-exact"
        out["width_columns"] = String(width); out["total_flow_lines"] = String(total)
        out["total_headings"] = String(headings); out["rendered_utf8_bytes"] = String(total * 20)
        out["parser_source_base"] = "3"; out["first_flow_line"] = String(first)
        let end = min(first + 64, total)
        let flow: [[String: Any]] = (first..<end).map { index -> [String: Any] in
            let start: UInt64 = index * 20
            return ["flow_line": String(index), "text": index == 0 ? "Café" : "A bold line.",
             "rendered_utf8_range": range(start, start + 16),
             "enclosing_original_range": range(paragraph.start, paragraph.end)] as [String: Any]
        }
        out["flow_lines"] = flow
        out["next_flow_line"] = end < total ? String(end) as Any : NSNull()
        out["whole_document_visible"] = first == 0 && end == total
        let headingEnd = min(headingFirst + 64, headings)
        out["headings"] = (headingFirst..<headingEnd).map { index in
            ["slug": index == 0 ? "café" : "heading-\(index)", "title": index == 0 ? "Café" : "Next",
             "rendered_utf8_offset": "0", "original_range": range(3, 11)] as [String: Any]
        }
        out["next_heading"] = headingEnd < headings ? String(headingEnd) as Any : NSNull()
        return out
    }
    static func selection(_ target: AtlasReaderDocumentTarget) -> [String: Any] {
        ["document_generation": String(target.inventory.generation), "selection_namespace": "document",
         "source_mapping": "enclosing-regions-not-glyph-exact",
         "rendered_utf8_range": range(target.rendered.start, target.rendered.end)]
    }
    static func copy(_ target: AtlasReaderDocumentTarget, mode: AtlasReaderDocumentCopyMode) -> [String: Any] {
        var out = header("document-copy", owner: target.inventory.identity.owner)
        out.merge(selection(target)) { _, new in new }
        out["enclosing_original_range"] = range(paragraph.start, paragraph.end)
        if mode == .renderedText { out["copy_domain"] = "rendered-text-utf8"; out["text"] = target.line.text }
        else {
            out["copy_domain"] = "enclosing-original-markdown"
            out["original_hex"] = hex(raw[Int(paragraph.start)..<Int(paragraph.end)])
        }
        return out
    }
    static func sourcePage(_ target: AtlasReaderDocumentTarget) -> [String: Any] {
        var out = header("document-source", owner: target.inventory.identity.owner)
        out["text_kind"] = "logical-captured-text-not-shaped"
        out["requested_original_range"] = range(0, UInt64(raw.count)); out["visible_range"] = range(0, UInt64(raw.count))
        out["range_limited"] = false; out["boundaries_adjusted"] = false; out["has_replacements"] = false
        out["first_physical_line"] = NSNull(); out["next_offset"] = NSNull()
        out["text"] = String(decoding: raw.dropFirst(3), as: UTF8.self); out["original_hex"] = hex(raw)
        var selected = selection(target)
        selected["original_range"] = range(paragraph.start, paragraph.end)
        selected["window_utf8_range"] = range(paragraph.start - 3, paragraph.end - 3)
        selected["original_hex"] = hex(raw[Int(paragraph.start)..<Int(paragraph.end)])
        out["selection"] = selected
        return out
    }
}

@main struct AtlasReaderDocumentTests {
    @MainActor static var checks = 0
    @MainActor static func check(_ value: @autoclosure () -> Bool, _ reason: String) {
        checks += 1; precondition(value(), reason)
    }
    @MainActor static func rejects(_ reason: String, _ body: () throws -> Void) {
        do { try body(); fatalError(reason) } catch { checks += 1 }
    }
    @MainActor static func main() throws {
        let info = try AtlasReaderInfo.decode(Fixture.json(Fixture.header("info")), handle: 7, path: Fixture.path)
        let state = AtlasReaderDocumentState()
        let prepare = try state.admit(.prepare(width: 100), info: info)
        let prepared = try prepare.decode(Fixture.json(Fixture.page()))
        try state.accept(prepared, job: prepare)
        let page = state.page!, headings = state.headings!
        check(page.lines.count == 3 && page.inventory.sourceBase == 3 && page.wholeDocumentVisible, "BOM preview admission")
        let target = page.target(at: 1)!
        check(target.rendered.start == 20 && target.rendered.end == 32, "copy included layout padding")
        check(page.target(at: 8) == nil, "out-of-page row authority")
        check(headings.target(slug: "café") != nil && headings.target(slug: "cafe\u{301}") == nil, "canonical-equivalent slug alias")
        for (key, value) in [("owner", "8"), ("source_revision", "2"), ("document_generation", "01"),
            ("width_columns", "101"), ("total_flow_lines", "8193"), ("total_headings", "4097"),
            ("rendered_utf8_bytes", "18446744073709551615"), ("parser_source_base", "2"),
            ("rendering", "native-glyphs"), ("source_mapping", "glyph-exact"), ("encoding", "utf16le"),
            ("additional_source_bytes_read", "1"), ("command", "document-window"), ("first_flow_line", "1"),
            ("next_flow_line", "3"), ("next_heading", "2")] {
            var bad = Fixture.page(); bad[key] = value
            rejects("invalid document field \(key)") { _ = try prepare.decode(Fixture.json(bad)) }
        }
        for (key, value) in [("document_ready", false), ("layout_complete", false), ("native_shaped", true),
                             ("native_presented", true), ("whole_document_visible", false)] {
            var bad = Fixture.page(); bad[key] = value
            rejects("false completeness contract \(key)") { _ = try prepare.decode(Fixture.json(bad)) }
        }
        for (key, value) in [("flow_line", "1"), ("text", String(repeating: "x", count: 17))] {
            var bad = Fixture.page(); var rows = bad["flow_lines"] as! [[String: Any]]
            rows[0][key] = value; bad["flow_lines"] = rows
            rejects("invalid flow row \(key)") { _ = try prepare.decode(Fixture.json(bad)) }
        }
        for key in ["flow_lines", "headings"] {
            var bad = Fixture.page(); let rows = bad[key] as! [[String: Any]]
            bad[key] = Array(repeating: rows[0], count: 65)
            rejects("unbounded page array \(key)") { _ = try prepare.decode(Fixture.json(bad)) }
            bad[key] = []
            rejects("short page silently truncated \(key)") { _ = try prepare.decode(Fixture.json(bad)) }
        }
        var wrongSpan = Fixture.page(); var wrongRows = wrongSpan["flow_lines"] as! [[String: Any]]
        wrongRows[0]["enclosing_original_range"] = Fixture.range(0, 2); wrongSpan["flow_lines"] = wrongRows
        rejects("BOM gained source mapping") { _ = try prepare.decode(Fixture.json(wrongSpan)) }
        var duplicate = Fixture.page(); let headingRows = duplicate["headings"] as! [[String: Any]]
        duplicate["headings"] = [headingRows[0], headingRows[0]]
        rejects("duplicate heading slugs") { _ = try prepare.decode(Fixture.json(duplicate)) }
        var emptyRow = Fixture.page(); var rows = emptyRow["flow_lines"] as! [[String: Any]]
        rows[0]["text"] = ""; rows[0]["enclosing_original_range"] = NSNull(); emptyRow["flow_lines"] = rows
        if case .prepared(let empty, _) = try prepare.decode(Fixture.json(emptyRow)) {
            check(empty.target(at: 0) == nil, "synthetic row acquired source/copy authority")
        } else { fatalError("wrong result") }
        for mode in [AtlasReaderDocumentCopyMode.renderedText, .enclosingMarkdown] {
            let job = try state.admit(.copy(target, mode), info: info)
            let response = try job.decode(Fixture.json(Fixture.copy(target, mode: mode)))
            try state.accept(response, job: job)
            if case .copy(let copied) = response {
                check(copied.mode == mode && copied.target == target, "copy domain or row lost")
                check(copied.enclosingOriginal == Fixture.paragraph, "enclosing source mapping lost")
            }
            var wrong = Fixture.copy(target, mode: mode); wrong["query_generation"] = "1"
            rejects("document used search namespace") { _ = try job.decode(Fixture.json(wrong)) }
            wrong = Fixture.copy(target, mode: mode); wrong["rendered_utf8_range"] = Fixture.range(20, 36)
            rejects("copy padding promoted into selected bytes") { _ = try job.decode(Fixture.json(wrong)) }
            wrong = Fixture.copy(target, mode: mode); wrong["copy_domain"] = "source"
            rejects("copy domain conflation") { _ = try job.decode(Fixture.json(wrong)) }
            wrong = Fixture.copy(target, mode: mode)
            wrong[mode == .renderedText ? "text" : "original_hex"] = "wrong"
            rejects("copy evidence mismatch") { _ = try job.decode(Fixture.json(wrong)) }
        }
        let sourceJob = try state.admit(.source(target), info: info)
        let sourceResult = try sourceJob.decode(Fixture.json(Fixture.sourcePage(target)))
        try state.accept(sourceResult, job: sourceJob)
        if case .source(let source) = sourceResult {
            check(source.enclosingOriginal == Fixture.paragraph && source.selectionUTF8.start == Fixture.paragraph.start - 3,
                "original/rendered/window domains mixed")
            check(source.selectedHex == Fixture.hex(Fixture.raw[Int(Fixture.paragraph.start)..<Int(Fixture.paragraph.end)]), "source copy bytes lost")
        }
        for (key, value) in [("document_generation", "2"), ("selection_namespace", "outline"),
            ("source_mapping", "glyph-exact"), ("outline_generation", "1"), ("symbol_id", "1"),
            ("original_hex", String(repeating: "00", count: Int(Fixture.paragraph.length)))] {
            var bad = Fixture.sourcePage(target); var selected = bad["selection"] as! [String: Any]
            selected[key] = value; bad["selection"] = selected
            rejects("source selection mismatch \(key)") { _ = try sourceJob.decode(Fixture.json(bad)) }
        }
        var changed = Fixture.sourcePage(target); changed["text"] = Fixture.source.replacingOccurrences(of: "bold", with: "fake")
        rejects("changed source text attached to authentic raw bytes") { _ = try sourceJob.decode(Fixture.json(changed)) }
        var shifted = Fixture.sourcePage(target); var selected = shifted["selection"] as! [String: Any]
        selected["window_utf8_range"] = Fixture.range(Fixture.paragraph.start - 2, Fixture.paragraph.end - 2)
        shifted["selection"] = selected
        rejects("shifted source highlight") { _ = try sourceJob.decode(Fixture.json(shifted)) }
        for width: UInt64 in [0, 3, 513, UInt64.max] {
            rejects("invalid reflow input") { _ = try state.admit(.prepare(width: width), info: info) }
        }
        check(state.page?.id == page.id, "invalid input destroyed accepted document")
        rejects("out-of-bounds document page") { _ = try state.admit(.window(first: UInt64.max), info: info) }
        rejects("BOM source anchor") { _ = try state.admit(.fromSource(offset: 0), info: info) }
        let headingTarget = headings.target(slug: "café")!
        let headingJob = try state.admit(.heading(headingTarget), info: info)
        var atHeading = Fixture.page(command: "document-heading")
        atHeading["heading_slug"] = "café"; atHeading["heading_original_range"] = Fixture.range(3, 11)
        try state.accept(try headingJob.decode(Fixture.json(atHeading)), job: headingJob)
        rejects("old row token after navigation") { _ = try state.admit(.source(target), info: info) }
        atHeading["heading_slug"] = "cafe\u{301}"
        rejects("canonical slug normalization at activation") { _ = try headingJob.decode(Fixture.json(atHeading)) }
        let sourceAnchor = try state.admit(.fromSource(offset: Fixture.paragraph.start), info: info)
        var fromSource = Fixture.page(command: "document-from-source")
        fromSource["original_anchor"] = String(Fixture.paragraph.start)
        try state.accept(try sourceAnchor.decode(Fixture.json(fromSource)), job: sourceAnchor)
        fromSource["original_anchor"] = "0"
        rejects("source anchor substituted") { _ = try sourceAnchor.decode(Fixture.json(fromSource)) }

        // Reflow invalidates both row and heading authority BEFORE the worker
        // returns; a failed/canceled call cannot resurrect engine-stale state.
        let reflow = try state.admit(.prepare(width: 80), info: info)
        check(reflow.generation == 2 && state.page == nil && state.headings == nil, "reflow not revoked synchronously")
        rejects("old heading after reflow admission") { _ = try state.admit(.heading(headingTarget), info: info) }
        rejects("stale previous job delivered") { try state.accept(sourceResult, job: sourceJob) }
        state.abandon(reflow)
        let new = try state.admit(.prepare(width: 100), info: info)
        check(new.generation == 3, "failed attempt reused generation")
        let full = try new.decode(Fixture.json(Fixture.page(generation: 3, total: 130, headings: 130)))
        try state.accept(full, job: new)
        check(state.page?.next == 64 && state.headings?.next == 64, "first bounded document/heading pages")
        let oldPage = state.page!
        let oldHeadings = state.headings!
        let next = try state.admit(.window(first: 64), info: info)
        let nextResult = try next.decode(Fixture.json(Fixture.page(command: "document-window", generation: 3, first: 64, total: 130, headings: 130)))
        try state.accept(nextResult, job: next)
        check(state.page?.first == 64 && state.page?.lines.count == 64 && state.headings?.id == oldHeadings.id, "flow paging changed headings")
        rejects("old copied row rebound by index") { _ = try state.admit(.copy(oldPage.target(at: 1)!, .renderedText), info: info) }
        let heads = try state.admit(.headings(first: 64), info: info)
        let headsResult = try heads.decode(Fixture.json(Fixture.page(command: "document-headings", generation: 3, total: 130, headingFirst: 64, headings: 130)))
        try state.accept(headsResult, job: heads)
        check(state.headings?.first == 64 && state.headings?.rows.count == 64 && state.page?.first == 64, "heading paging changed source flow")
        rejects("old heading page rebound by slug") { _ = try state.admit(.heading(oldHeadings.target(slug: "café")!), info: info) }
        let eof = try state.admit(.window(first: 130), info: info)
        let eofResult = try eof.decode(Fixture.json(Fixture.page(command: "document-window", generation: 3, first: 130, total: 130, headings: 130)))
        try state.accept(eofResult, job: eof)
        check(state.page?.lines.isEmpty == true && state.page?.next == nil && state.page?.wholeDocumentVisible == false, "EOF became whole source visibility")
        let beforeClose = try state.admit(.window(first: 0), info: info)
        let delayed = try beforeClose.decode(Fixture.json(Fixture.page(command: "document-window", generation: 3, total: 130, headings: 130)))
        state.revoke()
        rejects("closed preview published delayed result") { try state.accept(delayed, job: beforeClose) }
        let reopened = try state.admit(.prepare(width: 100), info: info)
        check(reopened.generation == 4, "closing a preview reused its reader's generation")
        state.close()
        let fresh = try state.admit(.prepare(width: 100), info: info)
        check(fresh.generation == 1, "closed source session did not reset counter")
        rejects("nil became empty preview") { _ = try fresh.decode(nil) }
        rejects("engine error became empty preview") {
            _ = try fresh.decode(Fixture.json(["schema": "fcb.reader-session/1", "owner": "7", "status": "error",
                "error": ["code": "DOCUMENT_READ_SOURCE_LIMIT"]]))
        }
        print("PASS: \(checks) document decoding, paging, copy domains, source mapping and stale-authority checks")
    }
}
