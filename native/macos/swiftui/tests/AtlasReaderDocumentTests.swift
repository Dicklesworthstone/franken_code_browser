// Portable production document admission/decoder tests. These fixed foreign
// responses test the host contract, not an imitation Markdown parser.
// swiftc -swift-version 6 -warnings-as-errors AtlasReader.swift \
//   AtlasReaderSearch.swift AtlasReaderOutline.swift AtlasReaderReferences.swift AtlasReaderDocument.swift \
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
    static func find(needle: String = "aa", first: UInt64 = 0, total: UInt64 = 3,
                     flowTotal: UInt64 = 3, rendered: UInt64? = nil) -> [String: Any] {
        var out = page(command: "document-find", total: flowTotal, headings: flowTotal == 0 ? 0 : 2)
        for key in ["first_flow_line", "flow_lines", "next_flow_line", "whole_document_visible", "headings", "next_heading"] {
            out.removeValue(forKey: key)
        }
        let bytes = rendered ?? flowTotal * 20, end = min(first + 128, total)
        out["rendered_utf8_bytes"] = String(bytes)
        out["needle"] = needle; out["search_domain"] = "rendered-text-utf8"
        out["case_sensitive"] = true; out["normalization"] = "exact"; out["overlapping"] = true
        out["max_scan_bytes"] = "16777216"; out["scanned_rendered_bytes"] = String(bytes)
        out["first_match"] = String(first); out["matches_seen"] = String(total)
        out["match_count_complete"] = true
        out["next_match"] = end < total ? String(end) as Any : NSNull()
        out["matches"] = (first..<end).map { index in
            ["match_id": String(index + 1),
             "rendered_utf8_range": range(index, index + UInt64(needle.utf8.count)),
             "first_flow_line": String(flowTotal == 0 ? 0 : min(index / 20, flowTotal - 1))] as [String: Any]
        }
        return out
    }
    static func linkRow(_ id: UInt64) -> [String: Any] {
        let kind: String, destination: String, fragment: Any
        switch id {
        case 2: kind = "same-document-start"; destination = "#"; fragment = NSNull()
        case 3: kind = "local-reference"; destination = "src/main.rs"; fragment = NSNull()
        case 4: kind = "external"; destination = "https://example.invalid"; fragment = NSNull()
        case 5: kind = "unsupported"; destination = "javascript:alert(1)"; fragment = NSNull()
        default: kind = "same-document-heading"; destination = "#caf%C3%A9"; fragment = "café"
        }
        return ["link_id": String(id), "block_index": "0", "evidence_range": range(0, UInt64(raw.count)),
            "destination": destination, "destination_display": destination, "kind": kind, "fragment_slug": fragment]
    }
    static func links(command: String = "document-links-prepare", first: UInt64 = 0,
                      total: UInt64 = 70) -> [String: Any] {
        var out = page(command: command)
        for key in ["first_flow_line", "flow_lines", "next_flow_line", "whole_document_visible", "headings", "next_heading"] {
            out.removeValue(forKey: key)
        }
        out["links_ready"] = true; out["links_complete"] = true
        out["links_seen"] = String(total); out["retained_links"] = String(total)
        out["links_truncated"] = false; out["parser_diagnostics"] = "0"
        out["evidence_precision"] = "whole-captured-markdown-document"
        out["first_link"] = String(first)
        let end = min(first + 64, total)
        out["links"] = (first..<end).map { linkRow($0 + 1) }
        out["next_link"] = end < total ? String(end) as Any : NSNull()
        return out
    }
    static func activation(_ target: AtlasReaderDocumentLinkTarget) -> [String: Any] {
        var out = links(command: "document-link")
        for key in ["first_link", "links", "next_link"] { out.removeValue(forKey: key) }
        out["link"] = linkRow(target.link.id)
        out["activated"] = target.link.kind.navigatesWithinCapture
        if target.link.kind.navigatesWithinCapture {
            let flow = page(command: "document-link")
            for key in ["first_flow_line", "flow_lines", "next_flow_line", "whole_document_visible"] { out[key] = flow[key] }
            if target.link.kind == .heading {
                out["heading_slug"] = "café"; out["heading_original_range"] = range(3, 11)
            }
        }
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
        try documentLinks()
        try documentFind()
        print("PASS: \(checks) document decoding, paging, copy domains, source mapping and stale-authority checks")
    }


    @MainActor static func documentFind() throws {
        let info = try AtlasReaderInfo.decode(Fixture.json(Fixture.header("info")), handle: 7, path: Fixture.path)
        let state = AtlasReaderDocumentState()
        let prepare = try state.admit(.prepare(width: 100), info: info)
        try state.accept(try prepare.decode(Fixture.json(Fixture.page(total: 8))), job: prepare)
        let source = state.page!, row = source.target(at: 0)!
        for needle in ["", "\0", String(repeating: "é", count: 513)] {
            rejects("unbounded or NUL Find input") { _ = try state.admit(.find(needle: needle, first: 0), info: info) }
        }
        rejects("pagination without an accepted search") { _ = try state.admit(.find(needle: "aa", first: 1), info: info) }
        let job = try state.admit(.find(needle: "aa", first: 0), info: info)
        let wire = Fixture.find(total: 130, flowTotal: 8)
        let result = try job.decode(Fixture.json(wire))
        try state.accept(result, job: job)
        let matches = state.matches!, target = matches.target(id: 1)!
        check(matches.rows.count == 128 && matches.total == 130 && matches.next == 128,
              "Find exposes bounded rows and a complete count")
        check(matches.rows[0].rendered.end > matches.rows[1].rendered.start, "Overlapping matches were rejected")
        check(state.page?.id == source.id && state.page?.target(at: 0) == row, "Find replaced source row authority")
        let exact = try state.admit(.find(needle: "Café", first: 0), info: info)
        var equivalent = Fixture.find(needle: "Café", total: 1, flowTotal: 8)
        equivalent["needle"] = "Cafe\u{301}"
        rejects("Unicode-normalized query echo") { _ = try exact.decode(Fixture.json(equivalent)) }
        rejects("new query retained an old match token") { _ = try state.admit(.findMatch(target), info: info) }
        let reload = try state.admit(.find(needle: "aa", first: 0), info: info)
        try state.accept(try reload.decode(Fixture.json(wire)), job: reload)
        let current = state.matches!.target(id: 1)!
        check(current != target, "A new Find page reused its authority UUID")
        let activation = try state.admit(.findMatch(current), info: info)
        // A newline separator may lie before the selected context row; no
        // fabricated range intersection or glyph/source highlight is required.
        var separator = wire, matchRows = separator["matches"] as! [[String: Any]]
        matchRows[0]["rendered_utf8_range"] = Fixture.range(19, 21)
        matchRows = [matchRows[0]]
        matchRows[0]["first_flow_line"] = "1"
        separator["matches"] = matchRows; separator["matches_seen"] = "1"; separator["next_match"] = NSNull()
        // This distinct query spelling starts a fresh count invariant.
        let fresh = try state.admit(.find(needle: "\n\n", first: 0), info: info)
        separator["needle"] = "\n\n"
        try state.accept(try fresh.decode(Fixture.json(separator)), job: fresh)
        let separatorTarget = state.matches!.target(id: 1)!
        let separatorOpen = try state.admit(.findMatch(separatorTarget), info: info)
        let context = Fixture.page(command: "document-window", first: 1, total: 8)
        try state.accept(try separatorOpen.decode(Fixture.json(context)), job: separatorOpen)
        check(state.page?.first == 1 && separatorTarget.match.rendered.start < state.page!.lines[0].rendered.start,
              "Separator match did not open its next context row")
        rejects("superseded activation was published") {
            try state.accept(try activation.decode(Fixture.json(Fixture.page(command: "document-window", total: 8))), job: activation)
        }
        for (key, value) in [
            ("needle", "aA"), ("first_match", "1"), ("matches_seen", "00130"),
            ("max_scan_bytes", "16777215"), ("scanned_rendered_bytes", "159"),
            ("search_domain", "original-source"), ("normalization", "nfc"),
            ("source_revision", "2"), ("document_generation", "2")
        ] {
            var bad = wire; bad[key] = value
            rejects("Find accepted invalid \(key)") { _ = try job.decode(Fixture.json(bad)) }
        }
        for key in ["case_sensitive", "overlapping", "match_count_complete"] {
            var bad = wire; bad[key] = false
            rejects("Find weakened \(key)") { _ = try job.decode(Fixture.json(bad)) }
        }
        var bad = wire; bad.removeValue(forKey: "next_match")
        rejects("Find omitted continuation boundary") { _ = try job.decode(Fixture.json(bad)) }
        bad = wire; bad["next_match"] = NSNull()
        rejects("Find claimed a premature terminal page") { _ = try job.decode(Fixture.json(bad)) }
        bad = wire; bad["matches_seen"] = "161"
        rejects("Find count exceeded possible byte positions") { _ = try job.decode(Fixture.json(bad)) }
        let rows = wire["matches"] as! [[String: Any]]
        for changedRows in [Array(rows.dropLast()), rows + [rows[0]]] {
            bad = wire; bad["matches"] = changedRows
            rejects("Find admitted a truncated or unbounded page") { _ = try job.decode(Fixture.json(bad)) }
        }
        for (key, value) in [("match_id", "2"), ("first_flow_line", "8")] {
            bad = wire; var invalid = rows; invalid[0][key] = value; bad["matches"] = invalid
            rejects("Find admitted invalid match \(key)") { _ = try job.decode(Fixture.json(bad)) }
        }
        for range in [Fixture.range(0, 1), Fixture.range(159, 161)] {
            bad = wire; var invalid = rows; invalid[0]["rendered_utf8_range"] = range; bad["matches"] = invalid
            rejects("Find admitted a wrong-length or out-of-bounds match") { _ = try job.decode(Fixture.json(bad)) }
        }
        bad = wire; var repeated = rows; repeated[1]["rendered_utf8_range"] = Fixture.range(0, 2); bad["matches"] = repeated
        rejects("Find admitted repeated match offsets") { _ = try job.decode(Fixture.json(bad)) }
        let first = try state.admit(.find(needle: "aa", first: 0), info: info)
        try state.accept(try first.decode(Fixture.json(wire)), job: first)
        let next = try state.admit(.find(needle: "aa", first: 128), info: info)
        let final = Fixture.find(first: 128, total: 130, flowTotal: 8)
        try state.accept(try next.decode(Fixture.json(final)), job: next)
        check(state.matches?.rows.count == 2 && state.matches?.next == nil, "Find final page lost its true boundary")
        let changedCount = Fixture.find(first: 128, total: 129, flowTotal: 8)
        rejects("Find pagination changed the frozen total") { _ = try next.decode(Fixture.json(changedCount)) }
        let terminal = try state.admit(.find(needle: "aa", first: 130), info: info)
        try state.accept(try terminal.decode(Fixture.json(Fixture.find(first: 130, total: 130, flowTotal: 8))), job: terminal)
        check(state.matches?.rows.isEmpty == true && state.matches?.first == 130, "Terminal empty Find page was refused")
        rejects("Find admitted an ordinal past the complete count") {
            _ = try state.admit(.find(needle: "aa", first: 131), info: info)
        }
        let delayed = try state.admit(.find(needle: "aa", first: 0), info: info)
        let delayedResult = try delayed.decode(Fixture.json(wire))
        _ = try state.admit(.prepare(width: 80), info: info)
        check(state.matches == nil, "Reflow retained Find targets")
        rejects("Find completed after reflow") { try state.accept(delayedResult, job: delayed) }

        let empty = AtlasReaderDocumentState()
        let emptyPrepare = try empty.admit(.prepare(width: 100), info: info)
        var emptyWire = Fixture.page(total: 0, headings: 0); emptyWire["rendered_utf8_bytes"] = "2"
        try empty.accept(try emptyPrepare.decode(Fixture.json(emptyWire)), job: emptyPrepare)
        let emptyFind = try empty.admit(.find(needle: "\n", first: 0), info: info)
        try empty.accept(try emptyFind.decode(Fixture.json(Fixture.find(needle: "\n", total: 2, flowTotal: 0, rendered: 2))), job: emptyFind)
        let emptyTarget = empty.matches!.target(id: 1)!
        check(emptyTarget.match.firstLine == 0 && empty.matches?.total == 2, "Zero-flow separators lost their matches")
        let emptyOpen = try empty.admit(.findMatch(emptyTarget), info: info)
        emptyWire["command"] = "document-window"
        try empty.accept(try emptyOpen.decode(Fixture.json(emptyWire)), job: emptyOpen)
        check(empty.page?.lines.isEmpty == true && empty.page?.first == 0, "Zero-flow match invented a displayed row")
        empty.revoke()
        rejects("Closed document retained a Find target") { _ = try empty.admit(.findMatch(emptyTarget), info: info) }
    }

    @MainActor static func documentLinks() throws {
        let info = try AtlasReaderInfo.decode(Fixture.json(Fixture.header("info")), handle: 7, path: Fixture.path)
        let state = AtlasReaderDocumentState()
        rejects("links acquired an unprepared document") { _ = try state.admit(.prepareLinks, info: info) }
        let preparation = try state.admit(.prepare(width: 100), info: info)
        try state.accept(try preparation.decode(Fixture.json(Fixture.page())), job: preparation)
        let sourcePage = state.page!
        let job = try state.admit(.prepareLinks, info: info)
        let response = try job.decode(Fixture.json(Fixture.links()))
        try state.accept(response, job: job)
        let links = state.links!, target = links.target(id: 1)!
        check(links.rows.count == 64 && links.next == 64 && links.total == 70 && links.complete,
              "link inventory must page without claiming extraction is truncated")
        check(links.target(id: 6)?.link.destination == target.link.destination
              && links.target(id: 6)?.link.id != target.link.id, "equal destinations lost occurrence identity")
        check(target.link.evidence == AtlasDocumentByteRange(start: 0, end: info.identity.capturedBytes),
              "link evidence was fabricated as an inline range")
        check(state.page?.id == sourcePage.id, "link parsing replaced the displayed flow")
        for (key, value) in [("document_generation", "2"), ("retained_links", "4097"), ("links_seen", "69"),
            ("first_link", "1"), ("evidence_precision", "inline-range"), ("parser_diagnostics", "00")] {
            var invalid = Fixture.links(); invalid[key] = value
            rejects("invalid link summary \(key)") { _ = try job.decode(Fixture.json(invalid)) }
        }
        for (key, value) in [("link_id", "01"), ("block_index", "18446744073709551615"),
            ("fragment_slug", ""), ("kind", "guaranteed-local"), ("destination", String(repeating: "a", count: 4097))] {
            var invalid = Fixture.links(), rows = invalid["links"] as! [[String: Any]]
            rows[0][key] = value; invalid["links"] = rows
            rejects("invalid link row \(key)") { _ = try job.decode(Fixture.json(invalid)) }
        }
        var inline = Fixture.links(), rows = inline["links"] as! [[String: Any]]
        rows[0]["evidence_range"] = Fixture.range(3, 11); inline["links"] = rows
        rejects("invented inline evidence") { _ = try job.decode(Fixture.json(inline)) }
        var duplicate = Fixture.links(); rows = duplicate["links"] as! [[String: Any]]
        rows[1] = rows[0]; duplicate["links"] = rows
        rejects("duplicated occurrence ID") { _ = try job.decode(Fixture.json(duplicate)) }
        var oversized = Fixture.links(); rows = oversized["links"] as! [[String: Any]]
        rows.append(Fixture.linkRow(65)); oversized["links"] = rows
        rejects("oversized link page") { _ = try job.decode(Fixture.json(oversized)) }
        var partial = Fixture.links(); partial["links_seen"] = "71"; partial["links_truncated"] = true
        partial["links_complete"] = false; partial["parser_diagnostics"] = "1"
        if case .links(let values) = try job.decode(Fixture.json(partial)) {
            check(!values.complete && values.truncated && values.diagnostics == 1 && values.seen == 71,
                  "partial link coverage was lost")
        } else { preconditionFailure("links were not decoded") }
        var gapped = Fixture.links(total: 1)
        gapped["links"] = [Fixture.linkRow(2)]; gapped["links_seen"] = "2"
        gapped["links_truncated"] = true; gapped["links_complete"] = false
        if case .links(let values) = try job.decode(Fixture.json(gapped)) {
            check(values.first == 0 && values.rows.first?.id == 2 && values.total == 1 && values.next == nil,
                  "omitted oversized destination rebound occurrence ID to retained row offset")
        } else { preconditionFailure("gapped occurrences were not decoded") }
        var falseComplete = Fixture.links(); falseComplete["parser_diagnostics"] = "1"
        rejects("diagnostic inventory claimed complete") { _ = try job.decode(Fixture.json(falseComplete)) }
        let activation = try state.admit(.link(target), info: info)
        let active = try activation.decode(Fixture.json(Fixture.activation(target)))
        try state.accept(active, job: activation)
        check(state.page?.id != sourcePage.id && state.page?.inventory.matches(sourcePage.inventory) == true,
              "same-document link did not retain source identity")
        var changed = Fixture.activation(target), row = changed["link"] as! [String: Any]
        row["fragment_slug"] = "cafe\u{301}"; changed["link"] = row
        rejects("canonical-equivalent fragment substituted") { _ = try activation.decode(Fixture.json(changed)) }
        changed = Fixture.activation(target); changed["heading_slug"] = "other"
        rejects("activation resolved a different heading") { _ = try activation.decode(Fixture.json(changed)) }
        let accepted = state.page!
        for id: UInt64 in [3, 4, 5] {
            let inert = links.target(id: id)!, request = try state.admit(.link(inert), info: info)
            try state.accept(try request.decode(Fixture.json(Fixture.activation(inert))), job: request)
            check(state.page?.id == accepted.id, "inert destination changed the current document")
            var forged = Fixture.activation(inert); forged["activated"] = true
            rejects("local/external destination forged navigation") { _ = try request.decode(Fixture.json(forged)) }
            forged = Fixture.activation(inert); forged["flow_lines"] = []
            rejects("inert activation supplied a hidden replacement page") { _ = try request.decode(Fixture.json(forged)) }
        }
        let next = try state.admit(.links(first: 64), info: info)
        try state.accept(try next.decode(Fixture.json(Fixture.links(command: "document-links", first: 64))), job: next)
        check(state.links?.rows.count == 6 && state.links?.next == nil, "final link page was miscounted")
        rejects("old list row revived after pagination") { _ = try state.admit(.link(target), info: info) }
        let last = state.links!.target(id: 65)!
        _ = try state.admit(.prepareLinks, info: info)
        rejects("repreparation retained old link authority") { _ = try state.admit(.link(last), info: info) }
        let reload = try state.admit(.prepareLinks, info: info)
        try state.accept(try reload.decode(Fixture.json(Fixture.links())), job: reload)
        let delayed = try state.admit(.link(state.links!.target(id: 1)!), info: info)
        let delayedResponse = try delayed.decode(Fixture.json(Fixture.activation(target)))
        _ = try state.admit(.prepare(width: 80), info: info)
        check(state.links == nil, "reflow retained stale link inventory")
        rejects("delayed link published after reflow") { try state.accept(delayedResponse, job: delayed) }
    }
}
