// Execute the production worker/codec against fixed C symbols. No Rust or OS I/O.
import Foundation

@_silgen_name("fixture_reset") private func resetFixture()
@_silgen_name("fixture_set") private func setFixture(_ kind: UInt32, _ index: UInt32, _ text: UnsafePointer<CChar>)
@_silgen_name("fixture_count") private func fixtureCount(_ key: UInt32) -> UInt32
@_silgen_name("fixture_cancel_step") private func cancelStep()
@_silgen_name("fixture_refuse_create") private func refuseCreate()
@_cdecl("fixture_mark_canceled") public func markCanceled(_ context: UnsafeMutableRawPointer?) {
    guard let context else { preconditionFailure("Missing callback lifetime") }
    Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().cancel()
}
private final class Reports: @unchecked Sendable {
    private let lock = NSLock()
    private var storage: [AtlasSearchReport] = []
    func append(_ report: AtlasSearchReport) { lock.lock(); storage.append(report); lock.unlock() }
    func clear() { lock.lock(); storage = []; lock.unlock() }
    var values: [AtlasSearchReport] { lock.lock(); defer { lock.unlock() }; return storage }
}

@main private enum AtlasNativeSearchTests {
    private static func hit(_ id: Int, path: String = "612e7273") -> [String: Any] {
        ["hit_id": String(id), "file_id": "1", "source_revision": "1",
         "capture_sha256": String(repeating: "a", count: 64), "capture_byte_length": "1000",
         "original_range": ["start": String(id - 1), "end": String(id)],
         "path": ["encoding": "unix-bytes", "hex": path, "display": "a.rs"]]
    }
    private static func page(_ command: String, step: Int, total: Int = 0, start: Int = 0,
                             running: Bool = true, complete: Bool = false, catalogued: Int = 2) -> [String: Any] {
        let end = min(total, start + (command == "page" ? 128 : 64))
        let rows: [[String: Any]] = start < end ? ((start + 1)...end).map { hit($0) } : []
        return ["schema": "fcb.atlas-search/1", "status": "ok", "command": command,
            "owner": "11", "source_manifest": "1", "layout_revision": "1", "query_generation": "1",
            "needle": "x", "mode": "exact-decoded-literal", "search_strategy": "live-capture-scan",
            "discovery_complete": true, "search_in_progress": running, "search_complete": complete,
            "truncated": false, "stop_reason": running ? NSNull() : "all-files-examined",
            "catalogued_files": String(catalogued), "examined_files": String(min(step, catalogued)),
            "scanned_files": String(min(step, catalogued)), "unavailable_files": "0",
            "pending_files": String(max(0, catalogued - step)), "matches_seen": String(total),
            "retained_hits": String(total), "displayed_hits": String(total), "step_count": String(step),
            "hits": rows, "next_offset": end < total ? String(end) : NSNull()]
    }
    private static func importReply() -> [String: Any] {
        var reply = hit(1)
        reply.merge(["schema": "fcb.atlas-search/1", "status": "ok", "command": "open-reader",
            "owner": "11", "source_manifest": "1", "layout_revision": "1", "query_generation": "1",
            "reader_owner": "100", "source_reopened": false, "source_observation": "retained-search-capture",
            "reader": ["schema": "fcb.reader-session/1", "status": "ok", "command": "info",
                "owner": "100", "file_id": "1", "source_revision": "1", "captured_bytes": "1000",
                "path": ["encoding": "unix-bytes", "hex": "612e7273", "display": "a.rs"],
                "capture_origin": "host-supplied", "initial_source_bytes_read": "0", "initial_read_calls": "0",
                "additional_source_bytes_read": "0", "native_presented": false, "encoding": "utf16le"]], uniquingKeysWith: { _, new in new })
        return reply
    }
    private static func install(_ kind: UInt32, _ object: [String: Any], index: UInt32 = 0) throws {
        let data = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
        String(decoding: data, as: UTF8.self).withCString { setFixture(kind, index, $0) }
    }
    private static func setup() throws {
        resetFixture()
        try install(0, ["status": "ok"])
        try install(1, page("begin", step: 0))
        try install(2, page("step", step: 1, total: 70))
        try install(3, page("page", step: 1, total: 70, start: 64))
        try install(2, page("step", step: 2, total: 70, running: false, complete: true), index: 1)
        try install(4, importReply())
    }
    // Last-owner retirement is intentionally asynchronous. Fixture counters are
    // atomic; never reset them until the previous session has actually closed.
    private static func retired() {
        let deadline = Date().addingTimeInterval(5)
        while fixtureCount(5) == 0 && Date() < deadline { Thread.sleep(forTimeInterval: 0.001) }
        precondition(fixtureCount(5) == 1, "Retained source did not retire exactly once")
    }
    @MainActor private static var checks = 0
    @MainActor private static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        precondition(condition(), message); checks += 1
    }
    @MainActor private static func rejected(_ label: String, _ change: () throws -> Void) throws {
        try setup(); try change()
        do {
            _ = try AtlasNativeSearch.run(root: "/project", query: "x", cancellation: AtlasSearchCancellation())
            preconditionFailure("Accepted invalid response: " + label)
        } catch AtlasSearchError.invalidResponse {}
        retired()
        check(fixtureCount(5) == 1, label + ": handle closed on rejection")
        check(fixtureCount(6) == fixtureCount(7), label + ": every C allocation released")
    }
    @MainActor static func main() throws {
        try setup()
        let progress = Reports()
        var terminal: AtlasSearchReport? = try AtlasNativeSearch.runProgressive(root: "/project", query: "x",
            cancellation: AtlasSearchCancellation(), progress: progress.append)
        check(terminal!.complete && !terminal!.isInProgress && terminal!.hits.count == 70, "Complete paginated result")
        check(terminal!.hits.map(\.id) == Array(0..<70), "Stable contiguous occurrence IDs")
        check(progress.values.contains { $0.isInProgress && $0.hits.count == 70 }, "Matches publish before final file")
        check(progress.values.allSatisfy { !$0.complete && $0.streamID == terminal!.streamID }, "Coverage and identity remain independent")
        check(fixtureCount(1) == 1 && fixtureCount(2) == 1 && fixtureCount(3) == 2,
              "One discovery, one admission, one step per file")
        check(fixtureCount(4) == 1, "Later progress does not refetch already retained pages")
        let firstID = terminal!.streamID
        var target = terminal!.capture?.target(terminal!.hits[0])
        check(target != nil && fixtureCount(5) == 0, "Terminal report retains an importable exact source")
        terminal = nil; progress.clear()
        check(fixtureCount(5) == 0, "Selected target outlives all report pages")
        let info = try target!.openReader(100)
        check(info.contains("utf16le") && fixtureCount(8) == 1, "Actual worker capability imports captured UTF-16 metadata")
        check(fixtureCount(1) == 1 && fixtureCount(3) == 2, "Activation neither rediscovers nor repeats source search")
        target = nil; retired()
        check(fixtureCount(5) == 1 && fixtureCount(6) == fixtureCount(7), "All retained/C resources retired")

        try setup()
        var again: AtlasSearchReport? = try AtlasNativeSearch.run(root: "/project", query: "x", cancellation: AtlasSearchCancellation())
        check(again!.streamID != firstID, "Repeated query starts a new stream")
        again = nil; retired()
        try setup()
        var partial = page("step", step: 2, total: 70, running: false)
        partial["unavailable_files"] = "1"
        try install(2, partial, index: 1)
        var incomplete: AtlasSearchReport? = try AtlasNativeSearch.run(root: "/project", query: "x", cancellation: AtlasSearchCancellation())
        check(!incomplete!.complete && !incomplete!.isInProgress && incomplete!.unavailableFiles == 1,
              "Finished with unavailable files is partial, not running or exhaustive")
        incomplete = nil; retired()

        try setup()
        let flag = AtlasSearchCancellation()
        do {
            _ = try AtlasNativeSearch.runProgressive(root: "/project", query: "x", cancellation: flag) { report in
                if !report.hits.isEmpty { flag.cancel() }
            }
            preconditionFailure("Cancellation ignored")
        } catch AtlasSearchError.canceled {}
        retired()
        check(fixtureCount(3) == 1 && fixtureCount(5) == 1, "Cancellation between files prevents next read")
        check(fixtureCount(6) == fixtureCount(7), "Cancel after publication releases all strings")
        try setup(); cancelStep()
        do {
            _ = try AtlasNativeSearch.run(root: "/project", query: "x", cancellation: AtlasSearchCancellation())
            preconditionFailure("FFI cancellation ignored")
        } catch AtlasSearchError.canceled {}
        retired()
        check(fixtureCount(5) == 1 && fixtureCount(6) == fixtureCount(7), "Cancellation racing a returned C string releases it")
        try setup()
        let preCanceled = AtlasSearchCancellation(); preCanceled.cancel()
        do {
            _ = try AtlasNativeSearch.run(root: "/project", query: "x", cancellation: preCanceled)
            preconditionFailure("Pre-canceled work admitted")
        } catch AtlasSearchError.canceled {}
        check(fixtureCount(0) == 0, "Pre-canceled work never creates a session")
        try setup(); refuseCreate()
        do {
            _ = try AtlasNativeSearch.run(root: "/project", query: "x", cancellation: AtlasSearchCancellation())
            preconditionFailure("Refused handle admitted")
        } catch AtlasSearchError.unavailable {}
        check(fixtureCount(5) == 0 && fixtureCount(1) == 0, "Refused handle is not opened or closed")
        try setup(); try install(0, ["status": "error"])
        do {
            _ = try AtlasNativeSearch.run(root: "/project", query: "x", cancellation: AtlasSearchCancellation())
            preconditionFailure("Failed discovery admitted")
        } catch AtlasSearchError.unavailable {}
        retired()
        check(fixtureCount(5) == 1 && fixtureCount(2) == 0, "Failed discovery closes before querying")

        for (key, value) in [("owner", "12"), ("source_manifest", "2"), ("layout_revision", "2"),
                             ("query_generation", "2"), ("needle", "y"), ("step_count", "01"),
                             ("mode", "regex"), ("search_strategy", "unknown")] {
            try rejected(key) {
                var bad = page("step", step: 1, total: 70); bad[key] = value
                try install(2, bad)
            }
        }
        try rejected("premature complete") {
            try install(2, page("step", step: 1, total: 70, complete: true))
        }
        try rejected("counter regression") {
            try install(2, page("step", step: 2, total: 69, running: false, complete: true), index: 1)
        }
        try rejected("rewinding cursor") {
            var bad = page("step", step: 1, total: 70); bad["next_offset"] = "0"
            try install(2, bad)
        }
        try rejected("page from different coverage") {
            var bad = page("page", step: 1, total: 70, start: 64); bad["matches_seen"] = "71"
            try install(3, bad)
        }
        try rejected("changed previous witness") {
            var bad = page("step", step: 2, total: 70, running: false, complete: true)
            var rows = bad["hits"] as! [[String: Any]]
            rows[0]["capture_sha256"] = String(repeating: "b", count: 64); bad["hits"] = rows
            try install(2, bad, index: 1)
        }
        for path in ["2f61", "2e2e2f61", "612f2e2f62", "612f2f62", "6100", "612F62", "f"] {
            try rejected("invalid raw path " + path) {
                var bad = page("step", step: 1, total: 1); bad["hits"] = [hit(1, path: path)]
                try install(2, bad)
            }
        }
        for (key, value) in [("capture_sha256", "not-a-digest"), ("capture_byte_length", "0"),
                             ("source_revision", "0"), ("hit_id", "2"), ("file_id", "01")] {
            try rejected(key) {
                var bad = page("step", step: 1, total: 1), row = hit(1)
                row[key] = value; bad["hits"] = [row]; try install(2, bad)
            }
        }
        try setup()
        var nonUTF8 = page("step", step: 1, total: 1)
        nonUTF8["hits"] = [hit(1, path: "ff2e7273")]
        try install(2, nonUTF8)
        var nonUTF8Final = page("step", step: 2, total: 1, running: false, complete: true)
        nonUTF8Final["hits"] = [hit(1, path: "ff2e7273")]
        try install(2, nonUTF8Final, index: 1)
        var raw: AtlasSearchReport? = try AtlasNativeSearch.run(root: "/project", query: "x", cancellation: AtlasSearchCancellation())
        check(raw!.hits.count == 1 && raw!.hits[0].sourcePath == nil, "Non-UTF-8 path remains visible without fabricated opener")
        check(raw!.hits[0].captureSHA256 != nil, "Raw-name row retains its source witness")
        check(raw!.capture?.target(raw!.hits[0]) == nil, "Unsupported native path does not acquire a false text-path opener")
        raw = nil; retired()
        resetFixture()
        print("AtlasNativeSearchTests: \(checks) checks passed")
    }
}
