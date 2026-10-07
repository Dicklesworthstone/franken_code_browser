import Foundation
import Dispatch

/// Fixed envelopes at the transport boundary, not another path matcher.
final class AtlasFileFinderFixture: @unchecked Sendable {
    private let lock = NSLock()
    private var counters: [String: Int] = [:]
    private var lastQuery: [UInt64: (UInt64, AtlasFileQuery)] = [:]
    var files = 300
    var matches = 300
    var discoveryComplete = true
    var cancelOn: String?
    var refuseCreate = false
    var holdFind: DispatchSemaphore?
    var holdSelect: DispatchSemaphore?
    var entered = DispatchSemaphore(value: 0)
    var transform: @Sendable (String, [String: Any]) -> [String: Any] = { _, value in value }
    var path: @Sendable (Int) -> [UInt8] = { Array("src/\($0).rs".utf8) }
    var malformedJSON: String?
    var closeFailures = 0

    func count(_ key: String) -> Int { lock.lock(); defer { lock.unlock() }; return counters[key, default: 0] }
    func add(_ key: String) { lock.lock(); counters[key, default: 0] += 1; lock.unlock() }
    private func record(_ key: String) -> Int {
        lock.lock(); defer { lock.unlock() }; counters[key, default: 0] += 1; return counters[key, default: 0]
    }
    var transport: AtlasFileFinderTransport {
        .init(create: {
            let n = self.record("create"); return self.refuseCreate ? 0 : UInt64(40 + n)
        }, open: { handle, root, limit, flag in
            precondition(!root.isEmpty && limit == 20_000)
            self.add("open")
            if self.cancelOn == "open" { flag.cancel(); return nil }
            return self.json("open", ["schema": "fcb.atlas-session/1", "status": "ok", "command": "info",
                "owner": String(handle), "layout_revision": "1", "catalogued_files": String(self.files),
                "scope": "all", "retention": "frozen-catalog-and-spatial-index", "discovery_complete": self.discoveryComplete])
        }, find: { handle, generation, query, limit, flag in
            precondition(limit == 256)
            self.lock.lock(); self.lastQuery[handle] = (generation, query); self.lock.unlock()
            self.add("find"); self.entered.signal(); self.holdFind?.wait()
            if self.cancelOn == "find" { flag.cancel(); return nil }
            return self.result(handle, generation, query, start: 0, limit: 64, command: "find")
        }, page: { handle, generation, start, limit in
            precondition(limit == 128)
            self.add("page")
            self.lock.lock(); let query = self.lastQuery[handle]!.1; self.lock.unlock()
            return self.result(handle, generation, query, start: Int(start), limit: Int(limit), command: "page")
        }, select: { handle, generation, file in
            self.add("select"); self.entered.signal(); self.holdSelect?.wait()
            return self.json("select", ["schema": "fcb.atlas-paths/1", "status": "ok", "command": "select",
                "owner": String(handle), "source_manifest": "1", "layout_revision": "1",
                "query_generation": String(generation), "source_payload_read": false, "selection": self.row(Int(file))])
        }, close: { _ in
            let n = self.record("close")
            return n > self.closeFailures
        }, retirementFailed: { self.add("retirementFailure") })
    }
    func row(_ file: Int) -> [String: Any] {
        ["file_id": String(file), "node": String(file), "match_kind": "filename-prefix",
         "path": ["encoding": "unix-bytes", "hex": Self.hex(path(file)), "display": "file \(file)"]]
    }
    private func result(_ handle: UInt64, _ generation: UInt64, _ query: AtlasFileQuery,
                        start: Int, limit: Int, command: String) -> String? {
        let retained = min(256, matches), end = min(start + limit, retained)
        let rows: [[String: Any]] = start < end ? ((start + 1)...end).map(row) : []
        return json(command, ["schema": "fcb.atlas-paths/1", "status": "ok", "command": command,
            "owner": String(handle), "source_manifest": "1", "layout_revision": "1", "query_generation": String(generation),
            "query_hex": Self.hex(Array(query.text.utf8)), "mode": query.mode.wire,
            "case": query.matchCase ? "sensitive" : "unicode-lowercase", "source_payload_read": false,
            "search_complete": discoveryComplete, "truncated": matches > retained,
            "retained_hits": String(retained), "matches_seen": String(matches), "files_examined": String(files),
            // Component posting candidates can outnumber distinct files.
            "candidates_examined": String(query.mode == .fuzzy ? files : 2 * files), "work_units": "1234",
            "hits": rows, "next_offset": end < retained ? String(end) : NSNull()])
    }
    private func json(_ command: String, _ value: [String: Any]) -> String? {
        if let malformedJSON { return malformedJSON }
        let result = transform(command, value)
        return String(decoding: try! JSONSerialization.data(withJSONObject: result), as: UTF8.self)
    }
    static func hex(_ bytes: [UInt8]) -> String { bytes.map { String(format: "%02x", $0) }.joined() }
}
