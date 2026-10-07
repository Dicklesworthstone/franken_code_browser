import Foundation
import Dispatch

private final class RootLease {
    let fixture: AtlasFileFinderFixture
    init(_ fixture: AtlasFileFinderFixture) { self.fixture = fixture }
    deinit { fixture.add("grantReleased") }
}
@main private enum AtlasFileFinderTests {
    @MainActor static var checks = 0
    @MainActor static func check(_ value: @autoclosure () -> Bool, _ message: String) {
        precondition(value(), message); checks += 1
    }
    @MainActor static func rejected(_ label: String, action: String = "find", change: @escaping @Sendable ([String: Any]) -> [String: Any]) throws {
        let fixture = AtlasFileFinderFixture()
        fixture.transform = { command, value in command == action ? change(value) : value }
        let finder = try AtlasFileFinder(root: "/repo", transport: fixture.transport)
        do {
            _ = try finder.find(AtlasFileQuery(text: "src"), cancellation: AtlasSearchCancellation())
            preconditionFailure("Invalid response accepted: " + label)
        } catch AtlasFileFinderError.invalidResponse { checks += 1 }
    }
    @MainActor static func wait(_ predicate: () -> Bool) async {
        for _ in 0..<1000 {
            if predicate() { return }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        preconditionFailure("Retirement did not finish")
    }
    @MainActor static func main() async throws {
        for text in ["", String(repeating: "x", count: 257), "a\0b"] {
            do { _ = try AtlasFileQuery(text: text); preconditionFailure("Invalid input accepted") }
            catch AtlasFileFinderError.invalidInput { checks += 1 }
        }
        let whitespace = try AtlasFileQuery(text: " ")
        check(whitespace.text == " ", "Literal whitespace is not silently trimmed")
        let composed = try AtlasFileQuery(text: "é"), decomposed = try AtlasFileQuery(text: "e\u{301}")
        check(composed != decomposed, "Exact query identity does not normalize Unicode")
        do {
            let fixture = AtlasFileFinderFixture()
            let finder = try AtlasFileFinder(root: "/repo", transport: fixture.transport)
            check(fixture.count("create") == 0, "Construction is inert")
            let flag = AtlasSearchCancellation()
            let first = try finder.find(AtlasFileQuery(text: "src"), cancellation: flag)
            check(first.rows.count == 256 && first.matchesSeen == 300 && first.truncated && first.complete,
                  "Complete count and bounded top results are separate states")
            check(fixture.count("page") == 2 && fixture.count("open") == 1, "All bounded result pages are delivered")
            check(first.rows.map(\.id) == Array(1...256).map(UInt64.init), "No reordering or native re-ranking")
            let opened = try finder.select(first.rowID(200), cancellation: flag)
            check(opened.path == "src/200.rs" && opened.root == "/repo", "Selection opens raw identity, not escaped display label")
            let next = try finder.find(AtlasFileQuery(text: "src/2", mode: .prefix, matchCase: true), cancellation: flag)
            check(next.generation == first.generation + 1 && next.catalog == first.catalog, "Refinements use monotone generations on the same catalog")
            check(fixture.count("create") == 1 && fixture.count("open") == 1, "Typing and mode changes never rediscover the directory")
            check(first.id != next.id, "Old ranked row identities are not reused")
            let selectCount = fixture.count("select")
            do { _ = try finder.select(first.rowID(200), cancellation: flag); preconditionFailure("Stale selection admitted") }
            catch AtlasFileFinderError.invalidInput { checks += 1 }
            check(fixture.count("select") == selectCount, "Stale rows are rejected before foreign selection")
            _ = try finder.find(AtlasFileQuery(text: "src/2", mode: .exact), cancellation: flag)
            check(fixture.count("open") == 1, "Exact mode reuses keys without a false files/candidates equality assumption")
        }
        do {
            let fixture = AtlasFileFinderFixture(); fixture.matches = 0
            let finder = try AtlasFileFinder(root: "/repo", transport: fixture.transport)
            let report = try finder.find(AtlasFileQuery(text: "missing"), cancellation: AtlasSearchCancellation())
            check(report.complete && report.rows.isEmpty && !report.truncated && fixture.count("page") == 0, "True zero matches has a complete, empty result")
        }
        do {
            let fixture = AtlasFileFinderFixture(); fixture.discoveryComplete = false; fixture.matches = 2
            let finder = try AtlasFileFinder(root: "/repo", transport: fixture.transport)
            let report = try finder.find(AtlasFileQuery(text: "src"), cancellation: AtlasSearchCancellation())
            check(!report.complete && report.summary.contains("Discovery is partial"), "Partial catalog is never exhaustive")
        }
        do {
            let fixture = AtlasFileFinderFixture(); fixture.matches = 2
            fixture.path = { $0 == 1 ? Array("é.rs".utf8) : Array("e\u{301}.rs".utf8) }
            let finder = try AtlasFileFinder(root: "/repo", transport: fixture.transport)
            let report = try finder.find(AtlasFileQuery(text: ".rs"), cancellation: AtlasSearchCancellation())
            check(report.rows.count == 2 && report.rows[0].rawPath != report.rows[1].rawPath,
                  "Canonically equivalent filenames remain distinct files")
        }
        do {
            let fixture = AtlasFileFinderFixture(); fixture.matches = 1; fixture.path = { _ in [255, 46, 114, 115] }
            let finder = try AtlasFileFinder(root: "/repo", transport: fixture.transport)
            let report = try finder.find(AtlasFileQuery(text: ".rs"), cancellation: AtlasSearchCancellation())
            check(report.rows.count == 1 && report.rows[0].path == nil, "Non-UTF-8 filename stays visible with no fabricated path")
            do { _ = try finder.select(report.rowID(1), cancellation: AtlasSearchCancellation()); preconditionFailure("Unsupported path opened") }
            catch AtlasFileFinderError.invalidInput { checks += 1 }
        }
        for key in ["owner", "source_manifest", "layout_revision", "query_generation", "retained_hits", "matches_seen", "files_examined", "work_units"] {
            try rejected("noncanonical " + key) { var value = $0; value[key] = "01"; return value }
        }
        for (key, value) in [("schema", "wrong"), ("mode", "regex"), ("case", "folded"), ("query_hex", "00"), ("owner", "42"), ("layout_revision", "2"), ("query_generation", "2")] {
            try rejected(key) { var object = $0; object[key] = value; return object }
        }
        try rejected("source work in filename search") { var value = $0; value["source_payload_read"] = true; return value }
        try rejected("rewound page cursor") { var value = $0; value["next_offset"] = "0"; return value }
        try rejected("short intermediate page") { var value = $0; value["hits"] = []; return value }
        try rejected("oversized array") { var value = $0; let rows = value["hits"] as! [[String: Any]]; value["hits"] = Array(repeating: rows[0], count: 129); return value }
        try rejected("duplicate file") { var value = $0; var rows = value["hits"] as! [[String: Any]]; rows[1] = rows[0]; value["hits"] = rows; return value }
        try rejected("changed page manifest", action: "page") { var value = $0; value["source_manifest"] = "2"; return value }
        try rejected("changed page counts", action: "page") { var value = $0; value["matches_seen"] = "299"; return value }
        try rejected("changed page completeness", action: "page") { var value = $0; value["search_complete"] = false; return value }
        for raw in ["2f61", "2e2e2f61", "612f2e2f62", "612f2f62", "6100", "A0", "1", ""] {
            try rejected("invalid raw path") {
                var value = $0; var rows = value["hits"] as! [[String: Any]]
                rows[0]["path"] = ["encoding": "unix-bytes", "hex": raw, "display": "harmless label"]
                value["hits"] = rows; return value
            }
        }
        for field in ["scope", "retention", "layout_revision"] {
            try rejected("invalid open " + field, action: "open") { var value = $0; value[field] = "wrong"; return value }
        }
        do {
            let fixture = AtlasFileFinderFixture()
            let finder = try AtlasFileFinder(root: "/repo", transport: fixture.transport)
            let report = try finder.find(AtlasFileQuery(text: "src"), cancellation: AtlasSearchCancellation())
            fixture.transform = { action, object in
                var object = object
                if action == "select" { var row = object["selection"] as! [String: Any]; row["file_id"] = "2"; object["selection"] = row }
                return object
            }
            do { _ = try finder.select(report.rowID(1), cancellation: AtlasSearchCancellation()); preconditionFailure("Foreign file selected") }
            catch AtlasFileFinderError.invalidResponse { checks += 1 }
        }
        for point in ["open", "find"] {
            let fixture = AtlasFileFinderFixture(); fixture.cancelOn = point
            let finder = try AtlasFileFinder(root: "/repo", transport: fixture.transport)
            do { _ = try finder.find(AtlasFileQuery(text: "src"), cancellation: AtlasSearchCancellation()); preconditionFailure("Cancellation ignored") }
            catch AtlasFileFinderError.canceled { checks += 1 }
            fixture.cancelOn = nil
            let report = try finder.find(AtlasFileQuery(text: "src"), cancellation: AtlasSearchCancellation())
            check(report.generation == 2, "Canceled attempts do not reuse query generations")
        }
        do {
            let fixture = AtlasFileFinderFixture()
            let finder = try AtlasFileFinder(root: "/repo", transport: fixture.transport)
            let flag = AtlasSearchCancellation(); flag.cancel()
            do { _ = try finder.find(AtlasFileQuery(text: "src"), cancellation: flag); preconditionFailure("Pre-cancel ignored") }
            catch AtlasFileFinderError.canceled { checks += 1 }
            check(fixture.count("create") == 0, "Pre-cancel creates no native owner")
        }
        do {
            let fixture = AtlasFileFinderFixture(); fixture.closeFailures = 2
            var lease: RootLease? = RootLease(fixture)
            weak var weakLease = lease
            var finder: AtlasFileFinder? = try AtlasFileFinder(root: "/repo", accessLease: lease, transport: fixture.transport)
            lease = nil
            _ = try finder!.find(AtlasFileQuery(text: "src"), cancellation: AtlasSearchCancellation())
            check(weakLease != nil && fixture.count("close") == 0, "Root access retained for reusable catalog lifetime")
            finder = nil
            await wait { fixture.count("close") == 3 && fixture.count("grantReleased") == 1 }
            check(fixture.count("retirementFailure") == 0, "Catalog retires off caller with bounded contention retries")
        }
        do {
            let fixture = AtlasFileFinderFixture(); fixture.refuseCreate = true
            let finder = try AtlasFileFinder(root: "/repo", transport: fixture.transport)
            do { _ = try finder.find(AtlasFileQuery(text: "src"), cancellation: AtlasSearchCancellation()); preconditionFailure("Refused handle used") }
            catch AtlasFileFinderError.unavailable { checks += 1 }
            check(fixture.count("open") == 0, "Handle refusal performs no discovery")
        }
        print("AtlasFileFinderTests: \(checks) checks passed")
    }
}
