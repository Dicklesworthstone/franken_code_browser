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
        var fileID: UInt64 = 1
        var digest = String(repeating: "a", count: 64)
        var encoding = "utf8"
        var insensitiveText = "X"
        var insensitiveRangeBytes = 1
        var expressionPrimary = "x"
        var expressionExcluded = 0
        var expressionWorkBytes: UInt64 = 8192
        // Explicit scripted scope receipts; this fixture does no file matching.
        var scopedEligible: Int? = nil
        var scopedCaptured: Int? = nil
        var scopedUnavailable: Int? = nil
        var scopedHitCount: Int? = nil
        var captureEligible: Int? = nil
        var captureCaptured: Int? = nil
        var captureUnavailable: Int? = nil
        var captureVisibleCatalogued: Int? = nil
        var captureVisibleMissing = 0
    }
    private struct Session {
        let config: Configuration
        var buildStep = 0
        var queryStep = 0
        var generation: UInt64 = 1
        var needle = ""
        var scope: AtlasSearchFileScope = .all
        var pathScope: AtlasSearchPathScope = .workspace
        var captureFileScope: AtlasSearchFileScope = .all
        var capturePathScope: AtlasSearchPathScope = .workspace
        var caseSensitive = true
        var expression: String?
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
            precondition(index == 1 && revision == 1)
            return try self.response("source", h, reader: reader, sourceFile: file)
        }, close: { h in
            self.lock.lock(); defer { self.lock.unlock() }
            precondition(self.sessions.removeValue(forKey: h) != nil)
            self.events.append("close:\(h)"); return true
        }, retirementFailed: { self.record("retirement-failed") }, scopedQueryBegin: { h, g, index, needle, scope in
            precondition(index == 1 && !scope.isAll)
            return try self.response("query-begin", h, generation: g, needle: needle, scope: scope)
        }, pathScopedQueryBegin: { h, g, index, needle, scope, pathScope in
            precondition(index == 1 && !pathScope.isWorkspace)
            return try self.response("query-begin", h, generation: g, needle: needle, scope: scope, pathScope: pathScope)
        }, matchingQueryBegin: { h, g, index, needle, scope, pathScope, caseSensitive in
            precondition(index == 1)
            return try self.response("query-begin", h, generation: g, needle: needle, scope: scope,
                pathScope: pathScope, caseSensitive: caseSensitive)
        }, scopedIndexBegin: { h, g, scope, pathScope in
            precondition(g == 1)
            return try self.response("build-begin", h, scope: scope, pathScope: pathScope)
        }, expressionQueryBegin: { h, g, index, expression, scope, pathScope, caseSensitive in
            precondition(index == 1)
            return try self.response("query-begin", h, generation: g, scope: scope,
                pathScope: pathScope, caseSensitive: caseSensitive, expression: expression)
        })
    }

    private func response(_ phase: String, _ h: UInt64, generation: UInt64 = 1,
                          needle: String = "", start: Int = 0, reader: UInt64 = 100, sourceFile: UInt64 = 1,
                          scope: AtlasSearchFileScope = .all,
                          pathScope: AtlasSearchPathScope = .workspace,
                          caseSensitive: Bool = true,
                          expression: String? = nil,
                          cancellation: AtlasSearchCancellation? = nil) throws -> String? {
        lock.lock(); let block = blocked; lock.unlock()
        block?(phase)
        if cancellation?.isCanceled == true { throw AtlasSearchError.canceled }
        lock.lock(); defer { lock.unlock() }
        guard var s = sessions[h] else { preconditionFailure("Use after close") }
        events.append("\(phase):\(h):\(generation)")
        if phase == "build-begin" {
            s.captureFileScope = scope; s.capturePathScope = pathScope
            events.append("capture-scope:\(scope.wireKind):\(scope.extensions ?? ""):\(scope.filenames ?? "")")
            events.append("capture-path:\(pathScope.kind.rawValue):\(pathScope.pathHex):\(pathScope.visibleIdentity ?? "")")
        }
        if phase == "build-step" { s.buildStep += 1 }
        if phase == "query-begin" {
            precondition(generation > s.generation)
            s.generation = generation; s.needle = needle; s.queryStep = 0; s.scope = scope
            s.pathScope = pathScope
            s.caseSensitive = caseSensitive
            s.expression = expression
            if let expression {
                s.needle = s.config.expressionPrimary
                events.append("query-expression:\(expression)")
            }
            events.append("query-scope:\(scope.wireKind):\(scope.extensions ?? ""):\(scope.filenames ?? "")")
            events.append("query-path:\(pathScope.kind.rawValue):\(pathScope.pathHex)")
            events.append("query-case:\(caseSensitive)")
        }
        if phase == "query-step" || phase == "page" { precondition(s.generation == generation) }
        if phase == "query-step" { s.queryStep += 1 }
        sessions[h] = s
        let c = s.config
        let captureScoped = !s.captureFileScope.isAll || !s.capturePathScope.isWorkspace
        let captureEligible = c.captureEligible ?? (captureScoped ? c.scopedEligible ?? c.files : c.files)
        let captureUnavailable = c.captureUnavailable ?? (captureScoped ? c.scopedUnavailable ?? c.unavailable : c.unavailable)
        let captured = c.captureCaptured ?? (captureScoped ? c.scopedCaptured ?? (captureEligible - c.pending - captureUnavailable)
            : captureEligible - c.pending - captureUnavailable)
        let capturePending = captureEligible - captured - captureUnavailable
        precondition(captureEligible <= c.files && captured >= 0 && captureUnavailable >= 0 && capturePending >= 0)
        let totalBytes = captured == 0 ? 0 : 4096 + max(0, captured - 1) * 100
        var value: [String: Any] = ["schema": "fcb.atlas-search/1", "status": "ok", "owner": String(h),
            "source_manifest": "1", "layout_revision": "3", "scope": "all", "workspace_files": String(c.files),
            "catalogued_files": String(c.files), "index_generation": "1"]
        if phase != "open" {
            value["capture_search_scope"] = s.captureFileScope.wireKind
            value["capture_scope_extensions"] = s.captureFileScope.extensions ?? ""
            value["capture_scope_filenames"] = s.captureFileScope.filenames ?? ""
            value["capture_scope_path_kind"] = s.capturePathScope.kind.rawValue
            value["capture_scope_path"] = s.capturePathScope.isVisible ? "" : s.capturePathScope.path
            value["capture_scope_path_hex"] = s.capturePathScope.pathHex
            value["capture_case_sensitive"] = true
            value["capture_eligible_files"] = String(captureEligible)
            value["capture_excluded_files"] = String(c.files - captureEligible)
            if s.capturePathScope.isVisible {
                value["capture_scope_region_sha256"] = s.capturePathScope.visibleIdentity
                value["capture_scope_region_files"] = String(s.capturePathScope.visibleCount ?? 0)
                value["capture_scope_region_path_bytes"] = String(Self.visiblePathBytes(s.capturePathScope))
                value["capture_scope_region_catalogued_files"] = String(c.captureVisibleCatalogued ?? captureEligible)
                value["capture_scope_region_missing_files"] = String(c.captureVisibleMissing)
            }
        }
        if phase == "open" {
            let fields: [String: Any] = ["schema": "fcb.atlas-session/1", "command": "info", "catalogued_files": String(c.files),
                "discovery_complete": c.discovery, "retention": "frozen-catalog-and-spatial-index"]
            value.merge(fields, uniquingKeysWith: { _, n in n })
        } else if phase.hasPrefix("build") {
            let examined = min(s.buildStep, captureEligible - capturePending)
            let unavailable = max(0, examined - captured), available = examined - unavailable
            let bytes = available == 0 ? 0 : 4096 + max(0, available - 1) * 100
            let running = phase == "build-begin" || examined < captureEligible - capturePending
            let fields: [String: Any] = ["command": phase == "build-begin" ? "index-begin" : "index-step",
                "index_build_in_progress": running, "capture_complete": !running && capturePending == 0
                    && captureUnavailable == 0 && c.captureVisibleMissing == 0 && c.discovery,
                "captured_files": String(available), "unavailable_files": String(unavailable), "examined_files": String(examined),
                "pending_files": String(captureEligible - examined), "indexed_files": String(c.fallback ? 0 : available),
                "uncovered_files": String(c.fallback ? available : 0), "indexed_source_bytes": String(bytes),
                "initial_source_bytes_read": String(bytes), "build_steps": String(s.buildStep),
                "stop_reason": running ? NSNull() : (capturePending > 0 ? "source-byte-limit" : "all-files-examined")]
            value.merge(fields, uniquingKeysWith: { _, n in n })
            if !running {
                value["capture_manifest"] = "2"; value["index_kind"] = "retained-ephemeral-trigrams"
                value["source_observation"] = "per-file-captures-not-atomic-workspace"; value["discovery_complete"] = c.discovery
            }
        } else if phase == "source" {
            precondition(sourceFile == c.fileID)
            let path: [String: Any] = ["encoding": "unix-bytes", "hex": c.pathHex, "display": "a.rs"]
            let fields: [String: Any] = ["command": "index-open-reader", "capture_manifest": "2", "selection_namespace": "index-source",
                "file_id": String(c.fileID), "source_revision": "1", "capture_byte_length": "4096", "capture_sha256": c.digest,
                "path": path, "reader_owner": String(reader), "source_reopened": false, "source_observation": "retained-index-capture",
                "reader": ["schema": "fcb.reader-session/1", "status": "ok", "command": "info", "owner": String(reader),
                    "file_id": "1", "source_revision": "1", "captured_bytes": "4096", "encoding": c.encoding, "path": path,
                    "capture_origin": "host-supplied", "initial_source_bytes_read": "0", "initial_read_calls": "0",
                    "additional_source_bytes_read": "0", "native_presented": false]]
            value.merge(fields, uniquingKeysWith: { _, n in n })
        } else {
            let scoped = !s.scope.isAll || !s.pathScope.isWorkspace
            let eligible = scoped ? c.scopedEligible ?? c.files : c.files
            let expressionExcluded = s.expression == nil ? 0 : c.expressionExcluded
            let available = max(0, (scoped ? c.scopedCaptured ?? captured : captured) - expressionExcluded)
            let unavailable = min(scoped ? c.scopedUnavailable ?? captureUnavailable : captureUnavailable,
                eligible - expressionExcluded - available)
            let pending = eligible - expressionExcluded - available - unavailable
            precondition(eligible <= c.files && available <= captured && unavailable <= captureUnavailable && pending >= 0)
            let examined = min(s.queryStep, available)
            let scanned = s.caseSensitive ? min(1, examined) : examined, skipped = max(0, examined - scanned)
            let running = phase == "query-begin" || examined < available
            let count = examined > 0 ? (scoped ? c.scopedHitCount ?? c.hitCount : c.hitCount) : 0
            let first = phase == "page" ? start : 0, end = min(count, first + (phase == "page" ? 128 : 64))
            var rows: [[String: Any]] = []
            if first < end {
                for n in first..<end {
                    let width = s.caseSensitive ? (s.expression == nil ? 1 : s.needle.utf8.count) : c.insensitiveRangeBytes
                    let range: [String: String] = ["start": String(n * 2), "end": String(n * 2 + width)]
                    let path: [String: String] = ["encoding": "unix-bytes", "hex": c.pathHex, "display": "a.rs"]
                    var row: [String: Any] = ["hit_id": String(n + 1), "file_id": String(c.fileID), "source_revision": "1",
                        "capture_sha256": c.digest, "capture_byte_length": "4096", "original_range": range, "path": path]
                    if !s.caseSensitive { row["matched_text"] = c.insensitiveText }
                    else if s.expression != nil { row["matched_text"] = s.needle }
                    rows.append(row)
                }
            }
            let fields: [String: Any] = ["command": phase == "query-begin" ? (s.expression == nil ? "begin-indexed" : "begin-indexed-expression") : (phase == "page" ? "page" : "step"),
                "capture_manifest": "2", "query_generation": String(generation), "needle": s.needle,
                "mode": s.caseSensitive ? "exact-decoded-literal" : "unicode-lowercase-literal",
                "case_sensitive": s.caseSensitive, "search_strategy": "retained-ephemeral-index",
                "catalogued_files": String(c.files), "discovery_complete": c.discovery,
                "examined_files": String(expressionExcluded + examined + unavailable), "scanned_files": String(scanned), "verified_files": String(scanned),
                "skipped_by_index": String(skipped), "fallback_files": String(c.fallback || !s.caseSensitive ? scanned : 0),
                "unavailable_files": String(unavailable), "pending_files": String(eligible - expressionExcluded - examined - unavailable),
                "retained_hits": String(count), "displayed_hits": String(count), "matches_seen": String(count), "step_count": String(s.queryStep),
                "verification_source_bytes": String(scanned == 0 ? 0 : min(4096, totalBytes)), "source_bytes_read": "0", "read_calls": "0",
                "search_in_progress": running, "search_complete": !running && pending == 0 && unavailable == 0
                    && (!s.pathScope.isVisible || c.captureVisibleMissing == 0) && c.discovery,
                "truncated": false, "stop_reason": running ? NSNull() : (pending > 0
                    ? (capturePending > 0 ? "source-byte-limit" : "capture-scope") : "all-files-examined"),
                "hits": rows, "next_offset": end < count ? String(end) : NSNull()]
            value.merge(fields, uniquingKeysWith: { _, n in n })
            if let expression = s.expression {
                let scannedInput = UInt64(scanned) * c.expressionWorkBytes
                value["mode"] = "decoded-text-expression"; value["expression"] = expression
                value["primary_needle"] = s.needle; value["query_syntax"] = "fcb-bounded-expression/1"
                value["predicate_scope"] = "same-complete-capture"; value["path_filter_domain"] = "utf8-or-escaped-search-key"
                value["max_scan_bytes"] = String(AtlasSearchExpression.scanLimit)
                value["scanned_input_bytes"] = String(scannedInput); value["verification_source_bytes"] = String(scannedInput)
                value["expression_excluded_files"] = String(expressionExcluded)
                value["expression_scope_files"] = String(eligible - expressionExcluded)
                value["eligible_captured_files"] = String(available)
                value["scoped_capture_unavailable_files"] = String(unavailable)
            }
            if scoped {
                value["search_scope"] = s.scope.wireKind
                value["scope_extensions"] = s.scope.extensions ?? ""
                value["scope_filenames"] = s.scope.filenames ?? ""
                value["eligible_files"] = String(eligible)
                value["excluded_files"] = String(c.files - eligible)
                value["eligible_captured_files"] = String(available)
                value["scoped_capture_unavailable_files"] = String(unavailable)
                value["scope_path_kind"] = s.pathScope.kind.rawValue
                value["scope_path"] = s.pathScope.isVisible ? "" : s.pathScope.path
                value["scope_path_hex"] = s.pathScope.pathHex
                if s.pathScope.isVisible {
                    value["scope_region_sha256"] = s.pathScope.visibleIdentity
                    value["scope_region_files"] = String(s.pathScope.visibleCount ?? 0)
                    value["scope_region_path_bytes"] = String(Self.visiblePathBytes(s.pathScope))
                    value["scope_region_catalogued_files"] = String(c.captureVisibleCatalogued ?? eligible)
                    value["scope_region_missing_files"] = String(c.captureVisibleMissing)
                }
            }
        }
        value = mutation(phase, value)
        return String(decoding: try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self)
    }

    private static func visiblePathBytes(_ scope: AtlasSearchPathScope) -> Int {
        scope.path.split(separator: ",").reduce(0) { $0 + $1.utf8.count / 2 }
    }
}

final class IndexReports: @unchecked Sendable {
    private let lock = NSLock()
    private var reports: [AtlasSearchReport] = []
    func offer(_ value: AtlasSearchReport) { lock.lock(); reports.append(value); lock.unlock() }
    func clear() { lock.lock(); reports = []; lock.unlock() }
    var values: [AtlasSearchReport] { lock.lock(); defer { lock.unlock() }; return reports }
}
