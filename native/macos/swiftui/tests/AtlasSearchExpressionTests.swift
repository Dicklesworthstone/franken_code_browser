// Real native codecs/workers against scripted foreign envelopes. These tests
// do not parse expressions, match source, execute Rust, or qualify native UI.
import Foundation

@_silgen_name("fixture_reset") private func resetExpressionFixture()
@_silgen_name("fixture_set") private func setExpressionFixture(_ kind: UInt32, _ index: UInt32, _ text: UnsafePointer<CChar>)
@_silgen_name("fixture_count") private func expressionFixtureCount(_ key: UInt32) -> UInt32
@_silgen_name("fixture_cancel_step") private func cancelExpressionStep()
@_silgen_name("fixture_expect_expression") private func expectExpression(_ text: UnsafePointer<CChar>, _ caseSensitive: UInt64)
@_cdecl("fixture_mark_canceled") public func cancelExpressionFixture(_ context: UnsafeMutableRawPointer?) {
    guard let context else { preconditionFailure("Missing cancellation context") }
    Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().cancel()
}

private enum ExpressionFixture {
    static let query = "\"x\" required -excluded path:*.rs"
    static func json(_ object: [String: Any]) -> String {
        String(data: try! JSONSerialization.data(withJSONObject: object), encoding: .utf8)!
    }
    static func metadata(query: String = ExpressionFixture.query, primary: String = "x", excluded: Int = 1,
                         eligible: Int = 3, scanned: UInt64 = 0) -> [String: Any] {
        ["expression": query, "primary_needle": primary, "query_syntax": "fcb-bounded-expression/1",
         "predicate_scope": "same-complete-capture", "path_filter_domain": "utf8-or-escaped-search-key",
         "max_scan_bytes": String(AtlasSearchExpression.scanLimit), "scanned_input_bytes": String(scanned),
         "expression_excluded_files": String(excluded), "expression_scope_files": String(eligible - excluded)]
    }
    static func hit(_ id: Int) -> [String: Any] {
        ["hit_id": String(id), "file_id": "1", "source_revision": "1",
         "matched_text": "x",
         "capture_sha256": String(repeating: "a", count: 64), "capture_byte_length": "1000",
         "original_range": ["start": String(id - 1), "end": String(id)],
         "path": ["encoding": "unix-bytes", "hex": "612e7273", "display": "a.rs"]]
    }
    static func page(_ command: String, step: Int, total: Int = 0, start: Int = 0, complete: Bool = false) -> [String: Any] {
        let end = min(total, start + (command == "page" ? 128 : 64))
        var out: [String: Any] = ["schema": "fcb.atlas-search/1", "status": "ok", "command": command,
            "owner": "11", "source_manifest": "1", "layout_revision": "1", "query_generation": "1",
            "needle": "x", "mode": "decoded-text-expression", "case_sensitive": true, "search_strategy": "live-capture-scan",
            "discovery_complete": true, "search_in_progress": !complete, "search_complete": complete,
            "truncated": false, "stop_reason": complete ? "all-files-examined" : NSNull(),
            "catalogued_files": "3", "examined_files": String(step + 1), "scanned_files": String(step),
            "unavailable_files": "0", "pending_files": String(2 - step), "matches_seen": String(total),
            "retained_hits": String(total), "displayed_hits": String(total), "step_count": String(step),
            "hits": (start..<end).map { hit($0 + 1) }, "next_offset": end < total ? String(end) : NSNull()]
        out.merge(metadata(scanned: UInt64(step * 2000)), uniquingKeysWith: { _, new in new })
        return out
    }
    static func install(_ kind: UInt32, _ object: [String: Any], index: UInt32 = 0) {
        json(object).withCString { setExpressionFixture(kind, index, $0) }
    }
    static func setup() {
        resetExpressionFixture(); query.withCString { expectExpression($0, 1) }
        install(0, ["status": "ok"])
        install(1, page("begin-expression", step: 0))
        install(2, page("step", step: 1, total: 70))
        install(3, page("page", step: 1, total: 70, start: 64))
        install(2, page("step", step: 2, total: 70, complete: true), index: 1)
        var imported = hit(1)
        imported.merge(["schema": "fcb.atlas-search/1", "status": "ok", "command": "open-reader",
            "owner": "11", "source_manifest": "1", "layout_revision": "1", "query_generation": "1",
            "reader_owner": "100", "source_reopened": false, "source_observation": "retained-search-capture",
            "reader": ["schema": "fcb.reader-session/1", "status": "ok", "command": "info", "owner": "100",
                "file_id": "1", "source_revision": "1", "captured_bytes": "1000", "encoding": "utf8",
                "capture_origin": "host-supplied", "initial_source_bytes_read": "0", "initial_read_calls": "0",
                "additional_source_bytes_read": "0", "native_presented": false,
                "path": ["encoding": "unix-bytes", "hex": "612e7273", "display": "a.rs"]]],
            uniquingKeysWith: { _, new in new })
        install(4, imported)
    }
}

@main private enum AtlasSearchExpressionTests {
    private static func reject(_ reason: String, _ operation: () throws -> Void) {
        do { try operation(); preconditionFailure(reason) } catch AtlasSearchError.invalidResponse { }
        catch { preconditionFailure("Wrong refusal for \(reason): \(error)") }
    }
    private static func retireLive() {
        let deadline = Date().addingTimeInterval(5)
        while expressionFixtureCount(5) == 0 && Date() < deadline { Thread.sleep(forTimeInterval: 0.001) }
        precondition(expressionFixtureCount(5) == 1 && expressionFixtureCount(6) == expressionFixtureCount(7),
            "Each live query retires once and frees every foreign response")
    }
    private static func live() throws -> AtlasSearchReport {
        try AtlasNativeSearch.runScoped(input: AtlasSearchInput(root: "/project", query: ExpressionFixture.query, mode: .expression),
            cancellation: AtlasSearchCancellation(), access: AtlasSearchAccessLease(nil), progress: { _ in })
    }
    private static func metadata(_ object: [String: Any], query: String = ExpressionFixture.query,
                                 needle: String = "x", eligible: Int = 3, mode: AtlasSearchMode = .expression) throws -> AtlasSearchExpression? {
        try JSONDecoder().decode(AtlasSearchExpression.Wire.self, from: Data(ExpressionFixture.json(object).utf8))
            .admit(mode: mode, query: query, needle: needle, eligibleFiles: eligible)
    }
    private static func liveSuccess() throws {
        let report = try live()
        precondition(report.complete && report.mode == .expression && report.hits.count == 70)
        precondition(report.expression?.query == ExpressionFixture.query && report.expression?.primaryNeedle == "x")
        precondition(report.expression?.excludedFiles == 1 && report.expression?.scannedInputBytes == 4000)
        precondition(expressionFixtureCount(13) == 1 && expressionFixtureCount(3) == 2 && expressionFixtureCount(4) == 1,
            "Expression search uses the shared progressive step/page pipeline")
        let target = report.capture!.target(report.hits[0])!
        precondition(target.mode == .expression && target.needle == "x",
            "Reader activation must carry the primary literal and expression origin")
        _ = try target.openReader(100)
        precondition(expressionFixtureCount(1) == 1 && expressionFixtureCount(8) == 1,
            "A result imports the retained capture without reopening source")
    }
    private static func run(_ worker: AtlasIndexedSearch, query: String = ExpressionFixture.query,
                            mode: AtlasSearchMode = .expression, caseSensitive: Bool = true,
                            reports: IndexReports = IndexReports()) throws -> AtlasSearchReport {
        try worker.runScoped(input: AtlasSearchInput(root: "/repo", query: query, mode: mode, caseSensitive: caseSensitive),
            cancellation: AtlasSearchCancellation(), access: AtlasSearchAccessLease(nil), progress: reports.offer)
    }
    static func main() throws {
        let admitted = try metadata(ExpressionFixture.metadata())!
        let progressed = try metadata(ExpressionFixture.metadata(scanned: 20))!
        precondition(progressed.advances(admitted) && !admitted.advances(progressed))
        let literal = try metadata([:], mode: .text)
        precondition(literal == nil)
        for (key, value) in [("expression", "other"), ("primary_needle", "other"), ("query_syntax", "shell"),
            ("predicate_scope", "visible-excerpt"), ("path_filter_domain", "normalized-native-path"),
            ("max_scan_bytes", "268435455"), ("scanned_input_bytes", "268435457"),
            ("scanned_input_bytes", "00"), ("expression_excluded_files", "4"), ("expression_scope_files", "3")] {
            var bad = ExpressionFixture.metadata(); bad[key] = value
            reject("invalid expression metadata \(key)") { _ = try metadata(bad) }
        }
        for key in ExpressionFixture.metadata().keys {
            var missing = ExpressionFixture.metadata(); missing.removeValue(forKey: key)
            reject("partial expression metadata \(key)") { _ = try metadata(missing) }
        }
        reject("literal page acquired expression policy") { _ = try metadata(ExpressionFixture.metadata(), mode: .text) }
        reject("canonical-equivalent expression rewrote raw query") {
            _ = try metadata(ExpressionFixture.metadata(query: "cafe\u{301}"), query: "café")
        }
        let decomposed = try metadata(ExpressionFixture.metadata(query: "cafe\u{301}", primary: "cafe\u{301}"),
            query: "cafe\u{301}", needle: "cafe\u{301}")!
        precondition(decomposed.primaryNeedle.utf8.elementsEqual("cafe\u{301}".utf8))

        ExpressionFixture.setup(); try liveSuccess(); retireLive()
        for (key, value) in [("expression", "different query"), ("primary_needle", "different term"),
            ("predicate_scope", "same-line"), ("max_scan_bytes", "33554432"),
            ("expression_excluded_files", "0"), ("scanned_input_bytes", "268435457")] {
            ExpressionFixture.setup(); var bad = ExpressionFixture.page("step", step: 1, total: 70); bad[key] = value
            ExpressionFixture.install(2, bad)
            reject("live worker admitted \(key)") { _ = try live() }; retireLive()
        }
        ExpressionFixture.setup()
        var drift = ExpressionFixture.page("page", step: 1, total: 70, start: 64); drift["scanned_input_bytes"] = "2001"
        ExpressionFixture.install(3, drift)
        reject("page borrowed another expression receipt") { _ = try live() }; retireLive()
        for missing in ["case_sensitive", "matched_text"] {
            ExpressionFixture.setup(); var bad = ExpressionFixture.page("step", step: 1, total: 70)
            if missing == "case_sensitive" { bad.removeValue(forKey: missing) }
            else {
                var rows = bad["hits"] as! [[String: Any]]; rows[0].removeValue(forKey: missing); bad["hits"] = rows
            }
            ExpressionFixture.install(2, bad)
            reject("live expression omitted \(missing)") { _ = try live() }; retireLive()
        }
        ExpressionFixture.setup(); cancelExpressionStep()
        do { _ = try live(); preconditionFailure("Canceled expression published") }
        catch AtlasSearchError.canceled { }
        retireLive()
        for code in ["QUERY_EMPTY", "QUERY_NEEDLE_TOO_LONG", "QUERY_LIMIT_EXCEEDED", "QUERY_REGEX_UNQUALIFIED", "QUERY_SYNTAX_ERROR"] {
            ExpressionFixture.setup()
            // The shared atlas ABI owns refusal envelopes, including query
            // errors, even though successful query pages use atlas-search/1.
            let error: [String: Any] = ["schema": "fcb.atlas-session/1", "status": "error", "error": ["code": code]]
            ExpressionFixture.install(1, error)
            do { _ = try live(); preconditionFailure("Query refusal became results") }
            catch AtlasSearchError.query(let actual) { precondition(actual == code) }
            retireLive()
            do { _ = try AtlasIndexWire.decode(ExpressionFixture.json(error)); preconditionFailure("Indexed query refusal lost") }
            catch AtlasSearchError.query(let actual) { precondition(actual == code) }
        }

        do {
            let fixture = IndexedFixture(), reports = IndexReports()
            fixture.configure { $0.expressionExcluded = 1; $0.hitCount = 70 }
            let worker = AtlasIndexedSearch(transport: fixture.transport)
            let report = try run(worker, reports: reports)
            precondition(report.mode == .expression && report.complete && report.hits.count == 70)
            precondition(report.expression?.excludedFiles == 1 && report.indexStatus?.uncapturedFiles == 0,
                "Expression-filtered files are excluded, not uncaptured")
            precondition(report.indexStatus!.verificationBytes > report.indexStatus!.capturedBytes,
                "Predicate verification can rescan one capture under its own work budget")
            precondition(reports.values.allSatisfy { $0.mode == .expression }, "Index-build reports retain requested query mode")
            let target = report.capture!.target(report.hits[0])!
            precondition(target.mode == .expression && target.needle == "x")
            _ = try run(worker, query: "literal source text", mode: .text)
            let again = try run(worker, query: "x another -excluded")
            precondition(again.indexStatus?.reused == true && again.streamID != report.streamID
                && fixture.count("open:") == 1 && fixture.count("build-begin:") == 1,
                "Expression/literal switches reuse captured bytes and replace query identities")
            _ = try target.openReader(100)
            precondition(fixture.count("source:") == 1 && fixture.count("open:") == 1,
                "Earlier expression target preserves its exact capture after later queries")
        }
        do {
            let fixture = IndexedFixture()
            fixture.configure { $0.expressionExcluded = 2; $0.hitCount = 0 }
            let report = try run(AtlasIndexedSearch(transport: fixture.transport))
            precondition(report.complete && report.hits.isEmpty && report.expression?.scopeFiles == 0
                && report.indexStatus?.verifiedFiles == 0 && report.indexStatus?.uncapturedFiles == 0,
                "An explicitly empty expression scope completes without a content scan")
        }
        do {
            let fixture = IndexedFixture()
            fixture.configure { $0.expressionPrimary = "i\u{307}"; $0.insensitiveText = "İ"; $0.insensitiveRangeBytes = 2; $0.hitCount = 1 }
            let report = try run(AtlasIndexedSearch(transport: fixture.transport), query: "\"i\u{307}\" required", caseSensitive: false)
            let target = report.capture!.target(report.hits[0])!
            precondition(!report.caseSensitive && target.mode == .expression && target.needle.utf8.elementsEqual("İ".utf8),
                "Insensitive expression activation must use exact matched source spelling")
        }
        do {
            let fixture = IndexedFixture(); var transport = fixture.transport; transport.expressionQueryBegin = nil
            do { _ = try run(AtlasIndexedSearch(transport: transport)); preconditionFailure("Legacy transport interpreted expression literally") }
            catch AtlasSearchError.unavailable { }
            precondition(fixture.count("create:") == 0, "Unsupported expression capability refused before capture")
        }
        for (key, value) in [("predicate_scope", "window"), ("primary_needle", "wrong"),
            ("scanned_input_bytes", "0"), ("max_scan_bytes", "33554432"),
            ("expression_scope_files", "0"), ("expression_excluded_files", "01")] {
            let fixture = IndexedFixture()
            fixture.patch { phase, original in
                var out = original; if phase == "query-step" { out[key] = value }; return out
            }
            reject("indexed worker admitted \(key)") { _ = try run(AtlasIndexedSearch(transport: fixture.transport)) }
        }
        for missing in ["case_sensitive", "matched_text"] {
            let fixture = IndexedFixture()
            fixture.patch { phase, original in
                var out = original
                if phase == "query-step" {
                    if missing == "case_sensitive" { out.removeValue(forKey: missing) }
                    else {
                        var rows = out["hits"] as! [[String: Any]]; rows[0].removeValue(forKey: missing); out["hits"] = rows
                    }
                }
                return out
            }
            reject("indexed expression omitted \(missing)") { _ = try run(AtlasIndexedSearch(transport: fixture.transport)) }
        }
        do {
            let fixture = IndexedFixture()
            fixture.patch { phase, original in
                var out = original
                if phase == "query-step" { out["needle"] = "y"; out["primary_needle"] = "y" }
                return out
            }
            reject("primary term changed within one query") { _ = try run(AtlasIndexedSearch(transport: fixture.transport)) }
        }
        print("Expression policy, live/indexed workers, primary activation, budgets, paging and refusal regressions passed.")
    }
}
