// Bounded wire and occurrence-identity regression tests. These inject response
// data; Rust source extraction and physical native presentation are separate
// qualification routes. Link the ordinary portable reader dependency set plus
// AtlasReaderReferences.swift. No compiler/reference resolver is simulated.
import Foundation

private struct ReferenceFixture {
    let name: String
    let copies: UInt64
    let littleEndian: Bool
    let leadingLines: UInt64
    init(name: String = "café", copies: UInt64 = 2, littleEndian: Bool = true, leadingLines: UInt64 = 0) {
        self.name = name; self.copies = copies; self.littleEndian = littleEndian; self.leadingLines = leadingLines
    }
    var text: String { String(repeating: "\n", count: Int(leadingLines)) + String(repeating: name + "\n", count: Int(copies)) }
    var bytes: [UInt8] {
        (littleEndian ? [255, 254] : [254, 255]) + text.utf16.flatMap {
            littleEndian ? [UInt8($0 & 255), UInt8($0 >> 8)] : [UInt8($0 >> 8), UInt8($0 & 255)]
        }
    }
    var nameLength: UInt64 { UInt64(name.utf16.count * 2) }
    static func hex(_ bytes: some Sequence<UInt8>) -> String { bytes.map { String(format: "%02x", $0) }.joined() }
    static func json(_ value: [String: Any]) -> String {
        String(data: try! JSONSerialization.data(withJSONObject: value), encoding: .utf8)!
    }
    static func range(_ start: UInt64, _ end: UInt64) -> [String: String] {
        ["start": String(start), "end": String(end)]
    }
    func header(_ command: String) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": command,
         "owner": "7", "file_id": "1", "source_revision": "1", "captured_bytes": String(bytes.count),
         "encoding": littleEndian ? "utf16le" : "utf16be", "additional_source_bytes_read": "0",
         "native_presented": false, "capture_origin": "host-supplied",
         "path": ["encoding": "unix-bytes", "hex": Self.hex("source.rs".utf8), "display": "source.rs"]]
    }
    func info() throws -> AtlasReaderInfo {
        try AtlasReaderInfo.decode(Self.json(header("info")), handle: 7, path: "source.rs")
    }
    func row(_ id: UInt64) -> [String: Any] {
        let start = 2 + leadingLines * 2 + (id - 1) * (nameLength + 2)
        return ["reference_id": String(id), "original_range": Self.range(start, start + nameLength),
            "line": String(leadingLines + id)]
    }
    func page(_ request: AtlasReferencePageRequest = .initial, preparing: Bool = true,
        cap: UInt64 = 4096, generation: UInt64 = 1) -> [String: Any] {
        var out = header(preparing ? "references" : "reference-page")
        let retained = min(copies, cap), limited = copies > cap
        let end = min(retained, request.start + request.limit)
        out["reference_generation"] = String(generation); out["name"] = name; out["name_case"] = "sensitive"
        out["selection_namespace"] = "reference"; out["evidence_level"] = "whole-token-text-candidate"
        out["includes_comments_and_literals"] = true; out["compiler_resolved"] = false; out["semantic_complete"] = false
        out["search_complete"] = !limited; out["count_complete"] = !limited; out["output_limited"] = limited
        out["retained_references"] = String(retained); out["matches_counted"] = String(retained + (limited ? 1 : 0))
        out["line_semantics"] = "LF-counted-source-lines"; out["max_source_bytes"] = "524288"; out["max_items"] = String(cap)
        out["references"] = (request.start..<end).map { row($0 + 1) }
        out["next_offset"] = end < retained ? String(end) as Any : NSNull()
        return out
    }
}

@main struct AtlasReaderReferencesTests {
    private static func reject(_ reason: String, _ work: () throws -> Void) {
        do { try work(); fatalError(reason) } catch { }
    }
    static func main() throws {
        let fixture = ReferenceFixture(), info = try fixture.info()
        func decode(_ wire: [String: Any], cap: UInt64 = 4096) throws -> AtlasReferencePage {
            try AtlasReferencePage.decode(ReferenceFixture.json(wire), info: info, generation: 1,
                name: fixture.name, request: .initial, preparing: true, maxItems: cap)
        }
        let page = try decode(fixture.page())
        precondition(page.rows.count == 2 && page.inventory.matchesCounted == 2 && !page.inventory.limited)
        precondition(page.rows[0].range == AtlasOutlineRange(start: 2, end: 10))
        precondition(page.rows[1].range == AtlasOutlineRange(start: 12, end: 20) && page.rows[1].line == 2)
        precondition(page.target(id: 0) == nil && page.target(id: 3) == nil)
        precondition(page.summary.contains("comments") && page.summary.contains("not compiler references"))
        let rebuilt = try decode(fixture.page())
        precondition(page.inventory.matches(rebuilt.inventory) && page.target(id: 1) != rebuilt.target(id: 1),
            "page token must reject old UI events even when source and generation match")

        for (key, value) in [
            ("owner", "8"), ("file_id", "2"), ("source_revision", "2"),
            ("reference_generation", "01"), ("reference_generation", "2"),
            ("command", "symbols"), ("selection_namespace", "outline"),
            ("evidence_level", "compiler-resolved"), ("name_case", "insensitive"),
            ("line_semantics", "native-wrapped-lines"), ("max_source_bytes", "524289"),
            ("max_items", "4095"), ("retained_references", "3"), ("matches_counted", "3"),
            ("next_offset", "2"), ("additional_source_bytes_read", "1"), ("encoding", "unsupported")
        ] {
            var bad = fixture.page(); bad[key] = value
            reject("invalid occurrence header: \(key)") { _ = try decode(bad) }
        }
        for (key, value) in [("includes_comments_and_literals", false), ("compiler_resolved", true),
            ("semantic_complete", true), ("search_complete", false), ("count_complete", false),
            ("output_limited", true), ("native_presented", true)] {
            var bad = fixture.page(); bad[key] = value
            reject("invalid occurrence evidence: \(key)") { _ = try decode(bad) }
        }
        var missingContinuation = fixture.page(); missingContinuation.removeValue(forKey: "next_offset")
        reject("missing terminal page marker") { _ = try decode(missingContinuation) }
        var short = fixture.page(); short["references"] = [fixture.row(1)]
        reject("short page cannot silently drop an occurrence") { _ = try decode(short) }
        for (key, value) in [("reference_id", "0"), ("reference_id", "2"), ("line", "0"), ("line", "999999")] {
            var bad = fixture.page(); var rows = bad["references"] as! [[String: Any]]
            rows[0][key] = value; bad["references"] = rows
            reject("invalid occurrence row: \(key)") { _ = try decode(bad) }
        }
        for range in [ReferenceFixture.range(0, 8), ReferenceFixture.range(3, 11), ReferenceFixture.range(2, 2),
                      ReferenceFixture.range(2, 8), ReferenceFixture.range(2, 12),
                      ReferenceFixture.range(2, 21), ReferenceFixture.range(20, 24)] {
            var bad = fixture.page(); var rows = bad["references"] as! [[String: Any]]
            rows[0]["original_range"] = range; bad["references"] = rows
            reject("invalid UTF-16 occurrence range") { _ = try decode(bad) }
        }
        var overlap = fixture.page(); var rows = overlap["references"] as! [[String: Any]]
        rows[1]["original_range"] = ReferenceFixture.range(8, 16); overlap["references"] = rows
        reject("overlapping whole tokens") { _ = try decode(overlap) }
        var duplicate = fixture.page(); duplicate["references"] = [fixture.row(1), fixture.row(1)]
        reject("duplicate local ID") { _ = try decode(duplicate) }
        var excessive = fixture.page(); excessive["references"] = Array(repeating: fixture.row(1), count: 129)
        reject("oversized decoder array") { _ = try decode(excessive) }

        let limited = try decode(fixture.page(cap: 1), cap: 1)
        precondition(limited.inventory.limited && limited.inventory.matchesCounted == 2 && limited.rows.count == 1)
        precondition(limited.summary.contains("At least 2") && limited.summary.contains("More may exist"))
        let zero = try decode(fixture.page(cap: 0), cap: 0)
        precondition(zero.inventory.limited && zero.rows.isEmpty && zero.inventory.matchesCounted == 1)
        var falseComplete = fixture.page(cap: 1); falseComplete["search_complete"] = true
        reject("limited inventory mislabeled complete") { _ = try decode(falseComplete, cap: 1) }
        var falseCount = fixture.page(cap: 1); falseCount["matches_counted"] = "1"
        reject("lookahead omitted") { _ = try decode(falseCount, cap: 1) }
        let emptyFixture = ReferenceFixture(copies: 0), emptyInfo = try emptyFixture.info()
        let empty = try AtlasReferencePage.decode(ReferenceFixture.json(emptyFixture.page()), info: emptyInfo,
            generation: 1, name: emptyFixture.name, request: .initial, preparing: true)
        precondition(empty.rows.isEmpty && !empty.inventory.limited && empty.inventory.matchesCounted == 0)

        let many = ReferenceFixture(copies: 130), manyInfo = try many.info()
        let first = try AtlasReferencePage.decode(ReferenceFixture.json(many.page()), info: manyInfo,
            generation: 1, name: many.name, request: .initial, preparing: true)
        precondition(first.rows.count == 64 && first.nextOffset == 64)
        let nextRequest = AtlasReferencePageRequest(start: 64, limit: 128)
        let next = try AtlasReferencePage.decode(ReferenceFixture.json(many.page(nextRequest, preparing: false)),
            info: manyInfo, generation: 1, name: many.name, request: nextRequest, inventory: first.inventory)
        precondition(next.rows.first?.id == 65 && next.rows.count == 66 && next.nextOffset == nil)
        precondition(next.target(id: 1) == nil && next.target(id: 65)?.occurrence.range.start == 642)
        var stranded = many.page(); stranded["next_offset"] = NSNull()
        reject("nonterminal page without continuation") {
            _ = try AtlasReferencePage.decode(ReferenceFixture.json(stranded), info: manyInfo,
                generation: 1, name: many.name, request: .initial, preparing: true)
        }
        var changed = many.page(nextRequest, preparing: false)
        changed["retained_references"] = "129"; changed["matches_counted"] = "129"
        changed["references"] = (65...129).map { many.row(UInt64($0)) }
        reject("retained inventory changed between pages") {
            _ = try AtlasReferencePage.decode(ReferenceFixture.json(changed), info: manyInfo,
                generation: 1, name: many.name, request: nextRequest, inventory: first.inventory)
        }

        for endian in [true, false] {
            let unicode = ReferenceFixture(name: "𐐀", littleEndian: endian), unicodeInfo = try unicode.info()
            let unicodePage = try AtlasReferencePage.decode(ReferenceFixture.json(unicode.page()), info: unicodeInfo,
                generation: 1, name: unicode.name, request: .initial, preparing: true)
            precondition(unicodePage.rows[0].range == AtlasOutlineRange(start: 2, end: 6))
        }
        let late = ReferenceFixture(leadingLines: 5_000), lateInfo = try late.info()
        let latePage = try AtlasReferencePage.decode(ReferenceFixture.json(late.page()), info: lateInfo,
            generation: 1, name: late.name, request: .initial, preparing: true)
        precondition(latePage.rows[0].line == 5_001 && latePage.rows[0].range.start == 10_002,
            "LF source lines are bounded by source bytes, not the retained occurrence cap")
        var utf8Header = fixture.header("info")
        utf8Header["captured_bytes"] = String(fixture.text.utf8.count); utf8Header["encoding"] = "utf8"
        let utf8Info = try AtlasReaderInfo.decode(ReferenceFixture.json(utf8Header), handle: 7, path: "source.rs")
        var utf8Page = fixture.page()
        utf8Page["captured_bytes"] = String(fixture.text.utf8.count); utf8Page["encoding"] = "utf8"
        utf8Page["references"] = [
            ["reference_id": "1", "line": "1", "original_range": ReferenceFixture.range(0, 5)],
            ["reference_id": "2", "line": "2", "original_range": ReferenceFixture.range(6, 11)]
        ] as [[String: Any]]
        let utf8 = try AtlasReferencePage.decode(ReferenceFixture.json(utf8Page), info: utf8Info,
            generation: 1, name: fixture.name, request: .initial, preparing: true)
        precondition(utf8.rows[0].range == AtlasOutlineRange(start: 0, end: 5))
        for width in [UInt64(4), UInt64(6)] {
            var wrong = utf8Page, rows = wrong["references"] as! [[String: Any]]
            rows[0]["original_range"] = ReferenceFixture.range(0, width); wrong["references"] = rows
            reject("UTF-8 token length differs from exact name bytes") {
                _ = try AtlasReferencePage.decode(ReferenceFixture.json(wrong), info: utf8Info,
                    generation: 1, name: fixture.name, request: .initial, preparing: true)
            }
        }
        let accent = ReferenceFixture(name: "cafe\u{301}"), accentInfo = try accent.info()
        reject("canonical String equivalence changed exact query spelling") {
            _ = try AtlasReferencePage.decode(ReferenceFixture.json(accent.page()), info: accentInfo,
                generation: 1, name: "café", request: .initial, preparing: true)
        }
        for invalid in ["", "x\0y", String(repeating: "x", count: 1025)] {
            reject("invalid reference marshaling name") { try AtlasReferencePage.validateName(invalid) }
        }
        for invalid in [AtlasReferencePageRequest(start: 4097, limit: 64),
            .init(start: 0, limit: 0), .init(start: 0, limit: 129)] {
            reject("invalid reference page request") { try invalid.validate() }
        }

        var retainedReceipt = fixture.header("info")
        retainedReceipt["capture_sha256"] = String(repeating: "0", count: 64)
        retainedReceipt["initial_source_bytes_read"] = "0"; retainedReceipt["initial_read_calls"] = "0"
        retainedReceipt["accepted_query_generation"] = NSNull(); retainedReceipt["accepted_outline_generation"] = NSNull()
        retainedReceipt["accepted_document_generation"] = NSNull(); retainedReceipt["accepted_reference_generation"] = NSNull()
        _ = try AtlasReaderInfo.decodeRetainedInitialization(ReferenceFixture.json(retainedReceipt),
            handle: 7, pathBytes: Array("source.rs".utf8))
        retainedReceipt["accepted_reference_generation"] = "1"
        reject("capture-only owner inherited occurrence authority") {
            _ = try AtlasReaderInfo.decodeRetainedInitialization(ReferenceFixture.json(retainedReceipt),
                handle: 7, pathBytes: Array("source.rs".utf8))
        }
        retainedReceipt.removeValue(forKey: "accepted_reference_generation")
        _ = try AtlasReaderInfo.decodeRetainedInitialization(ReferenceFixture.json(retainedReceipt),
            handle: 7, pathBytes: Array("source.rs".utf8))
        print("Occurrence wire, exact spelling, source ranges, paging and evidence regressions passed.")
    }
}
