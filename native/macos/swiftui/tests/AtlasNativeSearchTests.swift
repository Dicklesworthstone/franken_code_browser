// Execute the production worker/codec against fixed C symbols. No Rust or OS I/O.
import Foundation

@_silgen_name("fixture_reset") private func resetFixture()
@_silgen_name("fixture_set") private func setFixture(_ kind: UInt32, _ index: UInt32, _ text: UnsafePointer<CChar>)
@_silgen_name("fixture_count") private func fixtureCount(_ key: UInt32) -> UInt32
@_silgen_name("fixture_cancel_step") private func cancelStep()
@_silgen_name("fixture_refuse_create") private func refuseCreate()
@_silgen_name("fixture_expect_scope") private func expectScope(_ extensions: UnsafePointer<CChar>?, _ filenames: UnsafePointer<CChar>?)
@_silgen_name("fixture_expect_path") private func expectPath(_ kind: UInt64, _ path: UnsafePointer<CChar>)
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
    private static func scopedPage(_ command: String, scope: AtlasSearchFileScope, step: Int,
                                   eligible: Int, total: Int = 0, running: Bool = true,
                                   pathScope: AtlasSearchPathScope = .workspace, hitPath: String? = nil) -> [String: Any] {
        var value = page(command, step: step, total: total, running: running, complete: !running)
        value["search_scope"] = scope.wireKind
        value["scope_extensions"] = scope.extensions ?? ""
        value["scope_filenames"] = scope.filenames ?? ""
        value["eligible_files"] = String(eligible); value["excluded_files"] = String(2 - eligible)
        value["examined_files"] = String(min(step, eligible)); value["scanned_files"] = String(min(step, eligible))
        value["pending_files"] = String(max(0, eligible - step))
        if !pathScope.isWorkspace {
            value["scope_path_kind"] = pathScope.kind.rawValue
            value["scope_path"] = pathScope.path; value["scope_path_hex"] = pathScope.pathHex
        }
        if let hitPath, total > 0 { value["hits"] = (1...min(total, 64)).map { hit($0, path: hitPath) } }
        return value
    }
    private static func setupScoped(_ scope: AtlasSearchFileScope, eligible: Int = 1,
                                    pathScope: AtlasSearchPathScope = .workspace, hitPath: String? = nil) throws {
        resetFixture()
        scope.withCStrings { expectScope($0, $1) }
        if !pathScope.isWorkspace { pathScope.withCString { expectPath($0, $1!) } }
        try install(0, ["status": "ok"])
        try install(1, scopedPage("begin", scope: scope, step: 0, eligible: eligible, pathScope: pathScope))
        try install(2, scopedPage("step", scope: scope, step: 1, eligible: eligible,
            total: eligible == 0 ? 0 : 1, running: false, pathScope: pathScope, hitPath: hitPath))
    }
    private static func runScoped(_ scope: AtlasSearchFileScope,
                                  pathScope: AtlasSearchPathScope = .workspace) throws -> AtlasSearchReport {
        try AtlasNativeSearch.runScoped(input: AtlasSearchInput(root: "/project", query: "x", fileScope: scope, pathScope: pathScope),
            cancellation: AtlasSearchCancellation(), access: AtlasSearchAccessLease(nil), progress: { _ in })
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
        let rustScope = try AtlasFileScope.rust.searchScope()
        try setupScoped(rustScope)
        var scoped: AtlasSearchReport? = try runScoped(rustScope)
        check(scoped!.complete && scoped!.hits.count == 1 && scoped!.fileScope == rustScope, "Scoped native route returns only admitted witnesses")
        check(scoped!.progress?.cataloguedFiles == 1 && scoped!.excludedFiles == 1, "Progress denominator is eligible files")
        check(fixtureCount(9) == 1 && fixtureCount(3) == 1, "Scoped C admission receives both canonical lists")
        scoped = nil; retired()
        let emptyScope = try AtlasSearchFileScope(extensions: [])
        try setupScoped(emptyScope, eligible: 0)
        let empty = try runScoped(emptyScope)
        check(empty.complete && empty.hits.isEmpty && empty.progress?.examinedFiles == 0
            && empty.progress?.cataloguedFiles == 0 && empty.excludedFiles == 2, "Explicit empty scope completes without widening")
        retired()
        for defect in ["missing", "predicate", "denominator", "excluded-hit"] {
            try setupScoped(rustScope)
            if defect == "missing" {
                try install(1, page("begin", step: 0))
            } else {
                var bad = scopedPage("step", scope: rustScope, step: 1, eligible: 1, total: 1, running: false)
                if defect == "predicate" { bad["scope_extensions"] = "py" }
                if defect == "denominator" { bad["excluded_files"] = "0" }
                if defect == "excluded-hit" { bad["hits"] = [hit(1, path: "612e747874")] }
                try install(2, bad)
            }
            do { _ = try runScoped(rustScope); preconditionFailure("Accepted scoped defect: \(defect)") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
            retired()
            check(fixtureCount(6) == fixtureCount(7), "Scope rejection releases every C allocation")
        }
        let directory = try AtlasSearchPathScope(kind: .directory, path: "src")
        let file = try AtlasSearchPathScope(kind: .file, path: "src/a.rs")
        for pathScope in [directory, file] {
            try setupScoped(.all, pathScope: pathScope, hitPath: "7372632f612e7273")
            var result: AtlasSearchReport? = try runScoped(.all, pathScope: pathScope)
            check(result!.complete && result!.hits.count == 1 && result!.hits[0].sourcePath == "src/a.rs"
                && result!.pathScope == pathScope, "Exact requested location survives the C worker")
            check(result!.progress?.cataloguedFiles == 1 && result!.excludedFiles == 1,
                "Path-only scope uses eligible coverage even when all extensions are included")
            check(fixtureCount(10) == 1 && fixtureCount(9) == 0, "Location scope uses its dedicated C admission")
            result = nil; retired()
        }
        try setupScoped(rustScope, pathScope: directory, hitPath: "7372632fff2e7273")
        var rawChild: AtlasSearchReport? = try runScoped(rustScope, pathScope: directory)
        check(rawChild!.hits.count == 1 && rawChild!.hits[0].sourcePath == nil
            && rawChild!.capture?.target(rawChild!.hits[0]) == nil,
            "An opaque descendant is searchable without a fabricated Unicode opener")
        rawChild = nil; retired()
        try setupScoped(.all, eligible: 0, pathScope: file)
        let noFile = try runScoped(.all, pathScope: file)
        check(noFile.complete && noFile.hits.isEmpty && noFile.excludedFiles == 2
            && noFile.pathScope == file, "An absent exact file remains an empty scope")
        retired()
        for defect in ["missing", "kind", "path", "hex", "sibling-prefix", "outside-root", "range"] {
            try setupScoped(.all, pathScope: directory, hitPath: "7372632f612e7273")
            var bad = scopedPage("step", scope: .all, step: 1, eligible: 1, total: 1, running: false,
                pathScope: directory, hitPath: "7372632f612e7273")
            if defect == "missing" {
                for key in ["scope_path_kind", "scope_path", "scope_path_hex"] { bad.removeValue(forKey: key) }
            } else if defect == "kind" { bad["scope_path_kind"] = "file" }
            else if defect == "path" { bad["scope_path"] = "src2" }
            else if defect == "hex" { bad["scope_path_hex"] = "00" }
            else if defect == "sibling-prefix" { bad["hits"] = [hit(1, path: "737263322f612e7273")] }
            else if defect == "outside-root" { bad["hits"] = [hit(1, path: "7372632f2e2e2f612e7273")] }
            else {
                var row = hit(1, path: "7372632f612e7273")
                row["original_range"] = ["start": "0", "end": "1001"]; bad["hits"] = [row]
            }
            try install(2, bad)
            do { _ = try runScoped(.all, pathScope: directory); preconditionFailure("Path-scope defect accepted: \(defect)") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
            retired()
            check(fixtureCount(6) == fixtureCount(7), "Path rejection releases every C allocation")
        }
        let composed = try AtlasSearchPathScope(kind: .directory, path: "café")
        let decomposed = try AtlasSearchPathScope(kind: .directory, path: "cafe\u{301}")
        for defect in ["echo", "row"] {
            try setupScoped(.all, pathScope: composed, hitPath: composed.pathHex + "2f612e7273")
            var bad = scopedPage("step", scope: .all, step: 1, eligible: 1, total: 1, running: false,
                pathScope: composed, hitPath: composed.pathHex + "2f612e7273")
            if defect == "echo" { bad["scope_path"] = decomposed.path }
            else { bad["hits"] = [hit(1, path: decomposed.pathHex + "2f612e7273")] }
            try install(2, bad)
            do { _ = try runScoped(.all, pathScope: composed); preconditionFailure("Normalized path scope accepted") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
            retired()
        }
        try setupScoped(.all, pathScope: directory, hitPath: "7372632f612e7273"); cancelStep()
        do { _ = try runScoped(.all, pathScope: directory); preconditionFailure("Path-scoped cancellation ignored") }
        catch AtlasSearchError.canceled { checks += 1 }
        retired()
        check(fixtureCount(6) == fixtureCount(7), "Racing path-scoped cancellation releases every C allocation")
        resetFixture()
        print("AtlasNativeSearchTests: \(checks) checks passed")
    }
}
