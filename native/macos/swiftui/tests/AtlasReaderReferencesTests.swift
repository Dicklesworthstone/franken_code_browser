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
    func windowPage(_ window: AtlasReferenceWindowRequest,
        _ request: AtlasReferencePageRequest = .initial, preparing: Bool = true,
        cap: UInt64 = 4096, generation: UInt64 = 1) -> [String: Any] {
        var out = page(.initial, preparing: preparing, cap: cap, generation: generation)
        let skipped = min(copies, window.offset)
        let retained = min(cap, copies - skipped), more = copies - skipped > retained
        let counted = window.countAllMatches ? copies : skipped + retained + (more ? 1 : 0)
        let complete = window.countAllMatches || !more
        out["result_offset"] = String(window.offset); out["count_all_matches"] = window.countAllMatches
        out["skipped_matches"] = String(skipped); out["rows_truncated"] = counted > retained
        out["has_more_matches"] = more
        out["next_match_offset"] = more && retained > 0 ? String(window.offset + retained) as Any : NSNull()
        out["search_complete"] = complete; out["count_complete"] = complete; out["output_limited"] = more
        out["retained_references"] = String(retained); out["matches_counted"] = String(counted)
        let end = min(retained, request.start + request.limit)
        out["references"] = (request.start..<end).map { index -> [String: Any] in
            let id = index + 1, occurrence = window.offset + id
            var value = row(occurrence)
            value["reference_id"] = String(id); value["occurrence_id"] = String(occurrence)
            return value
        }
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
        try windowRegressions()
        print("Occurrence wire, exact spelling, source ranges, paging and evidence regressions passed.")
    }

    private static func windowRegressions() throws {
        for littleEndian in [true, false] {
            let fixture = ReferenceFixture(name: "𐐀", copies: 5_005, littleEndian: littleEndian, leadingLines: 2)
            let info = try fixture.info()
            let window = AtlasReferenceWindowRequest(offset: 4_096, countAllMatches: true)
            let first = try AtlasReferencePage.decode(ReferenceFixture.json(fixture.windowPage(window, cap: 128)),
                info: info, generation: 1, name: fixture.name, request: .initial, preparing: true,
                maxItems: 128, window: window)
            precondition(first.inventory.matchesCounted == 5_005 && first.inventory.retained == 128)
            precondition(first.inventory.limited && first.inventory.countComplete)
            precondition(first.inventory.window?.request == window && first.inventory.window?.skipped == 4_096)
            precondition(first.inventory.window?.hasMore == true && first.inventory.window?.rowsTruncated == true)
            precondition(first.inventory.window?.nextOffset == 4_224 && first.nextOffset == 64)
            precondition(first.rows.first?.id == 1 && first.rows.first?.occurrence == 4_097)
            precondition(first.rows.first?.ordinal == 4_097 && first.rows.first?.line == 4_099)
            precondition(first.rows.first?.range == AtlasOutlineRange(start: 24_582, end: 24_586))
            precondition(first.target(id: 4_097) == nil && first.target(id: 1)?.occurrence.ordinal == 4_097,
                "global ordinals must never acquire local activation authority")
            precondition(first.summary.contains("5005 whole-token occurrences")
                && first.summary.contains("4097–4224") && !first.summary.contains("At least"))
            let request = AtlasReferencePageRequest(start: 64, limit: 64)
            let later = try AtlasReferencePage.decode(
                ReferenceFixture.json(fixture.windowPage(window, request, preparing: false, cap: 128)),
                info: info, generation: 1, name: fixture.name, request: request, inventory: first.inventory, maxItems: 128)
            precondition(later.inventory.matches(first.inventory) && later.nextOffset == nil)
            precondition(later.inventory.window?.nextOffset == 4_224)
            precondition(later.rows.first?.id == 65 && later.rows.first?.occurrence == 4_161)
            precondition(later.rows.last?.id == 128 && later.rows.last?.occurrence == 4_224)
            precondition(later.rows.first?.range == AtlasOutlineRange(start: 24_966, end: 24_970))
        }

        let fixture = ReferenceFixture(copies: 5_005), info = try fixture.info()
        func decode(_ wire: [String: Any], _ window: AtlasReferenceWindowRequest,
            cap: UInt64 = 2) throws -> AtlasReferencePage {
            try AtlasReferencePage.decode(ReferenceFixture.json(wire), info: info, generation: 1,
                name: fixture.name, request: .initial, preparing: true, maxItems: cap, window: window)
        }
        let bounded = AtlasReferenceWindowRequest(offset: 4_096, countAllMatches: false)
        let probe = try decode(fixture.windowPage(bounded, cap: 2), bounded)
        precondition(probe.inventory.matchesCounted == 4_099 && !probe.inventory.countComplete)
        precondition(probe.inventory.window?.nextOffset == 4_098 && probe.inventory.window?.hasMore == true)
        precondition(probe.summary.contains("At least 4099"))
        let finalWindow = AtlasReferenceWindowRequest(offset: 5_003, countAllMatches: false)
        let exact = try decode(fixture.windowPage(finalWindow, cap: 2), finalWindow)
        precondition(exact.inventory.countComplete && !exact.inventory.limited)
        precondition(exact.inventory.matchesCounted == 5_005 && exact.rows.last?.occurrence == 5_005)
        precondition(exact.inventory.window?.rowsTruncated == true && exact.inventory.window?.hasMore == false)
        precondition(exact.inventory.window?.nextOffset == nil && exact.nextOffset == nil)

        let counted = AtlasReferenceWindowRequest(offset: 0, countAllMatches: true)
        let zero = try decode(fixture.windowPage(counted, cap: 0), counted, cap: 0)
        precondition(zero.rows.isEmpty && zero.inventory.matchesCounted == 5_005 && zero.inventory.countComplete)
        precondition(zero.inventory.window?.hasMore == true && zero.inventory.window?.nextOffset == nil)
        precondition(zero.summary.contains("no retained matches"))
        let zeroProbe = try decode(fixture.windowPage(bounded, cap: 0), bounded, cap: 0)
        precondition(zeroProbe.inventory.matchesCounted == 4_097 && !zeroProbe.inventory.countComplete)
        precondition(zeroProbe.rows.isEmpty && zeroProbe.inventory.window?.nextOffset == nil)
        for all in [true, false] {
            let beyond = AtlasReferenceWindowRequest(offset: .max, countAllMatches: all)
            let page = try decode(fixture.windowPage(beyond, cap: 2), beyond)
            precondition(page.rows.isEmpty && page.inventory.countComplete && !page.inventory.limited)
            precondition(page.inventory.matchesCounted == 5_005 && page.inventory.window?.skipped == 5_005)
            precondition(page.inventory.window?.nextOffset == nil && page.summary.contains("no retained matches"))
        }
        let emptyFixture = ReferenceFixture(copies: 0), emptyInfo = try emptyFixture.info()
        let emptyRequest = AtlasReferenceWindowRequest(offset: .max, countAllMatches: true)
        let empty = try AtlasReferencePage.decode(ReferenceFixture.json(emptyFixture.windowPage(emptyRequest, cap: 0)),
            info: emptyInfo, generation: 1, name: emptyFixture.name, request: .initial, preparing: true,
            maxItems: 0, window: emptyRequest)
        precondition(empty.rows.isEmpty && empty.inventory.countComplete && empty.inventory.matchesCounted == 0)
        precondition(empty.inventory.window?.rowsTruncated == false)

        let window = AtlasReferenceWindowRequest(offset: 4_096, countAllMatches: true)
        let valid = fixture.windowPage(window, cap: 2)
        for key in ["result_offset", "count_all_matches", "skipped_matches", "rows_truncated",
            "has_more_matches", "next_match_offset"] {
            var missing = valid; missing.removeValue(forKey: key)
            reject("missing required window field: \(key)") { _ = try decode(missing, window) }
            var null = valid; null[key] = NSNull()
            reject("null required nonterminal window field: \(key)") { _ = try decode(null, window) }
        }
        for (key, value) in [
            ("result_offset", "4095"), ("result_offset", "04096"), ("result_offset", "18446744073709551616"),
            ("skipped_matches", "4095"),
            ("matches_counted", "18446744073709551615"), ("retained_references", "3"),
            ("next_match_offset", "4097"), ("next_match_offset", "18446744073709551615")
        ] {
            var bad = valid; bad[key] = value
            reject("invalid window number: \(key)=\(value)") { _ = try decode(bad, window) }
        }
        for (key, value) in [("count_all_matches", false), ("rows_truncated", false),
            ("has_more_matches", false), ("output_limited", false), ("search_complete", false), ("count_complete", false)] {
            var bad = valid; bad[key] = value
            reject("invalid window count/evidence flag: \(key)") { _ = try decode(bad, window) }
        }
        var noncanonical = valid; noncanonical["result_offset"] = 4096
        reject("window offsets must be canonical integer strings") { _ = try decode(noncanonical, window) }
        var falseExhaustive = fixture.windowPage(bounded, cap: 2); falseExhaustive["matches_counted"] = "5005"
        reject("bounded lookahead must not masquerade as a complete total") { _ = try decode(falseExhaustive, bounded) }
        var overflow = fixture.windowPage(emptyRequest, cap: 2)
        overflow["retained_references"] = "1"; overflow["references"] = [fixture.row(1)]
        reject("full-width offset must not permit overflowing retained rows") { _ = try decode(overflow, emptyRequest) }
        reject("window request must not silently downgrade to legacy response") {
            _ = try decode(fixture.page(cap: 2), window)
        }
        reject("unsolicited window response must not acquire legacy authority") {
            _ = try AtlasReferencePage.decode(ReferenceFixture.json(valid), info: info, generation: 1,
                name: fixture.name, request: .initial, preparing: true, maxItems: 2)
        }
        for mutation in ["missing", "zero", "local", "duplicate", "overflow", "noncanonical"] {
            var bad = valid, rows = valid["references"] as! [[String: Any]]
            switch mutation {
            case "missing": rows[0].removeValue(forKey: "occurrence_id")
            case "zero": rows[0]["occurrence_id"] = "0"
            case "local": rows[0]["occurrence_id"] = "1"
            case "duplicate": rows[1]["occurrence_id"] = "4097"
            case "overflow": rows[0]["occurrence_id"] = "18446744073709551616"
            default: rows[0]["occurrence_id"] = "04097"
            }
            bad["references"] = rows
            reject("invalid global occurrence identity: \(mutation)") { _ = try decode(bad, window) }
        }
        var globalLocalID = valid, localRows = valid["references"] as! [[String: Any]]
        localRows[0]["reference_id"] = "4097"; globalLocalID["references"] = localRows
        reject("global occurrence cannot replace local activation ID") { _ = try decode(globalLocalID, window) }

        let inventoryRequest = AtlasReferenceWindowRequest(offset: 4_096, countAllMatches: true)
        let initial = try decode(fixture.windowPage(inventoryRequest, cap: 128), inventoryRequest, cap: 128)
        let request = AtlasReferencePageRequest(start: 64, limit: 64)
        var drift = fixture.windowPage(inventoryRequest, request, preparing: false, cap: 128)
        drift["matches_counted"] = "5006"
        reject("exact total changed between pages of one window") {
            _ = try AtlasReferencePage.decode(ReferenceFixture.json(drift), info: info, generation: 1,
                name: fixture.name, request: request, inventory: initial.inventory, maxItems: 128)
        }
        let different = AtlasReferenceWindowRequest(offset: 4_097, countAllMatches: true)
        reject("explicit changed window cannot relabel an accepted inventory") {
            _ = try AtlasReferencePage.decode(
                ReferenceFixture.json(fixture.windowPage(different, request, preparing: false, cap: 128)),
                info: info, generation: 1, name: fixture.name, request: request,
                inventory: initial.inventory, maxItems: 128, window: different)
        }
        let firstTarget = initial.target(id: 1)
        let rebuilt = try decode(fixture.windowPage(inventoryRequest, cap: 128), inventoryRequest, cap: 128)
        precondition(initial.inventory.matches(rebuilt.inventory) && firstTarget != rebuilt.target(id: 1),
            "same occurrence ordinal in a new page must not reactivate an old event")
    }
}
