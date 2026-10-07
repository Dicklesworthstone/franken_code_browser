// Full production worker, typed JSON decoder, captured-source handoff and
// original owner retirement. Scripted envelopes are NOT actual Rust execution.
import Foundation
import Dispatch

@main private enum AtlasIndexedSearchTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ value: @autoclosure () -> Bool, _ label: String) {
        precondition(value(), label); checks += 1
    }
    @MainActor private static func run(_ worker: AtlasIndexedSearch, query: String = "x", root: String = "/repo",
                                      access: AnyObject? = nil, flag: AtlasSearchCancellation = AtlasSearchCancellation(),
                                      reports: IndexReports = IndexReports()) throws -> AtlasSearchReport {
        try worker.run(root: root, query: query, cancellation: flag, access: AtlasSearchAccessLease(access), progress: reports.offer)
    }
    @MainActor private static func wait(_ condition: () -> Bool) {
        let deadline = Date().addingTimeInterval(5)
        while !condition() && Date() < deadline { Thread.sleep(forTimeInterval: 0.001) }
        check(condition(), "Timed out awaiting deferred retirement")
    }
    @MainActor private static func reject(_ phase: String, _ key: String, value: String) throws {
        let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
        f.patch { p, object in var object = object; if p == phase { object[key] = value }; return object }
        do { _ = try run(w); preconditionFailure("Accepted bad \(phase).\(key)") }
        catch AtlasSearchError.invalidResponse { checks += 1 }
    }
    @MainActor static func main() throws {
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport), reports = IndexReports()
            f.configure { $0.hitCount = 70 }
            let first = try run(w, reports: reports)
            check(first.complete && first.hits.count == 70, "First query returns all paginated hits")
            check(first.hits.map(\.id) == Array(0..<70), "Occurrence identity survives extra pages")
            check(first.indexStatus?.reused == false && first.indexStatus?.capturedFiles == 2, "First query reports a new captured index")
            check(first.indexStatus?.skippedFiles == 1 && first.indexStatus?.verifiedFiles == 1, "Actual verification/skip counters are exposed")
            check(first.summary.contains("Refresh to include file edits"), "Snapshot freshness cannot be mistaken for a live scan")
            check(reports.values.contains { $0.indexStatus?.building == true && $0.hits.isEmpty && !$0.complete }, "Index preparation is not an empty complete query")
            check(reports.values.contains { $0.indexStatus?.building == false && $0.isInProgress && $0.hits.count == 70 }, "Indexed hits publish progressively")
            check(reports.values.filter { $0.indexStatus?.building == false }.allSatisfy { $0.streamID == first.streamID }, "One query keeps one row identity")
            let oldTarget = first.capture!.target(first.hits[0])!
            let second = try run(w, query: "new exact query")
            check(second.indexStatus?.reused == true && second.streamID != first.streamID, "Second query reuses source, not result identities")
            check(f.count("create:") == 1 && f.count("open:") == 1 && f.count("build-begin:") == 1 && f.count("build-step:") == 2,
                  "Repeated queries do not rediscover, recapture or rebuild")
            check(f.count("query-begin:") == 2 && f.count("query-step:") == 4 && f.count("page:") == 2, "Only new query verification and needed pages repeat")
            check(f.trace.contains { $0.hasPrefix("query-begin:") && $0.hasSuffix(":3") }, "Monotone full-width query identities")
            let info = try oldTarget.openReader(100)
            check(info.contains("host-supplied") && f.count("source:") == 1, "An earlier query's source still imports after result replacement")
            check(f.count("open:") == 1 && f.count("build-step:") == 2, "Source activation does not read the live file")
            let forged = SearchHit(id: 0, path: "a.rs", sourcePath: "a.rs", start: 2, end: 3,
                captureSHA256: first.hits[0].captureSHA256, captureByteLength: 4096)
            check(first.capture?.target(forged) == nil, "Reused row numbers do not borrow capture witnesses")
        }
        do {
            let f = IndexedFixture()
            var w: AtlasIndexedSearch? = AtlasIndexedSearch(transport: f.transport)
            var first: AtlasSearchReport? = try run(w!)
            var target = first!.capture!.target(first!.hits[0])
            let firstID = first!.streamID
            first = nil; w = nil
            check(f.count("close:") == 0, "Selected target pins old index through refresh")
            f.configure { $0.digest = String(repeating: "b", count: 64) }
            let newer = AtlasIndexedSearch(transport: f.transport)
            let second = try run(newer)
            check(second.streamID != firstID && second.hits[0].captureSHA256 == String(repeating: "b", count: 64), "Explicit refresh establishes a new source owner")
            _ = try target!.openReader(100)
            check(f.count("create:") == 2 && f.count("source:") == 1, "Old source remains importable from old owner after refresh")
            target = nil; wait { f.count("close:") == 1 }
            check(f.count("retirement-failed") == 0, "Old index retires without affecting replacement")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.configure { $0.fallback = true }
            let r = try run(w)
            check(r.complete && r.hits.count == 2, "Uncovered gram segments retain fallback matches")
            check(r.indexStatus?.verifiedFiles == 1, "Fallback verification is recorded")
        }
        for mode in 0..<3 {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.configure { c in
                if mode == 0 { c.unavailable = 1 }
                if mode == 1 { c.pending = 1 }
                if mode == 2 { c.discovery = false }
            }
            let r = try run(w)
            check(!r.complete && !r.isInProgress && r.hits.count == 2, "Partial catalog/captures remain useful but never exhaustive")
            let rerun = try run(w)
            check(rerun.indexStatus?.reused == true, "Partial index is explicit and reusable without silently excluding coverage")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.configure { $0.files = 0 }
            let r = try run(w)
            check(r.complete && r.hits.isEmpty && r.indexStatus?.capturedFiles == 0, "Empty closed catalog finalizes without a phantom source")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.configure { $0.pathHex = "ff2e7273" }
            let r = try run(w)
            check(r.hits[0].sourcePath == nil && r.capture?.target(r.hits[0]) == nil, "Non-UTF-8 path stays visible without fabricated native opener")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            let canceled = AtlasSearchCancellation(); canceled.cancel()
            do { _ = try run(w, flag: canceled); preconditionFailure("Pre-cancel admitted") }
            catch AtlasSearchError.canceled { checks += 1 }
            check(f.count("create:") == 0, "Canceled query does not create an index")
            do { _ = try run(w, query: ""); preconditionFailure("Invalid input admitted") }
            catch AtlasSearchError.invalidRequest { checks += 1 }
            check(f.count("create:") == 0, "Invalid input is inert")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport), flag = AtlasSearchCancellation()
            f.block { p in if p == "build-step" { flag.cancel() } }
            do { _ = try run(w, flag: flag); preconditionFailure("Build cancellation ignored") }
            catch AtlasSearchError.canceled { checks += 1 }
            wait { f.count("close:") == 1 }
            f.block(nil)
            let retry = try run(w)
            check(retry.indexStatus?.reused == false && f.count("create:") == 2, "Canceled candidate is never cached as ready")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            _ = try run(w)
            let flag = AtlasSearchCancellation()
            f.block { p in if p == "query-step" { flag.cancel() } }
            do { _ = try run(w, query: "canceled", flag: flag); preconditionFailure("Query cancellation ignored") }
            catch AtlasSearchError.canceled { checks += 1 }
            f.block(nil)
            let next = try run(w, query: "after canceled")
            check(next.indexStatus?.reused == true && f.count("create:") == 1, "Canceled query preserves accepted index")
            check(f.trace.contains { $0.hasPrefix("query-begin:") && $0.hasSuffix(":4") }, "Canceled attempt cannot reuse generation")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport), flags = IndexReports()
            let flag = AtlasSearchCancellation()
            do {
                _ = try w.run(root: "/repo", query: "x", cancellation: flag, access: AtlasSearchAccessLease(nil)) { r in
                    flags.offer(r)
                    if !r.hits.isEmpty { flag.cancel() }
                }
                preconditionFailure("Progress cancellation ignored")
            } catch AtlasSearchError.canceled { checks += 1 }
            check(f.count("query-step:") == 1, "Cancel after first hits prevents another step")
            let rerun = try run(w)
            check(rerun.indexStatus?.reused == true, "Index survives stopping after useful early matches")
        }
        do {
            final class Grant {}
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport), a = Grant(), b = Grant()
            _ = try run(w, access: a); _ = try run(w, access: a)
            check(f.count("create:") == 1, "Same raw root and grant reuse index")
            _ = try run(w, access: b)
            check(f.count("create:") == 2, "New grant does not borrow old native access")
            _ = try run(w, root: "/re\u{301}po", access: b)
            _ = try run(w, root: "/répo", access: b)
            check(f.count("create:") == 4, "Canonically equivalent root strings remain distinct byte identities")
        }
        for (phase, key, value) in [
            ("open", "owner", "1"), ("open", "layout_revision", "0"), ("open", "catalogued_files", "4097"),
            ("build-begin", "index_generation", "2"), ("build-step", "source_manifest", "2"),
            ("build-step", "captured_files", "03"), ("build-step", "indexed_source_bytes", "33554433"),
            ("query-begin", "query_generation", "1"), ("query-step", "query_generation", "99"),
            ("query-step", "capture_manifest", "3"), ("query-step", "index_generation", "2"),
            ("query-step", "needle", "not the query"), ("query-step", "read_calls", "1"),
            ("query-step", "source_bytes_read", "1"), ("query-step", "search_strategy", "live-capture-scan"),
            ("query-step", "step_count", "99"), ("query-step", "verified_files", "2"),
            ("query-step", "pending_files", "0"), ("query-step", "retained_hits", "1001"),
            ("query-step", "verification_source_bytes", "999999"), ("query-step", "stop_reason", "unrecognized")
        ] { try reject(phase, key, value: value) }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.patch { p, object in var o = object; if p == "query-step" { o["search_complete"] = true }; return o }
            do { _ = try run(w); preconditionFailure("Early exhaustive flag accepted") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.configure { $0.hitCount = 70 }
            f.patch { p, object in var o = object; if p == "page" { o["matches_seen"] = "71" }; return o }
            do { _ = try run(w); preconditionFailure("Page from different coverage accepted") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
        }
        for field in ["query_generation", "hit_id", "original_range", "source_reopened", "capture_sha256", "capture_manifest", "reader_owner"] {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            let r = try run(w), t = r.capture!.target(r.hits[0])!
            f.patch { p, object in
                var o = object
                if p == "source" {
                    if field == "source_reopened" { o[field] = true }
                    else { o[field] = "wrong" }
                }
                return o
            }
            do { _ = try t.openReader(100); preconditionFailure("Corrupt source handoff accepted: \(field)") }
            catch { checks += 1 }
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.patch { phase, object in
                var o = object
                if phase == "query-step", o["step_count"] as? String == "2" {
                    var rows = o["hits"] as! [[String: Any]]
                    rows[0]["capture_sha256"] = String(repeating: "b", count: 64); o["hits"] = rows
                }
                return o
            }
            do { _ = try run(w); preconditionFailure("Changed repeated source witness accepted") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
        }
        print("AtlasIndexedSearchTests: \(checks) checks passed")
    }
}
