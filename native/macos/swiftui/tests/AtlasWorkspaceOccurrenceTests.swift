// Production workspace worker + fixed C-boundary receipts. Matching remains in
// Rust; these tests qualify native mode admission, retention and exact evidence.
import Foundation

@_silgen_name("fixture_reset") private func resetFixture()
@_silgen_name("fixture_set") private func setFixture(_ kind: UInt32, _ index: UInt32, _ text: UnsafePointer<CChar>)
@_silgen_name("fixture_count") private func fixtureCount(_ key: UInt32) -> UInt32
@_silgen_name("fixture_expect_scope") private func expectScope(_ extensions: UnsafePointer<CChar>?, _ filenames: UnsafePointer<CChar>?)
@_silgen_name("fixture_expect_path") private func expectPath(_ kind: UInt64, _ path: UnsafePointer<CChar>)
@_silgen_name("fixture_expect_structure") private func expectStructure(_ mode: UInt64, _ nameMode: UInt64)
@_silgen_name("fixture_cancel_step") private func cancelStep()
@_cdecl("fixture_mark_canceled") public func markCanceled(_ context: UnsafeMutableRawPointer?) {
    guard let context else { preconditionFailure("Missing cancellation context") }
    Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().cancel()
}
private final class OccurrenceReports: @unchecked Sendable {
    private let lock = NSLock()
    private var reports: [AtlasSearchReport] = []
    func append(_ report: AtlasSearchReport) { lock.lock(); reports.append(report); lock.unlock() }
    func clear() { lock.lock(); reports = []; lock.unlock() }
    var values: [AtlasSearchReport] { lock.lock(); defer { lock.unlock() }; return reports }
}

@main private enum AtlasWorkspaceOccurrenceTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        precondition(condition(), message); checks += 1
    }
    private static func hex(_ text: String) -> String {
        text.utf8.map { String(format: "%02x", $0) }.joined()
    }
    private static func hit(_ id: Int, name: String = "run", utf16: Bool = false,
                            path: String = "src/a.rs") -> [String: Any] {
        let start = (id - 1) * 16 + 2
        let range = ["start": String(start), "end": String(start + (utf16 ? name.utf16.count * 2 : name.utf8.count))]
        return ["hit_id": String(id), "file_id": "1", "source_revision": "1",
            "capture_sha256": String(repeating: "a", count: 64), "capture_byte_length": "32768",
            "original_range": range,
            "path": ["encoding": "unix-bytes", "hex": hex(path), "display": path],
            "structure": ["name": name, "kind": "identifier-occurrence", "language": "text",
                "candidate_id": String(id), "heading_slug": NSNull(), "evidence_range": range,
                "evidence_level": "whole-token-text-candidate"]]
    }
    private static func page(_ command: String, step: Int, total: Int = 0, start: Int = 0,
                             running: Bool = true, complete: Bool = false, catalogued: Int = 1,
                             name: String = "run", utf16: Bool = false,
                             path: String = "src/a.rs") -> [String: Any] {
        let end = min(total, start + (command == "page" ? 128 : 64))
        let rows = start < end ? ((start + 1)...end).map { hit($0, name: name, utf16: utf16, path: path) } : []
        return ["schema": "fcb.atlas-search/1", "status": "ok", "command": command,
            "owner": "11", "source_manifest": "1", "layout_revision": "1", "query_generation": "1",
            "needle": name, "mode": "identifier-occurrence", "search_strategy": "live-structure-scan",
            "requested_mode": "occurrences", "realized_mode": "whole-token-text-candidate",
            "evidence_level": "whole-token-text-candidate", "name_mode": "exact", "name_case": "sensitive",
            "semantic_complete": false, "completeness_basis": "admitted-extraction-processing",
            "includes_comments_and_literals": true, "compiler_resolved": false,
            "token_policy": AtlasSearchOccurrencePolicy.tokenPolicy,
            "name_syntax": AtlasSearchOccurrencePolicy.nameSyntax, "unicode_normalization": "none",
            "structure_file_byte_limit": "524288", "unsupported_structure_files": "0",
            "extraction_limited_files": "0", "structure_metadata_omitted": "0",
            "structure_metadata_bytes": String(total * name.utf8.count),
            "discovery_complete": true, "search_in_progress": running, "search_complete": complete,
            "truncated": false, "stop_reason": running ? NSNull() : "all-files-examined",
            "catalogued_files": String(catalogued), "examined_files": String(min(step, catalogued)),
            "scanned_files": String(min(step, catalogued)), "unavailable_files": "0",
            "pending_files": String(max(0, catalogued - step)), "matches_seen": String(total),
            "retained_hits": String(total), "displayed_hits": String(total), "step_count": String(step),
            "hits": rows, "next_offset": end < total ? String(end) : NSNull()]
    }
    private static func install(_ kind: UInt32, _ value: [String: Any], index: UInt32 = 0) throws {
        let data = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
        String(decoding: data, as: UTF8.self).withCString { setFixture(kind, index, $0) }
    }
    private static func setup(name: String = "run", utf16: Bool = false, total: Int = 1,
                              catalogued: Int = 1) throws {
        resetFixture(); expectStructure(4, 0)
        try install(0, ["status": "ok"])
        try install(1, page("begin-structure", step: 0, catalogued: catalogued, name: name))
        try install(2, page("step", step: 1, total: total, running: catalogued > 1,
            complete: catalogued == 1, catalogued: catalogued, name: name, utf16: utf16))
        var offset = 64, index: UInt32 = 0
        while offset < total {
            try install(3, page("page", step: 1, total: total, start: offset,
                running: catalogued > 1, complete: catalogued == 1, catalogued: catalogued,
                name: name, utf16: utf16), index: index)
            offset += 128; index += 1
        }
        if catalogued > 1 {
            try install(2, page("step", step: 2, total: total, running: false, complete: true,
                catalogued: catalogued, name: name, utf16: utf16), index: 1)
        }
    }
    private static func run(name: String = "run", scope: AtlasSearchFileScope = .all,
                            path: AtlasSearchPathScope = .workspace,
                            cancellation: AtlasSearchCancellation = AtlasSearchCancellation(),
                            progress: @Sendable (AtlasSearchReport) -> Void = { _ in }) throws -> AtlasSearchReport {
        try AtlasNativeSearch.runScoped(input: AtlasSearchInput(root: "/project", query: name,
            fileScope: scope, pathScope: path, mode: .occurrences, nameMode: .prefix, caseSensitive: false),
            cancellation: cancellation, access: AtlasSearchAccessLease(nil), progress: progress)
    }
    private static func retired() {
        let deadline = Date().addingTimeInterval(5)
        while fixtureCount(5) == 0 && Date() < deadline { Thread.sleep(forTimeInterval: 0.001) }
        precondition(fixtureCount(5) == 1, "Search owner must retire exactly once before resetting fixture")
        precondition(fixtureCount(6) == fixtureCount(7), "Every bounded C reply must be freed")
    }
    @MainActor private static func rejected(_ label: String, name: String = "run",
        _ change: (inout [String: Any]) -> Void) throws {
        try setup(name: name)
        var terminal = page("step", step: 1, total: 1, running: false, complete: true, name: name)
        change(&terminal); try install(2, terminal)
        do { _ = try run(name: name); preconditionFailure("Accepted invalid occurrence receipt: " + label) }
        catch AtlasSearchError.invalidResponse { checks += 1 }
        retired()
    }
    private static func editFirst(_ value: inout [String: Any], _ edit: (inout [String: Any]) -> Void) {
        var rows = value["hits"] as! [[String: Any]]
        edit(&rows[0]); value["hits"] = rows
    }
    private static func editStructure(_ value: inout [String: Any], _ edit: (inout [String: Any]) -> Void) {
        editFirst(&value) { row in
            var structure = row["structure"] as! [String: Any]
            edit(&structure); row["structure"] = structure
        }
    }
    @MainActor static func main() throws {
        check(AtlasSearchMode.occurrences.code == 4 && !AtlasSearchMode.occurrences.isTextual,
            "Occurrences uses the structural live path, never the literal index")
        check(!AtlasSearchMode.occurrences.supportsNameMatching
              && AtlasSearchMode.symbols.supportsNameMatching && AtlasSearchMode.headings.supportsNameMatching,
            "Prefix/contains control belongs only to declaration and heading names")
        for mode in AtlasSearchNameMode.allCases {
            let input = try AtlasSearchInput(root: "/project", query: "run", mode: .occurrences,
                nameMode: mode, caseSensitive: false)
            check(input.nameMode == .exact && input.caseSensitive, "Mode switch normalizes stale name/case controls")
        }

        try setup(total: 70, catalogued: 2)
        let progress = OccurrenceReports()
        var result: AtlasSearchReport? = try run(progress: progress.append)
        check(result!.complete && result!.hits.count == 70 && result!.mode == .occurrences
              && result!.nameMode == .exact && result!.caseSensitive, "Complete exact whole-token inventory")
        check(result!.hits.allSatisfy { $0.structure?.mode == .occurrences && $0.actualText == nil },
            "Occurrence evidence has its own row namespace")
        check(progress.values.contains { $0.isInProgress && $0.hits.count == 70 && !$0.complete },
            "Progressive occurrence results arrive before later files")
        check(fixtureCount(11) == 1 && fixtureCount(4) == 1 && fixtureCount(12) == 0,
            "Mode4 exact ABI called once, continuation page fetched once, no insensitive literal query")
        check(result!.summary.contains("comments and literals") && result!.summary.contains("compiler"),
            "Coverage labels retain whole-token qualification")
        var target = result!.capture?.target(result!.hits[0])
        check(target?.mode == .occurrences && target?.needle == "run", "Captured target retains exact occurrence mode")
        var forged = result!.hits[0]
        let good = forged.structure!
        forged.structure = AtlasSearchStructure(name: "other", kind: good.kind, language: good.language,
            evidence: good.evidence, candidate: good.candidate, headingSlug: nil,
            evidenceStart: good.evidenceStart, evidenceEnd: good.evidenceEnd)
        check(result!.capture?.target(forged) == nil, "Altered occurrence metadata cannot borrow a capture witness")
        var imported = hit(1)
        imported.merge(["schema": "fcb.atlas-search/1", "status": "ok", "command": "open-reader",
            "owner": "11", "source_manifest": "1", "layout_revision": "1", "query_generation": "1",
            "reader_owner": "100", "source_reopened": false, "source_observation": "retained-search-capture",
            "reader": ["schema": "fcb.reader-session/1", "status": "ok", "command": "info",
                "owner": "100", "file_id": "1", "source_revision": "1", "captured_bytes": "32768",
                "path": ["encoding": "unix-bytes", "hex": hex("src/a.rs"), "display": "src/a.rs"],
                "capture_origin": "host-supplied", "initial_source_bytes_read": "0", "initial_read_calls": "0",
                "additional_source_bytes_read": "0", "native_presented": false, "encoding": "utf8"]],
            uniquingKeysWith: { _, new in new })
        try install(4, imported)
        result = nil; progress.clear()
        check(fixtureCount(5) == 0, "Selected occurrence keeps its exact capture alive")
        let readerInfo = try target!.openReader(100)
        check(readerInfo.contains("utf8") && fixtureCount(8) == 1 && fixtureCount(1) == 1,
            "Activation imports retained source without catalog reopen")
        target = nil; retired()

        for utf16 in [false, true] {
            try setup(name: "éx", utf16: utf16)
            var unicode: AtlasSearchReport? = try run(name: "éx")
            check(unicode!.hits[0].end - unicode!.hits[0].start == (utf16 ? 4 : 3),
                "Exact non-ASCII name accepts original UTF-8 or UTF-16 width")
            var wrong = unicode!.hits[0]; let row = wrong.structure!
            wrong.structure = AtlasSearchStructure(name: "e\u{301}x", kind: row.kind, language: row.language,
                evidence: row.evidence, candidate: row.candidate, headingSlug: nil,
                evidenceStart: row.evidenceStart, evidenceEnd: row.evidenceEnd)
            check(unicode!.capture?.target(wrong) == nil,
                "Canonically equivalent metadata cannot replace exact name bytes")
            unicode = nil; retired()
        }

        try setup(total: 1000, catalogued: 2)
        var full: AtlasSearchReport? = try run()
        check(full!.complete && !full!.truncated && full!.hits.count == 1000 && fixtureCount(3) == 2,
            "Exact result allowance followed by a negative file remains complete")
        full = nil; retired()

        try setup(total: 1000)
        for index in 0...8 {
            var limited = page(index == 0 ? "step" : "page", step: 1, total: 1000,
                start: index == 0 ? 0 : 64 + (index - 1) * 128, running: false)
            limited["matches_seen"] = "1001"; limited["truncated"] = true
            limited["stop_reason"] = "match-limit"
            try install(index == 0 ? 2 : 3, limited, index: UInt32(max(0, index - 1)))
        }
        var partial: AtlasSearchReport? = try run()
        check(!partial!.complete && partial!.truncated && partial!.unavailableFiles == 0,
            "Real match lookahead is a result limit, not a source extraction failure")
        partial = nil; retired()

        try setup(total: 0)
        var unsupported = page("step", step: 1, running: false)
        unsupported["unavailable_files"] = "1"; unsupported["scanned_files"] = "0"
        try install(2, unsupported)
        var coverage: AtlasSearchReport? = try run()
        check(!coverage!.complete && coverage!.hits.isEmpty && coverage!.unavailableFiles == 1
              && coverage!.summary.contains("incomplete analysis"),
            "Unsupported text remains unavailable partial coverage, never a complete empty search")
        coverage = nil; retired()

        let scope = try AtlasSearchFileScope(extensions: ["rs", "swift"])
        let path = try AtlasSearchPathScope(kind: .directory, path: "src")
        try setup()
        scope.withCStrings { expectScope($0, $1) }
        path.withCString { expectPath($0, $1!) }
        for kind in [UInt32(1), 2] {
            var scoped = page(kind == 1 ? "begin-structure" : "step", step: kind == 1 ? 0 : 1,
                total: kind == 1 ? 0 : 1, running: kind == 1, complete: kind == 2)
            scoped["search_scope"] = scope.wireKind
            scoped["scope_extensions"] = scope.extensions ?? ""; scoped["scope_filenames"] = scope.filenames ?? ""
            scoped["eligible_files"] = "1"; scoped["excluded_files"] = "0"
            scoped["scope_path_kind"] = path.kind.rawValue
            scoped["scope_path"] = path.path; scoped["scope_path_hex"] = path.pathHex
            try install(kind, scoped)
        }
        var scoped: AtlasSearchReport? = try run(scope: scope, path: path)
        check(scoped!.complete && scoped!.fileScope == scope && scoped!.pathScope == path,
            "File-type and exact folder scopes survive mode4 admission and receipts")
        scoped = nil; retired()

        for (key, wrong) in [
            ("includes_comments_and_literals", false as Any), ("compiler_resolved", true as Any),
            ("semantic_complete", true as Any), ("token_policy", "language-parser" as Any),
            ("name_syntax", "arbitrary-text" as Any), ("unicode_normalization", "NFC" as Any),
            ("name_mode", "prefix" as Any), ("name_case", "insensitive" as Any),
            ("requested_mode", "symbols" as Any), ("realized_mode", "heuristic-outline-candidate" as Any),
            ("evidence_level", "exact-decoded-literal" as Any), ("structure_file_byte_limit", "1048576" as Any)
        ] {
            try rejected("wrong " + key) { $0[key] = wrong }
        }
        for key in ["includes_comments_and_literals", "compiler_resolved", "token_policy",
                    "name_syntax", "unicode_normalization"] {
            try rejected("missing " + key) { $0.removeValue(forKey: key) }
        }
        try rejected("wrong row name") { editStructure(&$0) { $0["name"] = "runner" } }
        try rejected("canonical-equivalent query echo", name: "é") { $0["needle"] = "e\u{301}" }
        try rejected("canonical-equivalent row name", name: "é") {
            editStructure(&$0) { $0["name"] = "e\u{301}" }
        }
        try rejected("wrong row kind") { editStructure(&$0) { $0["kind"] = "function" } }
        try rejected("wrong row language") { editStructure(&$0) { $0["language"] = "rust" } }
        try rejected("heading namespace") { editStructure(&$0) { $0["heading_slug"] = "run" } }
        try rejected("broad evidence range") {
            editStructure(&$0) { $0["evidence_range"] = ["start": "0", "end": "20"] }
        }
        try rejected("substring width") {
            editFirst(&$0) { row in
                row["original_range"] = ["start": "2", "end": "4"]
                var structure = row["structure"] as! [String: Any]
                structure["evidence_range"] = row["original_range"]; row["structure"] = structure
            }
        }
        try rejected("oversize occurrence capture") { editFirst(&$0) { $0["capture_byte_length"] = "524289" } }
        try rejected("literal namespace mixed into occurrence") { editFirst(&$0) { $0["matched_text"] = "run" } }
        try rejected("wrong preview spelling") {
            editFirst(&$0) { $0["excerpt"] = [
                "text": "bad", "original_range": ["start": "2", "end": "5"],
                "match_utf8_range": ["start": "0", "end": "3"],
                "has_replacements": false, "clipped_start": true, "clipped_end": true
            ] }
        }

        for mode in [AtlasSearchMode.text, .symbols, .headings] {
            resetFixture()
            if mode.supportsNameMatching { expectStructure(mode.code, 0) }
            try install(0, ["status": "ok"])
            var foreign = page(mode.isTextual ? "begin" : "begin-structure", step: 0)
            foreign["mode"] = mode.wireMode
            foreign["search_strategy"] = mode.isTextual ? "live-capture-scan" : "live-structure-scan"
            if mode.isTextual {
                for key in ["requested_mode", "realized_mode", "name_mode", "name_case", "evidence_level",
                            "semantic_complete", "completeness_basis", "structure_file_byte_limit",
                            "unsupported_structure_files", "extraction_limited_files",
                            "structure_metadata_omitted", "structure_metadata_bytes"] {
                    foreign.removeValue(forKey: key)
                }
            } else {
                foreign["requested_mode"] = mode.rawValue; foreign["realized_mode"] = mode.evidence
                foreign["evidence_level"] = mode.evidence
                foreign["structure_file_byte_limit"] = String(mode.structureFileByteLimit)
            }
            try install(1, foreign)
            do {
                _ = try AtlasNativeSearch.runScoped(input: AtlasSearchInput(root: "/project", query: "run",
                    mode: mode, nameMode: .exact), cancellation: AtlasSearchCancellation(),
                    access: AtlasSearchAccessLease(nil), progress: { _ in })
                preconditionFailure("Occurrence policy leaked into another mode")
            } catch AtlasSearchError.invalidResponse { checks += 1 }
            retired()
        }

        // Repeated progressive rows must preserve byte spelling even when
        // Swift String equality considers two Unicode strings equivalent.
        try setup(name: "é", catalogued: 2)
        var changed = page("step", step: 2, total: 1, running: false, complete: true,
            catalogued: 2, name: "é")
        editStructure(&changed) { $0["name"] = "e\u{301}" }
        try install(2, changed, index: 1)
        do { _ = try run(name: "é"); preconditionFailure("Repeated row spelling changed") }
        catch AtlasSearchError.invalidResponse { checks += 1 }
        retired()

        try setup()
        try install(1, ["schema": "fcb.atlas-search/1", "status": "error",
            "error": ["code": "REFERENCE_INVALID_NAME"]])
        do { _ = try run(); preconditionFailure("Invalid token accepted") }
        catch AtlasSearchError.query(let code) {
            check(code == "REFERENCE_INVALID_NAME"
                  && AtlasSearchError.query(code).message.contains("identifier")
                  && !AtlasSearchError.query(code).message.contains("expression"),
                "Token validation error has precise occurrence guidance")
        }
        check(fixtureCount(3) == 0, "Invalid token never starts source scanning")
        retired()

        try setup(); cancelStep()
        do { _ = try run(); preconditionFailure("Cancellation ignored") }
        catch AtlasSearchError.canceled { checks += 1 }
        retired()
        try setup(catalogued: 2)
        let flag = AtlasSearchCancellation()
        do {
            _ = try run(cancellation: flag) { if !$0.hits.isEmpty { flag.cancel() } }
            preconditionFailure("Canceled provisional occurrence work published terminal")
        } catch AtlasSearchError.canceled { checks += 1 }
        check(fixtureCount(3) == 1, "Cancellation after provisional rows prevents the next file")
        retired()
        print("AtlasWorkspaceOccurrenceTests: \(checks) checks passed")
    }
}
