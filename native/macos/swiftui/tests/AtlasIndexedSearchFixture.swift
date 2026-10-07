// Scripted engine envelopes, not another matcher, index, or Rust execution.
import Foundation

final class IndexedFixture: @unchecked Sendable {
    struct Configuration: Sendable {
        var files = 2
        var unavailable = 0
        var pending = 0
        var discovery = true
        var fallback = false
        var hitCount = 2
        var pathHex = "612e7273"
        var digest = String(repeating: "a", count: 64)
        var encoding = "utf8"
    }
    private struct Session {
        let config: Configuration
        var buildStep = 0
        var queryStep = 0
        var generation: UInt64 = 1
        var needle = ""
    }
    private let lock = NSLock()
    private var config = Configuration()
    private var sessions: [UInt64: Session] = [:]
    private var last: UInt64 = 9_007_199_254_740_992
    private var events: [String] = []
    private var mutation: @Sendable (String, [String: Any]) -> [String: Any] = { _, value in value }
    private var blocked: (@Sendable (String) -> Void)?
    func configure(_ change: (inout Configuration) -> Void) { lock.lock(); defer { lock.unlock() }; change(&config) }
    func patch(_ change: @escaping @Sendable (String, [String: Any]) -> [String: Any]) { lock.lock(); mutation = change; lock.unlock() }
    func block(_ work: (@Sendable (String) -> Void)?) { lock.lock(); blocked = work; lock.unlock() }
    func count(_ prefix: String) -> Int { lock.lock(); defer { lock.unlock() }; return events.filter { $0.hasPrefix(prefix) }.count }
    func record(_ event: String) { lock.lock(); events.append(event); lock.unlock() }
    var trace: [String] { lock.lock(); defer { lock.unlock() }; return events }

    var transport: AtlasIndexedSearchTransport {
        AtlasIndexedSearchTransport(create: {
            self.lock.lock(); defer { self.lock.unlock() }
            self.last += 1; self.sessions[self.last] = Session(config: self.config)
            self.events.append("create:\(self.last)"); return self.last
        }, open: { h, _, flag in try self.response("open", h, cancellation: flag) },
        indexBegin: { h, g in precondition(g == 1); return try self.response("build-begin", h) },
        indexStep: { h, g, flag in precondition(g == 1); return try self.response("build-step", h, cancellation: flag) },
        queryBegin: { h, g, index, needle in
            precondition(index == 1)
            return try self.response("query-begin", h, generation: g, needle: needle)
        }, queryStep: { h, g, flag in try self.response("query-step", h, generation: g, cancellation: flag) },
        page: { h, g, start, limit in
            precondition(limit == 128)
            return try self.response("page", h, generation: g, start: Int(start))
        }, sourceReader: { h, reader, index, file, revision in
            precondition(index == 1 && file == 1 && revision == 1)
            return try self.response("source", h, reader: reader)
        }, close: { h in
            self.lock.lock(); defer { self.lock.unlock() }
            precondition(self.sessions.removeValue(forKey: h) != nil)
            self.events.append("close:\(h)"); return true
        }, retirementFailed: { self.record("retirement-failed") })
    }

    private func response(_ phase: String, _ h: UInt64, generation: UInt64 = 1,
                          needle: String = "", start: Int = 0, reader: UInt64 = 100,
                          cancellation: AtlasSearchCancellation? = nil) throws -> String? {
        lock.lock(); let block = blocked; lock.unlock()
        block?(phase)
        if cancellation?.isCanceled == true { throw AtlasSearchError.canceled }
        lock.lock(); defer { lock.unlock() }
        guard var s = sessions[h] else { preconditionFailure("Use after close") }
        events.append("\(phase):\(h):\(generation)")
        if phase == "build-step" { s.buildStep += 1 }
        if phase == "query-begin" { precondition(generation > s.generation); s.generation = generation; s.needle = needle; s.queryStep = 0 }
        if phase == "query-step" || phase == "page" { precondition(s.generation == generation) }
        if phase == "query-step" { s.queryStep += 1 }
        sessions[h] = s
        let c = s.config, captured = c.files - c.pending - c.unavailable
        let totalBytes = captured == 0 ? 0 : 4096 + max(0, captured - 1) * 100
        var value: [String: Any] = ["schema": "fcb.atlas-search/1", "status": "ok", "owner": String(h),
            "source_manifest": "1", "layout_revision": "3", "scope": "all", "workspace_files": String(c.files), "index_generation": "1"]
        if phase == "open" {
            let fields: [String: Any] = ["schema": "fcb.atlas-session/1", "command": "info", "catalogued_files": String(c.files),
                "discovery_complete": c.discovery, "retention": "frozen-catalog-and-spatial-index"]
            value.merge(fields, uniquingKeysWith: { _, n in n })
        } else if phase.hasPrefix("build") {
            let examined = min(s.buildStep, c.files - c.pending)
            let unavailable = max(0, examined - captured), available = examined - unavailable
            let bytes = available == 0 ? 0 : 4096 + max(0, available - 1) * 100
            let running = phase == "build-begin" || examined < c.files - c.pending
            let fields: [String: Any] = ["command": phase == "build-begin" ? "index-begin" : "index-step",
                "index_build_in_progress": running, "capture_complete": !running && c.pending == 0 && c.unavailable == 0 && c.discovery,
                "captured_files": String(available), "unavailable_files": String(unavailable), "examined_files": String(examined),
                "pending_files": String(c.files - examined), "indexed_files": String(c.fallback ? 0 : available),
                "uncovered_files": String(c.fallback ? available : 0), "indexed_source_bytes": String(bytes),
                "initial_source_bytes_read": String(bytes), "build_steps": String(s.buildStep),
                "stop_reason": running ? NSNull() : (c.pending > 0 ? "source-byte-limit" : "all-files-examined")]
            value.merge(fields, uniquingKeysWith: { _, n in n })
            if !running {
                value["capture_manifest"] = "2"; value["index_kind"] = "retained-ephemeral-trigrams"
                value["source_observation"] = "per-file-captures-not-atomic-workspace"; value["discovery_complete"] = c.discovery
            }
        } else if phase == "source" {
            let path: [String: Any] = ["encoding": "unix-bytes", "hex": c.pathHex, "display": "a.rs"]
            let fields: [String: Any] = ["command": "index-open-reader", "capture_manifest": "2", "selection_namespace": "index-source",
                "file_id": "1", "source_revision": "1", "capture_byte_length": "4096", "capture_sha256": c.digest,
                "path": path, "reader_owner": String(reader), "source_reopened": false, "source_observation": "retained-index-capture",
                "reader": ["schema": "fcb.reader-session/1", "status": "ok", "command": "info", "owner": String(reader),
                    "file_id": "1", "source_revision": "1", "captured_bytes": "4096", "encoding": c.encoding, "path": path,
                    "capture_origin": "host-supplied", "initial_source_bytes_read": "0", "initial_read_calls": "0",
                    "additional_source_bytes_read": "0", "native_presented": false]]
            value.merge(fields, uniquingKeysWith: { _, n in n })
        } else {
            let examined = min(s.queryStep, captured), scanned = min(1, examined), skipped = max(0, examined - scanned)
            let running = phase == "query-begin" || examined < captured
            let count = examined > 0 ? c.hitCount : 0
            let first = phase == "page" ? start : 0, end = min(count, first + (phase == "page" ? 128 : 64))
            var rows: [[String: Any]] = []
            if first < end {
                for n in first..<end {
                    let range: [String: String] = ["start": String(n * 2), "end": String(n * 2 + 1)]
                    let path: [String: String] = ["encoding": "unix-bytes", "hex": c.pathHex, "display": "a.rs"]
                    let row: [String: Any] = ["hit_id": String(n + 1), "file_id": "1", "source_revision": "1",
                        "capture_sha256": c.digest, "capture_byte_length": "4096", "original_range": range, "path": path]
                    rows.append(row)
                }
            }
            let fields: [String: Any] = ["command": phase == "query-begin" ? "begin-indexed" : (phase == "page" ? "page" : "step"),
                "capture_manifest": "2", "query_generation": String(generation), "needle": s.needle,
                "mode": "exact-decoded-literal", "search_strategy": "retained-ephemeral-index",
                "catalogued_files": String(c.files), "discovery_complete": c.discovery,
                "examined_files": String(examined + c.unavailable), "scanned_files": String(scanned), "verified_files": String(scanned),
                "skipped_by_index": String(skipped), "fallback_files": String(c.fallback ? scanned : 0),
                "unavailable_files": String(c.unavailable), "pending_files": String(c.files - examined - c.unavailable),
                "retained_hits": String(count), "displayed_hits": String(count), "matches_seen": String(count), "step_count": String(s.queryStep),
                "verification_source_bytes": String(scanned == 0 ? 0 : min(4096, totalBytes)), "source_bytes_read": "0", "read_calls": "0",
                "search_in_progress": running, "search_complete": !running && c.pending == 0 && c.unavailable == 0 && c.discovery,
                "truncated": false, "stop_reason": running ? NSNull() : (c.pending > 0 ? "source-byte-limit" : "all-files-examined"),
                "hits": rows, "next_offset": end < count ? String(end) : NSNull()]
            value.merge(fields, uniquingKeysWith: { _, n in n })
        }
        value = mutation(phase, value)
        return String(decoding: try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self)
    }
}

final class IndexReports: @unchecked Sendable {
    private let lock = NSLock()
    private var reports: [AtlasSearchReport] = []
    func offer(_ value: AtlasSearchReport) { lock.lock(); reports.append(value); lock.unlock() }
    func clear() { lock.lock(); reports = []; lock.unlock() }
    var values: [AtlasSearchReport] { lock.lock(); defer { lock.unlock() }; return reports }
}
