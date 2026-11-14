import Foundation

/// Injected receipts exercise production admission and scheduling. They do not
/// substitute for engine streaming/encoding/filesystem tests or native shaping.
private enum StreamFixture {
    static let root = "/canonical-project"
    static let start: UInt64 = 2 * 1024 * 1024
    static let length: UInt64 = 4 * 1024 * 1024
    static func json(_ value: [String: Any]) throws -> String { try ObservedReaderFixture.encode(value) }
    static func path(_ bytes: [UInt8]) -> [String: String] {
        ["encoding": "unix-bytes", "hex": ObservedReaderFixture.hex(bytes), "display": "escaped native path"]
    }
    static func catalog(_ owner: UInt64) -> [String: Any] {
        ["schema": "fcb.atlas-session/1", "status": "ok", "command": "info", "owner": String(owner),
         "source_manifest": "1", "layout_revision": "1", "root_path": path(Array(root.utf8)),
         "catalogued_files": "1", "workspace_files": "1", "discovery_complete": true,
         "scope": "all", "retention": "frozen-catalog-and-spatial-index"]
    }
    static func common(_ owner: UInt64, _ generation: UInt64, _ command: String) -> [String: Any] {
        ["schema": "fcb.atlas-stream-search/1", "status": "ok", "command": command,
         "owner": String(owner), "source_manifest": "1", "layout_revision": "1",
         "query_generation": String(generation), "last_attempt": String(generation),
         "root_path": path(Array(root.utf8)), "scope": "catalogued-whole-files",
         "strategy": "streaming-whole-file", "mode": "exact-decoded-literal",
         "capture_kind": "literal-witnesses-only", "source_observation": "per-file-streams-not-atomic-workspace",
         "catalogued_files": "1", "discovery_policy": "product-defaults/no-rule-files-v1", "excluded_entries": "2"]
    }
    static func scope(_ request: AtlasWorkspaceStreamRequest, catalogued: UInt64 = 1,
        eligible: UInt64 = 1, missing: UInt64 = 0) -> [String: Any] {
        var value: [String: Any] = [
            "search_scope": request.fileScope.wireKind,
            "scope_extensions": request.fileScope.extensions ?? "",
            "scope_filenames": request.fileScope.filenames ?? "",
            "scope_path_kind": request.pathScope.kind.rawValue,
            "scope_path": request.pathScope.isVisible ? "" : request.pathScope.path,
            "scope_path_hex": request.pathScope.pathHex, "case_sensitive": request.caseSensitive,
            "eligible_files": String(eligible), "excluded_files": String(catalogued - eligible)]
        if request.pathScope.isVisible {
            value["scope_region_sha256"] = request.pathScope.visibleIdentity!
            value["scope_region_files"] = String(request.pathScope.visibleCount!)
            value["scope_region_path_bytes"] = String(request.pathScope.path.split(separator: ",").reduce(0) { $0 + $1.utf8.count / 2 })
            value["scope_region_catalogued_files"] = String(UInt64(request.pathScope.visibleCount!) - missing)
            value["scope_region_missing_files"] = String(missing)
        }
        return value
    }
    static func report(_ owner: UInt64, _ generation: UInt64, _ request: AtlasWorkspaceStreamRequest,
                       count: Int = 40, encoding: AtlasObservedReaderEncoding = .utf8,
                       spellings: [String]? = nil, primaryNeedle: String? = nil) -> [String: Any] {
        var value = common(owner, generation, "search")
        // An explicit fixture oracle supplies parsed evidence; Swift does not
        // duplicate the production expression parser, even in this test seam.
        precondition(request.mode != .expression || primaryNeedle != nil)
        let primary = primaryNeedle ?? request.needle
        let literals = spellings ?? Array(repeating: primary, count: count)
        precondition(literals.count == count)
        let bytes = AtlasObservedReaderSearchRequest.originalBytes(primary, encoding: encoding)
        value["mode"] = request.mode.wireMode(caseSensitive: request.caseSensitive)
        if request.mode == .expression {
            value["expression"] = request.needle; value["primary_needle"] = primary
            value["query_syntax"] = "fcb-bounded-expression/1"
            value["predicate_scope"] = "same-observed-stream"
            value["path_filter_domain"] = "root-relative-utf8-or-escaped-search-key"
            value["expression_scope_files"] = "1"; value["expression_excluded_files"] = "0"
            value["terminal"] = true; value["selection_available"] = true
            value["page_start_after"] = "0"; value["skipped_matches"] = "0"
            value["page_has_more_matches"] = false
        }
        if request.mode == .occurrences {
            value["evidence_level"] = "whole-token-text-candidate"
            value["includes_comments_and_literals"] = true
            value["compiler_resolved"] = false; value["semantic_complete"] = false
            value["count_all_matches"] = request.countAllMatches
            value["match_count_complete"] = true
            value["retained_matching_files"] = count == 0 ? "0" : "1"
            value["terminal"] = true; value["selection_available"] = true
        }
        value["needle"] = primary; value["discovery_complete"] = true
        value["search_complete"] = true; value["truncated"] = false; value["stop_reason"] = NSNull()
        value["files_examined"] = "1"; value["files_unexamined"] = "0"
        value["incomplete_files"] = "0"; value["unavailable_files"] = "0"
        value["unsupported_files"] = "0"; value["failed_files"] = "0"
        value["matching_files"] = count == 0 ? "0" : "1"
        value["retained_hits"] = String(count); value["matches_seen"] = String(count)
        value["payload_bytes_read"] = String(length); value["scanned_input_bytes"] = String(length)
        value["read_calls"] = "256"; value["interrupted_read_calls"] = "0"
        value["peak_input_buffer_bytes"] = "16384"
        value["retained_witness_bytes"] = String(request.caseSensitive ? (count == 0 ? 0 : bytes.count)
            : literals.reduce(0) { $0 + AtlasObservedReaderSearchRequest.originalBytes($1, encoding: encoding).count })
        value["limits"] = ["max_matches": String(request.maximumMatches),
            "max_scan_bytes": String(request.maximumScanBytes), "max_read_calls": String(request.maximumReadCalls)]
        value["hits"] = (0..<count).map { index -> [String: Any] in
            let original = AtlasObservedReaderSearchRequest.originalBytes(literals[index], encoding: encoding)
            return ["index": String(index), "occurrence_id": String(index + 1), "file_id": "1", "node_ordinal": "5",
             "source_revision": String(generation), "path": path([0x73, 0x72, 0x63, 0x2f, 0xff, 0x2e, 0x74, 0x78, 0x74]),
             "original_range": ObservedReaderFixture.range(start + UInt64(index * 8), start + UInt64(index * 8 + original.count)),
             "encoding": encoding.rawValue, "observed_length": String(length), "final_observed_length": String(length),
             "consistency": "unchanged-metadata-not-atomic", "file_state": "complete-observed-input", "file_complete": true,
             "witness_sha256": String(repeating: request.caseSensitive || index % 2 == 0 ? "b" : "c", count: 64),
             "fixture_spelling": literals[index]]
        }
        // Keep default receipts legacy to exercise backward compatibility.
        if request.mode != .text || !request.fileScope.isAll || !request.pathScope.isWorkspace || !request.caseSensitive {
            value.merge(scope(request), uniquingKeysWith: { _, next in next })
        }
        if request.countAllMatches {
            value = counted(value, matches: UInt64(count), matchingFiles: count == 0 ? 0 : 1)
        }
        return value
    }
    /// One live-file observation containing total matching occurrences. Window
    /// ordinals are regenerated on each call and never appended to old evidence.
    static func matchWindow(_ owner: UInt64, _ generation: UInt64, _ request: AtlasWorkspaceStreamRequest,
        total: UInt64 = 600) -> [String: Any] {
        let skipped = min(request.pageStartAfter, total)
        let retained = min(request.maximumMatches, total - skipped)
        let seen = request.countAllMatches ? total : min(total, skipped + retained + 1)
        let more = seen - skipped > retained, complete = request.countAllMatches || !more
        var value = report(owner, generation, request, count: Int(retained))
        value["page_start_after"] = String(request.pageStartAfter)
        value["skipped_matches"] = String(skipped); value["page_has_more_matches"] = more
        value["matches_seen"] = String(seen); value["truncated"] = seen > retained
        value["count_all_matches"] = request.countAllMatches; value["match_count_complete"] = complete
        value["matching_files"] = seen == 0 ? "0" : "1"
        value["retained_matching_files"] = retained == 0 ? "0" : "1"
        value["search_complete"] = complete
        value["stop_reason"] = complete ? NSNull() : "match-limit" as Any
        value["incomplete_files"] = complete ? "0" : "1"
        value["terminal"] = true; value["selection_available"] = true
        var rows = value["hits"] as! [[String: Any]]
        for index in rows.indices {
            rows[index]["occurrence_id"] = String(request.pageStartAfter + UInt64(index) + 1)
            rows[index]["file_state"] = complete ? "complete-observed-input" : "match-limit"
            rows[index]["file_complete"] = complete
        }
        value["hits"] = rows
        return value
    }
    static func counted(_ report: [String: Any], matches: UInt64, matchingFiles: UInt64) -> [String: Any] {
        var value = report
        value["count_all_matches"] = true
        value["match_count_complete"] = report["search_complete"]
        value["retained_matching_files"] = report["retained_matching_files"] ?? report["matching_files"]
        value["matching_files"] = String(matchingFiles); value["matches_seen"] = String(matches)
        value["truncated"] = matches > UInt64(report["retained_hits"] as! String)!
        if let start = report["page_start_after"] as? String {
            let skipped = min(UInt64(start)!, matches)
            value["skipped_matches"] = String(skipped)
            value["page_has_more_matches"] = matches - skipped > UInt64(report["retained_hits"] as! String)!
        }
        return value
    }
    static func page(_ report: [String: Any], offset: Int = 0) -> [String: Any] {
        var value = report
        let hits = report["hits"] as! [[String: Any]], end = min(hits.count, offset + 32)
        value["command"] = offset == 0 ? "search" : "page"
        value["page_offset"] = String(offset); value["page_count"] = String(end - offset)
        value["hits"] = Array(hits[offset..<end])
        value["next_offset"] = end < hits.count ? String(end) as Any : NSNull()
        return value
    }
    static func witness(_ report: [String: Any], index: Int) -> [String: Any] {
        var value = report; value["command"] = "hit"
        var row = (report["hits"] as! [[String: Any]])[index]
        let text = row["fixture_spelling"] as? String ?? report["needle"] as! String
        let encoding = AtlasObservedReaderEncoding(rawValue: row["encoding"] as! String)!
        row["text"] = text
        row["original_hex"] = ObservedReaderFixture.hex(AtlasObservedReaderSearchRequest.originalBytes(text, encoding: encoding))
        value["hit"] = row; value.removeValue(forKey: "hits")
        value["additional_source_bytes_read"] = "0"; value["additional_read_calls"] = "0"
        return value
    }
}

private enum StreamProgressFixture {
    static func catalog(_ owner: UInt64) -> [String: Any] {
        var value = StreamFixture.catalog(owner)
        value["catalogued_files"] = "2"; value["workspace_files"] = "2"
        return value
    }
    /// The first file has finalized matches; read accounting also includes one
    /// chunk of a second unfinished file, whose matches are deliberately absent.
    static func report(_ owner: UInt64, _ generation: UInt64, _ request: AtlasWorkspaceStreamRequest,
        stopped: Bool, count: Int = 40, examined: UInt64 = 1,
        payload: UInt64 = StreamFixture.length + 16_384, primaryNeedle: String? = nil) -> [String: Any] {
        var value = StreamFixture.report(owner, generation, request, count: count, primaryNeedle: primaryNeedle)
        value["catalogued_files"] = "2"; value["terminal"] = true; value["selection_available"] = true
        value["files_examined"] = stopped ? String(examined) : "2"
        value["files_unexamined"] = stopped ? String(2 - examined) : "0"
        value["search_complete"] = !stopped; value["stop_reason"] = stopped ? "user-stopped" as Any : NSNull()
        if request.countAllMatches || request.mode == .occurrences { value["match_count_complete"] = !stopped }
        let bytes = stopped ? payload : 2 * StreamFixture.length
        value["payload_bytes_read"] = String(bytes); value["scanned_input_bytes"] = String(bytes)
        value["read_calls"] = String((bytes + 16_383) / 16_384)
        value["peak_input_buffer_bytes"] = String(min(bytes, 16_384))
        if request.mode != .text || !request.fileScope.isAll || !request.pathScope.isWorkspace || !request.caseSensitive {
            value.merge(StreamFixture.scope(request, catalogued: 2, eligible: 2),
                uniquingKeysWith: { _, next in next })
        }
        if request.mode == .expression { value["expression_scope_files"] = "2" }
        return value
    }
    static func progress(_ owner: UInt64, _ generation: UInt64, _ request: AtlasWorkspaceStreamRequest,
        sequence: UInt64, count: Int = 40, examined: UInt64 = 1,
        payload: UInt64 = StreamFixture.length + 16_384, primaryNeedle: String? = nil) -> [String: Any] {
        var value = report(owner, generation, request, stopped: true, count: count, examined: examined,
            payload: payload, primaryNeedle: primaryNeedle)
        value["stop_reason"] = NSNull(); value["terminal"] = false; value["selection_available"] = false
        value["progress_sequence"] = String(sequence)
        value = StreamFixture.page(value)
        value["command"] = "progress"
        return value
    }
}

private final class StreamProbe: @unchecked Sendable {
    private let expressionPrimary: String?
    init(expressionPrimary: String? = nil) { self.expressionPrimary = expressionPrimary }
    private let lock = NSLock()
    private var nextOwner: UInt64 = 100
    private var generations: [UInt64: UInt64] = [:]
    private var reports: [UInt64: [String: Any]] = [:]
    private var searchGate: ObservedReaderGate?, hitGate: ObservedReaderGate?, pageGate: ObservedReaderGate?
    private var badSearch = false, badPage = false, badOpen = false, staleSearch = false
    private var matchWindows = false
    private var searches = 0, hits = 0, pages = 0, active = 0, mostActive = 0, created = 0
    private var closed = Set<UInt64>(), released = Set<String>()
    private var closeAllowed = true, retirementFailures = 0
    private var progressGate: ObservedReaderGate?, badProgress = false, burstProgress = false
    private var stoppedRequests = 0, stoppedWithCancellation = false, cancelCalls = 0
    private func locked<T>(_ work: () throws -> T) rethrows -> T {
        lock.lock(); defer { lock.unlock() }; return try work()
    }
    var transport: AtlasWorkspaceStreamTransport {
        .init(create: { self.locked { self.nextOwner += 1; self.created += 1; return self.nextOwner } },
            open: { owner, _, _ in try self.open(owner) },
            search: { owner, request, _ in try self.search(owner, request) },
            page: { owner, generation, start, count in try self.page(owner, generation, start, count) },
            hit: { owner, generation, index in try self.hit(owner, generation, index) },
            clear: { owner in try self.clear(owner) }, cancel: { _ in self.locked { self.cancelCalls += 1 }; return true },
            close: { owner in self.locked { guard self.closeAllowed else { return false }; self.closed.insert(owner); return true } },
            retirementFailed: { self.locked { self.retirementFailures += 1 } })
    }
    var progressiveTransport: AtlasWorkspaceStreamTransport {
        let base = transport
        return .init(create: base.create,
            open: { owner, _, _ in try StreamFixture.json(StreamProgressFixture.catalog(owner)) },
            search: base.search, page: base.page, hit: base.hit, clear: base.clear,
            cancel: base.cancel, close: base.close, retirementFailed: base.retirementFailed,
            progressiveSearch: { owner, request, flag, stop, publish in
                try self.progressiveSearch(owner, request, flag, stop, publish)
            })
    }
    func blockProgress(_ gate: ObservedReaderGate) { locked { progressGate = gate } }
    func poisonProgress() { locked { badProgress = true } }
    func burstNextProgress() { locked { burstProgress = true } }
    var stopCount: Int { locked { stoppedRequests } }
    var stopCanceledWork: Bool { locked { stoppedWithCancellation } }
    var cancelCount: Int { locked { cancelCalls } }
    func enableMatchWindows() { locked { matchWindows = true } }
    func blockSearch(_ gate: ObservedReaderGate) { locked { searchGate = gate } }
    func blockHit(_ gate: ObservedReaderGate) { locked { hitGate = gate } }
    func blockPage(_ gate: ObservedReaderGate) { locked { pageGate = gate } }
    func failSearch() { locked { badSearch = true } }
    func failPage() { locked { badPage = true } }
    func failOpen() { locked { badOpen = true } }
    func repeatGeneration() { locked { staleSearch = true } }
    func allowClose(_ allow: Bool) { locked { closeAllowed = allow } }
    func releaseGrant(_ label: String) { locked { _ = released.insert(label) } }
    func hasReleased(_ label: String) -> Bool { locked { released.contains(label) } }
    var searchCount: Int { locked { searches } }
    var hitCount: Int { locked { hits } }
    var pageCount: Int { locked { pages } }
    var activeCount: Int { locked { active } }
    var maximumActive: Int { locked { mostActive } }
    var createCount: Int { locked { created } }
    var closedCount: Int { locked { closed.count } }
    var failureCount: Int { locked { retirementFailures } }
    private func enter() { locked { active += 1; mostActive = max(mostActive, active) } }
    private func leave() { locked { active -= 1 } }
    private func open(_ owner: UInt64) throws -> String {
        let fails = locked { let result = badOpen; badOpen = false; return result }
        if fails { throw AtlasWorkspaceStreamError.engine("OPEN_FAILED") }
        return try StreamFixture.json(StreamFixture.catalog(owner))
    }
    private func search(_ owner: UInt64, _ request: AtlasWorkspaceStreamRequest) throws -> String {
        enter(); defer { leave() }
        let (gate, fails, stale, generation, windows) = locked {
            let result = (searchGate, badSearch, staleSearch, (generations[owner] ?? 0) + 1, matchWindows)
            searchGate = nil; badSearch = false; staleSearch = false
            searches += 1; generations[owner] = result.3
            return result
        }
        gate?.wait()
        var report = windows
            ? StreamFixture.matchWindow(owner, stale ? generation - 1 : generation, request)
            : StreamFixture.report(owner, stale ? generation - 1 : generation, request,
                count: request.countAllMatches ? 256 : 40,
                primaryNeedle: request.mode == .expression ? expressionPrimary : nil)
        if request.countAllMatches && !windows { report = StreamFixture.counted(report, matches: 2_000, matchingFiles: 1) }
        locked { reports[owner] = report }
        if fails { report["scope"] = "visible-only" } // Backend already replaced before bad delivery.
        return try StreamFixture.json(StreamFixture.page(report))
    }
    private func progressiveSearch(_ owner: UInt64, _ request: AtlasWorkspaceStreamRequest,
        _ flag: AtlasSearchCancellation, _ stop: AtlasWorkspaceStreamStop,
        _ publish: @Sendable (String) -> Void) throws -> String {
        enter(); defer { leave() }
        let (gate, poisoned, burst, stale, generation) = locked {
            let result = (progressGate, badProgress, burstProgress, staleSearch, (generations[owner] ?? 0) + 1)
            progressGate = nil; badProgress = false; burstProgress = false; staleSearch = false
            searches += 1; generations[owner] = result.4
            return result
        }
        publish(try StreamFixture.json(StreamProgressFixture.progress(owner, generation, request,
            sequence: 1, count: 0, examined: 0, payload: 16_384,
            primaryNeedle: request.mode == .expression ? expressionPrimary : nil)))
        for sequence in UInt64(2)...UInt64(burst ? 128 : 2) {
            var progress = StreamProgressFixture.progress(owner, generation, request, sequence: sequence,
                count: request.countAllMatches ? 256 : 40,
                primaryNeedle: request.mode == .expression ? expressionPrimary : nil)
            if request.countAllMatches { progress = StreamFixture.counted(progress, matches: 1_000, matchingFiles: 1) }
            if poisoned { progress["scope"] = "visible-only" }
            publish(try StreamFixture.json(progress))
            if flag.isCanceled { throw AtlasWorkspaceStreamError.canceled }
        }
        gate?.wait()
        locked {
            if stop.isRequested { stoppedRequests += 1; stoppedWithCancellation = stoppedWithCancellation || flag.isCanceled }
        }
        guard !flag.isCanceled else { throw AtlasWorkspaceStreamError.canceled }
        var report = StreamProgressFixture.report(owner, stale ? generation - 1 : generation, request,
            stopped: stop.isRequested, count: request.countAllMatches ? 256 : 40,
            primaryNeedle: request.mode == .expression ? expressionPrimary : nil)
        if request.countAllMatches {
            report = StreamFixture.counted(report, matches: stop.isRequested ? 1_000 : 2_000,
                matchingFiles: stop.isRequested ? 1 : 2)
        }
        locked { reports[owner] = report }
        return try StreamFixture.json(StreamFixture.page(report))
    }
    private func page(_ owner: UInt64, _ generation: UInt64, _ start: UInt64, _ count: UInt64) throws -> String {
        enter(); defer { leave() }
        let (gate, fails, report) = locked {
            let result = (pageGate, badPage, reports[owner]!); pageGate = nil; badPage = false; pages += 1
            return result
        }
        gate?.wait(); precondition(count == 32 && report["query_generation"] as? String == String(generation))
        var value = StreamFixture.page(report, offset: Int(start))
        if fails { value["query_generation"] = String(generation + 1) }
        return try StreamFixture.json(value)
    }
    private func hit(_ owner: UInt64, _ generation: UInt64, _ index: UInt64) throws -> String {
        enter(); defer { leave() }
        let (gate, report) = locked { let result = (hitGate, reports[owner]!); hitGate = nil; hits += 1; return result }
        gate?.wait(); precondition(report["query_generation"] as? String == String(generation))
        return try StreamFixture.json(StreamFixture.witness(report, index: Int(index)))
    }
    private func clear(_ owner: UInt64) throws -> String {
        let generation = locked { generations[owner, default: 0] += 1; reports[owner] = nil; return generations[owner]! }
        var value = StreamFixture.common(owner, generation, "clear")
        value["retained_hits"] = "0"; value["retained_witness_bytes"] = "0"
        return try StreamFixture.json(value)
    }
}
private final class StreamGrant {
    let probe: StreamProbe
    let label: String
    init(_ probe: StreamProbe, _ label: String) { self.probe = probe; self.label = label }
    deinit { probe.releaseGrant(label) }
}

private final class StreamContextProbe: @unchecked Sendable {
    private let lock = NSLock()
    private var inputs: [UInt64: AtlasObservedReaderInput] = [:]
    private var next: UInt64 = 500, offsets: [UInt64] = []
    private var changed = false
    var requestedOffsets: [UInt64] { lock.lock(); defer { lock.unlock() }; return offsets }
    func change() { lock.lock(); changed = true; lock.unlock() }
    var transport: AtlasObservedReaderTransport {
        .init(create: { self.create() }, open: { owner, input, _ in try self.open(owner, input) },
            info: { _ in nil }, window: { owner, request, _ in try self.window(owner, request) },
            retained: { _, _ in nil }, copy: { _, _, _, _ in nil }, cancel: { _ in true }, close: { _ in true }, retirementFailed: {})
    }
    private func create() -> UInt64 { lock.lock(); defer { lock.unlock() }; next += 1; return next }
    private func open(_ owner: UInt64, _ input: AtlasObservedReaderInput) throws -> String {
        lock.lock(); inputs[owner] = input; lock.unlock()
        return try StreamFixture.json(ObservedReaderFixture.info(owner, input))
    }
    private func window(_ owner: UInt64, _ request: AtlasObservedReaderRequest) throws -> String {
        lock.lock(); let input = inputs[owner]!, changed = self.changed; offsets.append(request.offset); lock.unlock()
        let prefix = Int(StreamFixture.start - request.offset)
        let literal = changed ? "new" : "old"
        let text = String(repeating: "x", count: prefix) + literal + String(repeating: "y", count: Int(request.bytes) - prefix - 3)
        return try StreamFixture.json(ObservedReaderFixture.page(owner, input, 1, request,
            length: StreamFixture.length, text: text))
    }
}

@main private enum AtlasWorkspaceStreamTests {
    static func main() async throws {
        try wireTests()
        try scopeWireTests()
        try countedWireTests()
        try matchWindowWireTests()
        try lowercaseWireTests()
        try occurrenceWireTests()
        try expressionWireTests()
        try progressWireTests()
        try await countedModelTests()
        try await lowercaseModelTests()
        try await occurrenceModelTests()
        try await expressionModelTests()
        try await progressModelTests()
        try await modelTests()
        try await matchWindowModelTests()
        try await scopedModelTests()
        try await lifetimeTests()
        print("Workspace stream wire/model/context fixtures passed; no native presentation qualification claimed")
    }
    private static func rejects(_ label: String, _ work: () throws -> Void) {
        do { try work(); preconditionFailure("Accepted invalid " + label) } catch { }
    }
    @MainActor private static func wait(_ label: String, _ predicate: () -> Bool) async throws {
        for _ in 0..<1000 {
            if predicate() { return }
            try await Task.sleep(for: .milliseconds(5))
        }
        preconditionFailure("Timed out: " + label)
    }
    private static func matchWindowWireTests() throws {
        let owner: UInt64 = 0xfedcba9876543210
        let catalog = try AtlasWorkspaceStreamCatalog.decode(StreamFixture.json(StreamFixture.catalog(owner)), owner: owner)
        let request = try AtlasWorkspaceStreamRequest(needle: "old", maximumMatches: 2, pageStartAfter: 3)
        func decode(_ value: [String: Any], selected: AtlasWorkspaceStreamRequest? = nil) throws -> AtlasWorkspaceStreamPage {
            try .decode(StreamFixture.json(StreamFixture.page(value)), catalog: catalog, root: catalog.root,
                request: selected ?? request, requireTerminal: true)
        }
        let finalWire = StreamFixture.matchWindow(owner, 9, request, total: 5)
        let final = try decode(finalWire)
        precondition(final.summary.complete && final.summary.truncated && !final.summary.pageHasMoreMatches)
        precondition(final.summary.skippedMatches == 3 && final.summary.nextStartAfter == nil)
        precondition(final.hits.map(\.occurrence) == [4, 5])
        let report = try AtlasWorkspaceStreamReport(summary: final.summary, hits: final.hits)
        let target = report.target(for: final.hits[0])
        _ = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(StreamFixture.witness(finalWire, index: 0)),
            report: report, root: catalog.root, target: target)
        var wrongWitness = StreamFixture.witness(finalWire, index: 0)
        wrongWitness["page_start_after"] = "0"
        rejects("witness from another match window") {
            _ = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(wrongWitness),
                report: report, root: catalog.root, target: target)
        }
        let more = try decode(StreamFixture.matchWindow(owner, 10, request, total: 6))
        precondition(!more.summary.complete && more.summary.pageHasMoreMatches && more.summary.nextStartAfter == 5)
        let counted = try AtlasWorkspaceStreamRequest(needle: "old", maximumMatches: 2,
            countAllMatches: true, pageStartAfter: 3)
        let countedPage = try decode(StreamFixture.matchWindow(owner, 11, counted, total: 99), selected: counted)
        precondition(countedPage.summary.complete && countedPage.summary.matchCountComplete
            && countedPage.summary.pageHasMoreMatches && countedPage.summary.matchesSeen == 99)
        let beyond = try AtlasWorkspaceStreamRequest(needle: "old", maximumMatches: 2,
            pageStartAfter: 9_007_199_254_740_993)
        let empty = try decode(StreamFixture.matchWindow(owner, 12, beyond, total: 5), selected: beyond)
        precondition(empty.hits.isEmpty && empty.summary.complete && empty.summary.skippedMatches == 5
            && !empty.summary.pageHasMoreMatches)
        rejects("overflowing match window") {
            _ = try AtlasWorkspaceStreamRequest(needle: "old", pageStartAfter: UInt64.max)
        }
        for (key, replacement) in [("page_start_after", "03" as Any), ("page_start_after", "2" as Any),
            ("skipped_matches", "2" as Any), ("page_has_more_matches", true as Any)] {
            var invalid = finalWire; invalid[key] = replacement
            rejects("changed match window " + key) { _ = try decode(invalid) }
        }
        for key in ["page_start_after", "skipped_matches", "page_has_more_matches"] {
            var invalid = finalWire; invalid.removeValue(forKey: key)
            rejects("incomplete match-window metadata") { _ = try decode(invalid) }
        }
        var legacy = finalWire
        for key in ["page_start_after", "skipped_matches", "page_has_more_matches"] { legacy.removeValue(forKey: key) }
        rejects("legacy reply cannot authorize a later scan") { _ = try decode(legacy) }

        // A budget stop before the skip is exhausted is partial, never an
        // exhaustive empty result. Earlier matches still contribute to counts.
        let pending = try AtlasWorkspaceStreamRequest(needle: "old",
            maximumScanBytes: StreamFixture.length, maximumMatches: 2, pageStartAfter: 8)
        var limited = StreamFixture.matchWindow(owner, 13, pending, total: 3)
        limited["search_complete"] = false; limited["match_count_complete"] = false
        limited["incomplete_files"] = "1"; limited["stop_reason"] = "byte-limit"
        let partial = try decode(limited, selected: pending)
        precondition(partial.hits.isEmpty && !partial.summary.complete && partial.summary.skippedMatches == 3)
        precondition(!partial.summary.pageHasMoreMatches && partial.summary.nextStartAfter == nil)

        // The lookahead can occur in another file with no retained rows.
        let first = try AtlasWorkspaceStreamRequest(needle: "old", maximumMatches: 2)
        var crossFile = StreamFixture.matchWindow(owner, 14, first, total: 3)
        var twoFiles = StreamFixture.catalog(owner)
        twoFiles["catalogued_files"] = "2"; twoFiles["workspace_files"] = "2"
        let catalog2 = try AtlasWorkspaceStreamCatalog.decode(StreamFixture.json(twoFiles), owner: owner)
        crossFile["catalogued_files"] = "2"; crossFile["files_examined"] = "2"
        crossFile["matching_files"] = "2"; crossFile["retained_matching_files"] = "1"
        let crossPage = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(StreamFixture.page(crossFile)),
            catalog: catalog2, root: catalog2.root, request: first, requireTerminal: true)
        precondition(crossPage.summary.matchingFiles == 2 && crossPage.summary.retainedMatchingFiles == 1
            && crossPage.summary.pageHasMoreMatches)
    }
    private static func wireTests() throws {
        let owner: UInt64 = 0xfedcba9876543210
        let request = try AtlasWorkspaceStreamRequest(needle: "old")
        let catalog = try AtlasWorkspaceStreamCatalog.decode(StreamFixture.json(StreamFixture.catalog(owner)), owner: owner)
        let base = StreamFixture.report(owner, 1, request)
        func page(_ value: [String: Any], previous: AtlasWorkspaceStreamSummary? = nil, offset: UInt64 = 0) throws -> AtlasWorkspaceStreamPage {
            try .decode(StreamFixture.json(value), catalog: catalog, root: catalog.root, request: request,
                previous: previous, offset: offset)
        }
        let first = try page(StreamFixture.page(base)), tail = try page(StreamFixture.page(base, offset: 32), previous: first.summary, offset: 32)
        let report = try AtlasWorkspaceStreamReport(summary: first.summary, hits: first.hits + tail.hits)
        precondition(report.hits.count == 40 && first.next == 32 && tail.next == nil)
        precondition(report.hits[0].path.contains(255) && report.hits[0].range.start > 1024 * 1024)
        precondition(report.summary.complete && report.summary.excludedEntries == 2)
        let target = report.target(for: report.hits[0])
        let witnessJSON = StreamFixture.witness(base, index: 0)
        let witness = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(witnessJSON), report: report,
            root: catalog.root, target: target)
        precondition(witness.text == "old" && witness.originalHex == "6f6c64")
        var explicitAll = base
        explicitAll.merge(StreamFixture.scope(request), uniquingKeysWith: { _, next in next })
        let explicitPage = try page(StreamFixture.page(explicitAll))
        precondition(explicitPage.summary == first.summary && explicitPage.hits == first.hits,
            "The new all-files scope echo preserves legacy result semantics")
        _ = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(StreamFixture.witness(explicitAll, index: 0)),
            report: report, root: catalog.root, target: target)

        for (key, value) in [("owner", "1" as Any), ("source_manifest", "2" as Any), ("layout_revision", "2" as Any),
            ("scope", "visible" as Any), ("needle", "new" as Any), ("retained_hits", "040" as Any),
            ("files_examined", 1 as Any), ("unavailable_files", "1" as Any), ("search_complete", false as Any),
            ("truncated", true as Any), ("matches_seen", "41" as Any), ("retained_witness_bytes", "120" as Any),
            ("capture_sha256", String(repeating: "a", count: 64) as Any)] {
            var bad = StreamFixture.page(base); bad[key] = value
            rejects("report " + key) { _ = try page(bad) }
        }
        var wrongRoot = StreamFixture.page(base); wrongRoot["root_path"] = StreamFixture.path(Array("/other".utf8))
        rejects("root binding") { _ = try page(wrongRoot) }
        var stale = StreamFixture.page(base, offset: 32); stale["query_generation"] = "2"; stale["last_attempt"] = "2"
        rejects("mixed page generations") { _ = try page(stale, previous: first.summary, offset: 32) }
        var missingNull = StreamFixture.page(base); missingNull.removeValue(forKey: "stop_reason")
        rejects("missing nullable stop") { _ = try page(missingNull) }
        var short = StreamFixture.page(base); short["hits"] = Array(first.hits.dropLast().map { _ in [String: Any]() })
        rejects("short page") { _ = try page(short) }
        for badPath in [[UInt8]([46, 46, 47, 120]), [47, 120], [120, 0], [120, 47, 47, 121]] {
            var bad = StreamFixture.page(base), rows = bad["hits"] as! [[String: Any]]
            rows[0]["path"] = StreamFixture.path(badPath); bad["hits"] = rows
            rejects("unsafe raw relative path") { _ = try page(bad) }
        }
        for (key, value) in [("text", "new" as Any), ("original_hex", "6e6577" as Any),
            ("witness_sha256", String(repeating: "c", count: 64) as Any), ("source_revision", "2" as Any)] {
            var bad = witnessJSON, row = bad["hit"] as! [String: Any]; row[key] = value; bad["hit"] = row
            rejects("witness " + key) { _ = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(bad),
                report: report, root: catalog.root, target: target) }
        }
        var readingWitness = witnessJSON; readingWitness["additional_source_bytes_read"] = "1"
        rejects("witness reread") { _ = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(readingWitness),
            report: report, root: catalog.root, target: target) }
        for query in ["", "a\0b", String(repeating: "x", count: 1025)] {
            rejects("invalid needle") { _ = try AtlasWorkspaceStreamRequest(needle: query) }
        }
        let composed = try AtlasWorkspaceStreamRequest(needle: "é"), decomposed = try AtlasWorkspaceStreamRequest(needle: "e\u{301}")
        precondition(composed != decomposed)

        for encoding in AtlasObservedReaderEncoding.allCases {
            let unicode = try AtlasWorkspaceStreamRequest(needle: "é😀\u{feff}")
            let wire = StreamFixture.report(owner, 7, unicode, count: 1, encoding: encoding)
            let decoded = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(StreamFixture.page(wire)),
                catalog: catalog, root: catalog.root, request: unicode)
            let unicodeReport = try AtlasWorkspaceStreamReport(summary: decoded.summary, hits: decoded.hits)
            let hit = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(StreamFixture.witness(wire, index: 0)),
                report: unicodeReport, root: catalog.root, target: unicodeReport.target(for: decoded.hits[0]))
            precondition(hit.text.utf8.elementsEqual(unicode.needle.utf8))
            let input = try AtlasObservedReaderInput(root: catalog.root, relativePath: [120])
            let bytes = AtlasObservedReaderSearchRequest.originalBytes(unicode.needle, encoding: encoding)
            let match = try AtlasObservedReaderExternalMatch(input: input, range: .init(start: 0, end: UInt64(bytes.count)),
                encoding: encoding, text: unicode.needle, originalHex: ObservedReaderFixture.hex(bytes))
            let contextRequest = try match.request()
            let rawPage = ObservedReaderFixture.page(44, input, 1, contextRequest, length: UInt64(bytes.count),
                text: unicode.needle, originalHex: ObservedReaderFixture.hex(bytes))
            let context = try AtlasObservedReaderPage.decode(StreamFixture.json(rawPage), handle: 44, input: input, request: contextRequest)
            precondition(match.verify(context: context) == .verified(selection: NSRange(location: 0, length: unicode.needle.utf16.count)))
            let otherInput = try AtlasObservedReaderInput(root: catalog.root, relativePath: [121])
            let otherMatch = try AtlasObservedReaderExternalMatch(input: otherInput, range: match.range,
                encoding: encoding, text: unicode.needle, originalHex: match.originalHex)
            precondition(otherMatch.verify(context: context) == .unavailable)
        }
        for stop in ["match-limit", "byte-limit", "read-call-limit"] {
            let limits = try AtlasWorkspaceStreamRequest(needle: "old",
                maximumScanBytes: StreamFixture.length + (stop == "read-call-limit" ? 1024 : 0),
                maximumMatches: 2, maximumReadCalls: 256)
            var limited = StreamFixture.report(owner, 11, limits, count: 2), rows = limited["hits"] as! [[String: Any]]
            for index in rows.indices {
                rows[index]["file_state"] = stop; rows[index]["file_complete"] = false
                if stop != "match-limit" {
                    rows[index]["observed_length"] = String(StreamFixture.length + 1024)
                    rows[index]["final_observed_length"] = String(StreamFixture.length + 1024)
                }
            }
            limited["hits"] = rows; limited["stop_reason"] = stop; limited["search_complete"] = false
            limited["incomplete_files"] = "1"; limited["truncated"] = stop == "match-limit"
            limited["matches_seen"] = stop == "match-limit" ? "3" : "2"
            let decoded = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(StreamFixture.page(limited)),
                catalog: catalog, root: catalog.root, request: limits)
            precondition(!decoded.summary.complete && decoded.summary.truncated == (stop == "match-limit"))
        }
        var incomplete = StreamFixture.report(owner, 8, request, count: 0)
        incomplete["incomplete_files"] = "1"; incomplete["unsupported_files"] = "1"; incomplete["search_complete"] = false
        let incompletePage = try page(StreamFixture.page(incomplete))
        precondition(!incompletePage.summary.complete && incompletePage.summary.unsupportedFiles == 1 && incompletePage.hits.isEmpty)
        let empty = try page(StreamFixture.page(StreamFixture.report(owner, 9, request, count: 0)))
        precondition(empty.summary.complete && empty.hits.isEmpty)
        for consistency in ["changed-during-read", "metadata-unavailable"] {
            var changed = StreamFixture.report(owner, 10, request, count: 1), rows = changed["hits"] as! [[String: Any]]
            rows[0]["consistency"] = consistency; rows[0]["file_complete"] = false
            rows[0]["final_observed_length"] = "1" // Retained original range can exceed final live length.
            changed["hits"] = rows; changed["incomplete_files"] = "1"; changed["search_complete"] = false
            let accepted = try page(StreamFixture.page(changed))
            precondition(accepted.hits[0].finalLength == 1 && !accepted.hits[0].complete)
        }
    }

    private static func scopeWireTests() throws {
        let owner: UInt64 = 0xfedcba9876543210
        let types = try AtlasSearchFileScope(extensions: ["TXT"])
        let folder = try AtlasSearchPathScope(kind: .directory, path: "src")
        let request = try AtlasWorkspaceStreamRequest(needle: "old", fileScope: types, pathScope: folder)
        var catalogWire = StreamFixture.catalog(owner)
        catalogWire["catalogued_files"] = "3"; catalogWire["workspace_files"] = "3"
        let catalog = try AtlasWorkspaceStreamCatalog.decode(StreamFixture.json(catalogWire), owner: owner)
        var base = StreamFixture.report(owner, 1, request, count: 1)
        base["catalogued_files"] = "3"
        base.merge(StreamFixture.scope(request, catalogued: 3, eligible: 1), uniquingKeysWith: { _, next in next })
        func page(_ value: [String: Any], request selected: AtlasWorkspaceStreamRequest? = nil) throws -> AtlasWorkspaceStreamPage {
            try .decode(StreamFixture.json(StreamFixture.page(value)), catalog: catalog, root: catalog.root, request: selected ?? request)
        }
        let accepted = try page(base)
        precondition(accepted.summary.complete && accepted.summary.eligibleFiles == 1 && accepted.summary.excludedFiles == 2)
        precondition(accepted.summary.examinedFiles == 1 && accepted.summary.unexaminedFiles == 0
            && accepted.hits[0].path.contains(255), "Scoped admission preserves native filename bytes")
        let report = try AtlasWorkspaceStreamReport(summary: accepted.summary, hits: accepted.hits)
        let target = report.target(for: report.hits[0]), witness = StreamFixture.witness(base, index: 0)
        _ = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(witness), report: report, root: catalog.root, target: target)
        for (key, value) in [("scope_path", "src-old" as Any), ("scope_path_hex", "7372632d6f6c64" as Any),
            ("scope_path_kind", "workspace" as Any), ("search_scope", "all" as Any),
            ("scope_extensions", "rs" as Any), ("scope_filenames", "cargo.toml" as Any),
            ("case_sensitive", false as Any), ("eligible_files", "01" as Any),
            ("eligible_files", "2" as Any), ("excluded_files", "1" as Any)] {
            var bad = base; bad[key] = value
            rejects("scope receipt " + key) { _ = try page(bad) }
            var badWitness = witness; badWitness[key] = value
            rejects("scope witness " + key) {
                _ = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(badWitness), report: report,
                    root: catalog.root, target: target)
            }
        }
        for key in ["scope_path", "search_scope", "eligible_files", "case_sensitive"] {
            var bad = base; bad.removeValue(forKey: key)
            rejects("missing scope field") { _ = try page(bad) }
        }
        var legacy = base
        for key in StreamFixture.scope(request).keys { legacy.removeValue(forKey: key) }
        rejects("legacy envelope cannot widen a selected scope") { _ = try page(legacy) }
        for raw in [Array("src-old/file.txt".utf8), Array("src/file.rs".utf8), Array("other/file.txt".utf8)] {
            var bad = base, rows = base["hits"] as! [[String: Any]]
            rows[0]["path"] = StreamFixture.path(raw); bad["hits"] = rows
            rejects("returned row outside selected scope") { _ = try page(bad) }
        }
        let exact = try AtlasWorkspaceStreamRequest(needle: "old", fileScope: types,
            pathScope: .init(kind: .file, path: "src/file.txt"))
        var exactWire = base, exactRows = base["hits"] as! [[String: Any]]
        exactRows[0]["path"] = StreamFixture.path(Array("src/file.txt".utf8)); exactWire["hits"] = exactRows
        exactWire.merge(StreamFixture.scope(exact, catalogued: 3, eligible: 1), uniquingKeysWith: { _, next in next })
        let exactPage = try page(exactWire, request: exact)
        precondition(exactPage.summary.complete)
        exactRows[0]["path"] = StreamFixture.path(Array("src/file.txt/child.txt".utf8)); exactWire["hits"] = exactRows
        rejects("exact file is not directory prefix") { _ = try page(exactWire, request: exact) }
        let composed = try AtlasWorkspaceStreamRequest(needle: "old", pathScope: .init(kind: .file, path: "é.txt"))
        let decomposed = try AtlasWorkspaceStreamRequest(needle: "old", pathScope: .init(kind: .file, path: "e\u{301}.txt"))
        precondition(composed != decomposed, "Scope equality must preserve Unix spelling")

        let noTypes = try AtlasWorkspaceStreamRequest(needle: "old", fileScope: .init(extensions: []), pathScope: folder)
        var empty = StreamFixture.report(owner, 2, noTypes, count: 0)
        empty["catalogued_files"] = "3"; empty["files_examined"] = "0"
        for key in ["payload_bytes_read", "scanned_input_bytes", "read_calls", "peak_input_buffer_bytes"] { empty[key] = "0" }
        empty.merge(StreamFixture.scope(noTypes, catalogued: 3, eligible: 0), uniquingKeysWith: { _, next in next })
        let zero = try page(empty, request: noTypes)
        precondition(zero.summary.complete && zero.summary.eligibleFiles == 0 && zero.summary.excludedFiles == 3
            && zero.summary.readCalls == 0 && zero.hits.isEmpty, "An empty custom scope does no reads")

        let progressiveCatalog = try AtlasWorkspaceStreamCatalog.decode(
            StreamFixture.json(StreamProgressFixture.catalog(owner)), owner: owner)
        let progressWire = StreamProgressFixture.progress(owner, 3, request, sequence: 1)
        let progress = try AtlasWorkspaceStreamProgress.decode(StreamFixture.json(progressWire),
            catalog: progressiveCatalog, request: request)
        let terminalWire = StreamProgressFixture.report(owner, 3, request, stopped: true)
        let terminal = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(StreamFixture.page(terminalWire)),
            catalog: progressiveCatalog, root: progressiveCatalog.root, request: request, requireTerminal: true)
        try progress.validateTerminal(terminal)
        precondition(terminal.summary.eligibleFiles == 2 && terminal.summary.unexaminedFiles == 1
            && terminal.summary.stop == "user-stopped", "Stop keeps scoped finalized-file evidence")
        var widened = progressWire; widened["scope_path_kind"] = "workspace"
        rejects("scope change during progressive delivery") {
            _ = try AtlasWorkspaceStreamProgress.decode(StreamFixture.json(widened),
                catalog: progressiveCatalog, request: request)
        }

        #if canImport(CryptoKit)
        let raw = accepted.hits[0].path
        let visible = try AtlasWorkspaceStreamRequest(needle: "old", fileScope: types,
            pathScope: .init(visiblePaths: [raw, Array("absent.txt".utf8)]))
        var visibleWire = base
        visibleWire["search_complete"] = false
        visibleWire.merge(StreamFixture.scope(visible, catalogued: 3, eligible: 1, missing: 1),
            uniquingKeysWith: { _, next in next })
        let incomplete = try page(visibleWire, request: visible)
        precondition(!incomplete.summary.complete && incomplete.summary.missingVisibleFiles == 1)
        visibleWire["search_complete"] = true
        rejects("missing visible member cannot be complete") { _ = try page(visibleWire, request: visible) }
        visibleWire["search_complete"] = false; visibleWire["scope_region_sha256"] = String(repeating: "0", count: 64)
        rejects("changed visible membership") { _ = try page(visibleWire, request: visible) }
        #endif
    }

    private static func countedWireTests() throws {
        let owner: UInt64 = 0xfedcba9876543210
        let request = try AtlasWorkspaceStreamRequest(needle: "old", maximumMatches: 2, countAllMatches: true)
        let bounded = try AtlasWorkspaceStreamRequest(needle: "old", maximumMatches: 2)
        precondition(!bounded.countAllMatches && request != bounded,
            "Full counting is an explicit immutable request option")
        let catalog = try AtlasWorkspaceStreamCatalog.decode(StreamFixture.json(StreamProgressFixture.catalog(owner)), owner: owner)
        let base = StreamFixture.counted(StreamProgressFixture.report(owner, 7, request,
            stopped: false, count: 2), matches: 2_000, matchingFiles: 2)
        func page(_ value: [String: Any], selected: AtlasWorkspaceStreamRequest? = nil) throws -> AtlasWorkspaceStreamPage {
            try .decode(StreamFixture.json(StreamFixture.page(value)), catalog: catalog, root: catalog.root,
                request: selected ?? request, requireTerminal: true)
        }
        func report(_ value: [String: Any]) throws -> AtlasWorkspaceStreamReport {
            let decoded = try page(value)
            return try .init(summary: decoded.summary, hits: decoded.hits)
        }
        let accepted = try report(base)
        precondition(accepted.summary.complete && accepted.summary.matchCountComplete && accepted.summary.truncated)
        precondition(accepted.summary.matchesSeen == 2_000 && accepted.summary.matchingFiles == 2
            && accepted.summary.retainedMatchingFiles == 1 && accepted.hits.count == 2,
            "Later files count even when the retained result inventory is already full")
        precondition(accepted.summary.countLabel == "2000 matches in 2 observed files; first 2 retained")
        precondition(accepted.summary.coverage.contains("counts include later matches"))
        let target = accepted.target(for: accepted.hits[0]), witness = StreamFixture.witness(base, index: 0)
        _ = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(witness), report: accepted,
            root: catalog.root, target: target)
        rejects("Counted reply cannot change a bounded request") { _ = try page(base, selected: bounded) }
        for (key, value) in [("count_all_matches", false as Any), ("count_all_matches", "true" as Any),
            ("match_count_complete", false as Any), ("match_count_complete", NSNull() as Any),
            ("retained_matching_files", "0" as Any), ("retained_matching_files", "2" as Any),
            ("retained_matching_files", "01" as Any), ("matching_files", "3" as Any),
            ("matches_seen", "1" as Any), ("matches_seen", String(UInt64.max) as Any),
            ("truncated", false as Any), ("stop_reason", "match-limit" as Any)] {
            var bad = base; bad[key] = value
            rejects("Counted report \(key)") { _ = try report(bad) }
        }
        let extensionFields = ["count_all_matches", "match_count_complete", "retained_matching_files"]
        for key in extensionFields {
            var bad = base; bad.removeValue(forKey: key)
            rejects("Incomplete count extension \(key)") { _ = try report(bad) }
            var badHit = witness; badHit.removeValue(forKey: key)
            rejects("Counted witness omitted \(key)") {
                _ = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(badHit), report: accepted,
                    root: catalog.root, target: target)
            }
            badHit = witness; badHit[key] = key == "retained_matching_files" ? "2" as Any : false as Any
            rejects("Counted witness changed \(key)") {
                _ = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(badHit), report: accepted,
                    root: catalog.root, target: target)
            }
        }

        // Older bounded-mode receipts remain valid, but an absent or partial
        // extension cannot silently turn a requested full count into that mode.
        let legacy = StreamProgressFixture.report(owner, 8, bounded, stopped: false, count: 2)
        let legacyPage = try page(legacy, selected: bounded)
        precondition(legacyPage.summary.matchCountComplete && legacyPage.summary.retainedMatchingFiles == 1)
        rejects("Counted request accepted a legacy receipt") { _ = try page(legacy) }
        var explicitBounded = legacy
        explicitBounded["count_all_matches"] = false; explicitBounded["match_count_complete"] = true
        explicitBounded["retained_matching_files"] = "1"
        let explicitPage = try page(explicitBounded, selected: bounded)
        precondition(explicitPage.summary == legacyPage.summary)
        for key in extensionFields {
            var partial = explicitBounded; partial.removeValue(forKey: key)
            rejects("Partial bounded count extension") { _ = try page(partial, selected: bounded) }
        }
        var widened = explicitBounded
        widened["matches_seen"] = "2000"; widened["truncated"] = true
        rejects("Legacy mode cannot silently count beyond its result limit") { _ = try page(widened, selected: bounded) }

        let progressWire = StreamFixture.counted(StreamProgressFixture.progress(owner, 7, request,
            sequence: 1, count: 2), matches: 1_000, matchingFiles: 1)
        let progress = try AtlasWorkspaceStreamProgress.decode(StreamFixture.json(progressWire), catalog: catalog, request: request)
        precondition(progress.summary.truncated && !progress.summary.complete && !progress.summary.matchCountComplete)
        precondition(progress.countLabel.contains("1000 matches so far") && progress.page.hits.count == 2)
        try progress.validateTerminal(page(base))
        var falseComplete = progressWire; falseComplete["match_count_complete"] = true
        rejects("Provisional full count cannot claim completion") {
            _ = try AtlasWorkspaceStreamProgress.decode(StreamFixture.json(falseComplete), catalog: catalog, request: request)
        }
        var regressed = base; regressed["matches_seen"] = "999"
        rejects("Terminal count regressed below finalized progress") { try progress.validateTerminal(page(regressed)) }

        let stoppedWire = StreamFixture.counted(StreamProgressFixture.report(owner, 7, request,
            stopped: true, count: 2), matches: 1_000, matchingFiles: 1)
        let stopped = try report(stoppedWire)
        try progress.validateTerminal(page(stoppedWire))
        precondition(stopped.summary.truncated && !stopped.summary.matchCountComplete
            && stopped.summary.countLabel.hasPrefix("At least 1000 matches")
            && stopped.summary.stop == "user-stopped")
        var budget = stoppedWire
        budget["stop_reason"] = "byte-limit"
        budget["payload_bytes_read"] = String(request.maximumScanBytes)
        budget["scanned_input_bytes"] = String(request.maximumScanBytes)
        budget["read_calls"] = String(request.maximumScanBytes / 16_384)
        let limited = try report(budget)
        precondition(!limited.summary.matchCountComplete && limited.summary.countLabel.hasPrefix("At least"))
        budget["match_count_complete"] = true
        rejects("Byte-limited count cannot claim exact totals") { _ = try report(budget) }
    }

    @MainActor private static func countedModelTests() async throws {
        let probe = StreamProbe(), model = AtlasWorkspaceStreamModel(transport: probe.progressiveTransport, query: "old")
        precondition(!model.countAllMatches)
        model.open(.init(root: StreamFixture.root, project: UUID(), browserOwner: 1, access: nil))
        try await wait("counted catalog") { !model.busy && model.catalog != nil }
        model.countAllMatches = true
        let gate = ObservedReaderGate(); probe.blockProgress(gate); model.search()
        try await wait("counting beyond retained rows") { gate.entered && model.progress?.summary.matchesSeen == 1_000 }
        precondition(model.progress?.summary.request.countAllMatches == true
            && model.progress?.summary.retainedHits == 256 && !model.canSelect)
        gate.release()
        try await wait("complete counted inventory") { !model.busy && model.reportAuthorized }
        let accepted = model.report!
        precondition(accepted.summary.matchCountComplete && accepted.summary.matchesSeen == 2_000
            && accepted.summary.matchingFiles == 2 && accepted.summary.retainedMatchingFiles == 1
            && accepted.hits.count == 256 && probe.pageCount == 7)
        model.select(accepted.target(for: accepted.hits[0]))
        try await wait("counted retained literal") { !model.busy && model.witness != nil }
        model.countAllMatches = false
        precondition(model.report == nil && model.witness == nil && !model.canSelect,
            "Changing the counting policy revokes prior evidence and selection authority")

        model.countAllMatches = true
        let obsolete = ObservedReaderGate(); probe.blockProgress(obsolete); model.search()
        try await wait("obsolete counted progress") { obsolete.entered && model.progress != nil }
        model.countAllMatches = false
        precondition(!model.busy && model.progress == nil && model.report == nil)
        obsolete.release(); try await wait("counted worker drain") { probe.activeCount == 0 }
        await Task.yield(); precondition(model.report == nil && model.progress == nil)
        model.search(); try await wait("bounded search after policy change") { !model.busy && model.reportAuthorized }
        precondition(model.report?.summary.request.countAllMatches == false && model.report?.hits.count == 40)
        model.close(); try await wait("counted owners retired") { probe.closedCount == probe.createCount }
    }

    private static func progressWireTests() throws {
        let owner: UInt64 = 0xfedcba9876543210
        let request = try AtlasWorkspaceStreamRequest(needle: "old")
        let catalog = try AtlasWorkspaceStreamCatalog.decode(StreamFixture.json(StreamProgressFixture.catalog(owner)), owner: owner)
        func progress(_ object: [String: Any], previous: AtlasWorkspaceStreamProgress? = nil) throws -> AtlasWorkspaceStreamProgress {
            try .decode(StreamFixture.json(object), catalog: catalog, request: request, previous: previous)
        }
        func terminal(_ object: [String: Any], required: Bool = true) throws -> AtlasWorkspaceStreamPage {
            try .decode(StreamFixture.json(StreamFixture.page(object)), catalog: catalog, root: catalog.root,
                request: request, requireTerminal: required)
        }
        let initialWire = StreamProgressFixture.progress(owner, 7, request,
            sequence: 1, count: 0, examined: 0, payload: 16_384)
        let initial = try progress(initialWire)
        precondition(initial.page.hits.isEmpty && initial.summary.examinedFiles == 0 && initial.summary.payloadBytes == 16_384)
        precondition(!initial.summary.complete && initial.summary.unexaminedFiles == 2)
        let middleWire = StreamProgressFixture.progress(owner, 7, request, sequence: 2)
        let middle = try progress(middleWire, previous: initial)
        precondition(middle.page.hits.count == 32 && middle.summary.retainedHits == 40 && middle.page.next == 32)
        precondition(middle.summary.examinedFiles == 1 && middle.summary.unexaminedFiles == 1)
        precondition(middle.countLabel.contains("so far") && middle.coverage.contains("unfinished file"))
        rejects("Provisional first page treated as a complete retained report") {
            _ = try AtlasWorkspaceStreamReport(summary: middle.summary, hits: middle.page.hits)
        }
        rejects("Provisional callback crossed the terminal decoder") {
            _ = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(middleWire),
                catalog: catalog, root: catalog.root, request: request, requireTerminal: true)
        }
        let activeWire = StreamProgressFixture.progress(owner, 7, request,
            sequence: 3, payload: StreamFixture.length + 32_768)
        let active = try progress(activeWire, previous: middle)
        precondition(active.summary.retainedHits == middle.summary.retainedHits
            && active.summary.payloadBytes > middle.summary.payloadBytes,
            "Active-file I/O must advance without inventing finalized matches")
        let stoppedWire = StreamProgressFixture.report(owner, 7, request, stopped: true,
            payload: StreamFixture.length + 32_768)
        let stopped = try terminal(stoppedWire)
        try active.validateTerminal(stopped)
        precondition(stopped.summary.stop == "user-stopped" && !stopped.summary.complete && !stopped.summary.truncated)
        let tail = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(StreamFixture.page(stoppedWire, offset: 32)),
            catalog: catalog, root: catalog.root, request: request, previous: stopped.summary, offset: 32, requireTerminal: true)
        let stoppedReport = try AtlasWorkspaceStreamReport(summary: stopped.summary, hits: stopped.hits + tail.hits)
        precondition(stoppedReport.hits.count == 40 && stoppedReport.summary.coverage.contains("at your request"))
        let complete = try terminal(StreamProgressFixture.report(owner, 7, request, stopped: false))
        try active.validateTerminal(complete)
        precondition(complete.summary.complete, "A stop racing completed work may receive its complete terminal result")

        for (key, value) in [("terminal", true as Any), ("selection_available", true as Any),
            ("progress_sequence", "0" as Any), ("progress_sequence", "01" as Any),
            ("command", "search" as Any), ("search_complete", true as Any), ("stop_reason", "user-stopped" as Any),
            ("truncated", true as Any), ("scope", "visible" as Any), ("needle", "new" as Any)] {
            var bad = middleWire; bad[key] = value
            rejects("Invalid provisional authority \(key)") { _ = try progress(bad) }
        }
        for field in ["terminal", "selection_available", "progress_sequence"] {
            var bad = middleWire; bad.removeValue(forKey: field)
            rejects("Missing provisional field \(field)") { _ = try progress(bad) }
        }
        rejects("Duplicate progress sequence") { _ = try progress(middleWire, previous: middle) }
        rejects("Reordered progress sequence") { _ = try progress(initialWire, previous: middle) }
        var changedGeneration = activeWire
        changedGeneration["query_generation"] = "8"; changedGeneration["last_attempt"] = "8"
        rejects("Progress borrowed another attempted generation") { _ = try progress(changedGeneration, previous: middle) }
        var regressedBytes = middleWire; regressedBytes["progress_sequence"] = "4"
        rejects("Cumulative active-file I/O regressed") { _ = try progress(regressedBytes, previous: active) }
        var regressedRows = initialWire; regressedRows["progress_sequence"] = "4"
        rejects("Finalized-file matches disappeared") { _ = try progress(regressedRows, previous: middle) }
        for (key, value) in [("witness_sha256", String(repeating: "c", count: 64)), ("source_revision", "9"),
                            ("path", "")] {
            var bad = activeWire, rows = bad["hits"] as! [[String: Any]]
            if key == "path" { rows[0]["path"] = StreamFixture.path(Array("different.txt".utf8)) }
            else { rows[0][key] = value }
            bad["hits"] = rows
            rejects("A published finalized prefix changed \(key)") { _ = try progress(bad, previous: middle) }
        }
        var changedPolicy = activeWire; changedPolicy["excluded_entries"] = "3"
        rejects("Discovery coverage changed within one scan") { _ = try progress(changedPolicy, previous: middle) }
        var missingPrefix = middleWire
        missingPrefix["hits"] = Array((middleWire["hits"] as! [[String: Any]]).dropLast())
        missingPrefix["page_count"] = "31"
        rejects("Progress page silently shortened the required first32") { _ = try progress(missingPrefix) }
        var terminalGeneration = stoppedWire
        terminalGeneration["query_generation"] = "8"; terminalGeneration["last_attempt"] = "8"
        let wrongTerminal = try terminal(terminalGeneration)
        rejects("Final response replaced the progress generation") { try active.validateTerminal(wrongTerminal) }
        let lessRead = try terminal(StreamProgressFixture.report(owner, 7, request, stopped: true))
        rejects("Final response lost active-file I/O") { try active.validateTerminal(lessRead) }
        var absentFlags = stoppedWire
        absentFlags.removeValue(forKey: "terminal"); absentFlags.removeValue(forKey: "selection_available")
        rejects("Progressive terminal omitted its authority boundary") { _ = try terminal(absentFlags) }
        _ = try terminal(absentFlags, required: false) // Explicit legacy fallback only.
        var oneFlag = absentFlags; oneFlag["terminal"] = true
        rejects("Partial terminal metadata was treated as legacy") { _ = try terminal(oneFlag, required: false) }
        var falseTerminal = stoppedWire; falseTerminal["terminal"] = false
        rejects("Provisional state disguised as final command") { _ = try terminal(falseTerminal) }
        let emptyStopped = try terminal(StreamProgressFixture.report(owner, 7, request,
            stopped: true, count: 0, examined: 0, payload: 16_384))
        precondition(emptyStopped.hits.isEmpty && !emptyStopped.summary.complete
            && emptyStopped.summary.unexaminedFiles == 2,
            "An early stop must not turn absence of finalized matches into a complete no-match result")
    }

    @MainActor private static func progressModelTests() async throws {
        let probe = StreamProbe(), model = AtlasWorkspaceStreamModel(transport: probe.progressiveTransport, query: "old")
        precondition(probe.transport.progressiveSearch == nil, "Legacy injected transports retain their optional boundary")
        model.open(.init(root: StreamFixture.root, project: UUID(), browserOwner: 1, access: nil))
        try await wait("progress catalog") { !model.busy && model.catalog?.files == 2 }
        let scan = ObservedReaderGate(), paging = ObservedReaderGate()
        probe.blockProgress(scan); probe.blockPage(paging); model.search()
        try await wait("typed progress before scan completes") { scan.entered && model.progress?.sequence == 2 }
        precondition(model.progress?.summary.retainedHits == 40 && model.progress?.page.hits.count == 32)
        precondition(model.report == nil && !model.canSelect && !model.canReadCurrentContext && model.canStopAndKeep)
        let cancelCalls = probe.cancelCount
        model.stopAndKeep()
        precondition(model.busy && model.stopping && !model.canStopAndKeep && probe.cancelCount == cancelCalls,
            "Stop must retain the task, operation ticket, cancellation flag and atlas owner")
        scan.release()
        try await wait("stopped terminal still paging retained rows") { paging.entered }
        precondition(model.busy && !model.canSelect && !model.reportAuthorized,
            "Stopping does not authorize a first-page-only inventory")
        paging.release()
        try await wait("stopped inventory accepted") { !model.busy && model.reportAuthorized }
        precondition(model.report?.hits.count == 40 && model.report?.summary.stop == "user-stopped")
        precondition(model.report?.summary.unexaminedFiles == 1 && model.progress == nil && !model.stopping)
        precondition(probe.stopCount == 1 && !probe.stopCanceledWork && model.canSelect)
        let stopped = model.report!, target = stopped.target(for: stopped.hits[0])
        model.select(target); try await wait("stopped retained literal selection") { !model.busy && model.witness != nil }
        precondition(model.witness?.target == target && model.witness?.text == "old")

        // The C scan has already returned a complete result, but metadata paging
        // is still in flight. A late Stop request does not downgrade that result.
        let completePaging = ObservedReaderGate(); probe.blockPage(completePaging); model.search()
        try await wait("complete result metadata paging") { completePaging.entered }
        model.stopAndKeep(); completePaging.release()
        try await wait("stop raced complete scan") { !model.busy && model.reportAuthorized }
        precondition(model.report?.summary.complete == true && model.report?.summary.stop == nil)

        let obsolete = ObservedReaderGate(); probe.blockProgress(obsolete); model.search()
        try await wait("obsolete progress accepted for display") { obsolete.entered && model.progress != nil }
        model.queryText = "edited"
        precondition(!model.busy && model.progress == nil && model.report == nil && model.witness == nil)
        obsolete.release(); try await wait("edited progress scan drained") { probe.activeCount == 0 }
        await Task.yield(); precondition(model.progress == nil && model.report == nil)

        let queued = ObservedReaderGate(); probe.blockProgress(queued); model.search()
        try await wait("bounded progressive worker") { queued.entered && model.progress != nil }
        let before = probe.searchCount
        model.search(); await Task.yield(); model.search(); await Task.yield()
        queued.release()
        try await wait("latest progressive replacement") { !model.busy && model.reportAuthorized }
        precondition(probe.searchCount == before + 1 && probe.maximumActive == 1)
        precondition(model.report?.summary.request.needle == "edited" && model.progress == nil)

        let accepted = model.report!.summary.identity
        probe.poisonProgress(); model.search()
        try await wait("malformed progress aborts attempt") { !model.busy }
        precondition(model.notice == AtlasWorkspaceStreamError.invalidResponse.message)
        precondition(model.report?.summary.identity == accepted && !model.reportAuthorized && model.progress == nil)
        probe.repeatGeneration(); model.search()
        try await wait("terminal progress identity mismatch") { !model.busy }
        precondition(model.report?.summary.identity == accepted && !model.canSelect)

        let burst = ObservedReaderGate(); probe.burstNextProgress(); probe.blockProgress(burst); model.search()
        try await wait("latest coalesced progress snapshot") { burst.entered && model.progress?.sequence == 128 }
        precondition(model.progress?.page.hits.count == 32 && !model.canSelect)
        burst.release(); try await wait("burst terminal publication") { !model.busy && model.reportAuthorized }
        for _ in 0..<3 { await Task.yield() }
        precondition(model.progress == nil && model.report?.summary.complete == true,
            "Queued progress must never overwrite a terminal result")

        let canceled = ObservedReaderGate(); probe.blockProgress(canceled); model.search()
        try await wait("cancelable progressive scan") { canceled.entered && model.progress != nil }
        let previousIdentity = model.report!.summary.identity
        model.cancel(); precondition(!model.busy && model.progress == nil && !model.canSelect)
        canceled.release(); try await wait("discarded progressive scan drains") { probe.activeCount == 0 }
        await Task.yield()
        precondition(model.report?.summary.identity == previousIdentity && !model.reportAuthorized)

        model.open(.init(root: StreamFixture.root, project: UUID(), browserOwner: 2,
            access: StreamGrant(probe, "progress-drain")))
        try await wait("new progressive grant catalog") { !model.busy && model.catalog != nil }
        let draining = ObservedReaderGate(); probe.blockProgress(draining); model.search()
        try await wait("progress owner holds grant") { draining.entered && model.progress != nil }
        model.close()
        precondition(model.closed && model.progress == nil && !probe.hasReleased("progress-drain"))
        draining.release()
        try await wait("progress owner retirement after callback drain") {
            probe.hasReleased("progress-drain") && probe.closedCount == probe.createCount
        }
        precondition(model.report == nil && model.progress == nil)
    }

    @MainActor private static func matchWindowModelTests() async throws {
        let probe = StreamProbe(), model = AtlasWorkspaceStreamModel(transport: probe.transport, query: "old")
        probe.enableMatchWindows()
        model.open(.init(root: StreamFixture.root, project: UUID(), browserOwner: 1, access: nil))
        try await wait("match-window catalog") { !model.busy && model.catalog != nil }
        model.search(); try await wait("initial match window") { !model.busy && model.reportAuthorized }
        let first = model.report!, oldTarget = first.target(for: first.hits[0])
        precondition(first.hits.count == 256 && model.canScanLater && !model.canScanEarlier)
        precondition(first.summary.matchesSeen == 257 && first.summary.request.pageStartAfter == 0)
        model.scanLater()
        precondition(!model.canSelect && !model.canScanLater && model.report?.summary.identity == first.summary.identity)
        try await wait("second match window") { !model.busy && model.reportAuthorized }
        let second = model.report!
        precondition(second.hits.count == 256 && second.summary.request.pageStartAfter == 256)
        precondition(second.hits.first?.occurrence == 257 && second.hits.last?.occurrence == 512)
        precondition(second.summary.identity.generation > first.summary.identity.generation
            && second.hits[0].revision > first.hits[0].revision && !second.admits(oldTarget))
        let priorHits = probe.hitCount
        model.select(oldTarget); precondition(probe.hitCount == priorHits && !model.busy)
        model.select(second.target(for: second.hits[0]))
        try await wait("later retained witness") { !model.busy && model.witness != nil }
        precondition(model.witness?.target.hit.occurrence == 257)
        model.scanLater(); try await wait("final match window") { !model.busy && model.reportAuthorized }
        let last = model.report!
        precondition(last.hits.count == 88 && last.summary.request.pageStartAfter == 512
            && last.summary.complete && last.summary.truncated)
        precondition(last.hits.first?.occurrence == 513 && last.hits.last?.occurrence == 600)
        precondition(!model.canScanLater && model.canScanEarlier, "An omitted prefix is not a later match")
        model.scanEarlier(); try await wait("earlier fresh scan") { !model.busy && model.reportAuthorized }
        precondition(model.report?.summary.request.pageStartAfter == 256 && model.report?.hits.count == 256)
        precondition(model.report!.summary.identity.generation > last.summary.identity.generation)

        // Cancellation revokes the pending new observation, including its
        // eventual late receipt, while preserving the previous bytes read-only.
        let accepted = model.report!, gate = ObservedReaderGate()
        probe.blockSearch(gate); model.scanLater()
        try await wait("later scan entered") { gate.entered }
        model.cancel(); gate.release()
        try await wait("later scan drained") { probe.activeCount == 0 }
        await Task.yield()
        precondition(model.report?.summary.identity == accepted.summary.identity
            && !model.reportAuthorized && !model.canScanLater && !model.canScanEarlier)
        model.search(); try await wait("restart from beginning") { !model.busy && model.reportAuthorized }
        precondition(model.report?.summary.request.pageStartAfter == 0 && model.report?.hits.first?.occurrence == 1)

        let beforeFailure = model.report!
        probe.failSearch(); model.scanLater(); try await wait("rejected later response") { !model.busy }
        precondition(model.report?.summary.identity == beforeFailure.summary.identity && !model.canSelect)
        model.queryText = "different"
        precondition(model.report == nil && model.witness == nil && !model.canScanLater)
        model.close(); try await wait("match-window owner retired") { probe.closedCount == probe.createCount }
    }
    @MainActor private static func modelTests() async throws {
        let probe = StreamProbe(), model = AtlasWorkspaceStreamModel(transport: probe.transport, query: "old")
        model.open(.init(root: "/selected-alias/", project: UUID(), browserOwner: 99, access: nil))
        try await wait("canonical catalog") { model.catalog != nil && !model.busy }
        precondition(model.catalog?.root == StreamFixture.root && probe.searchCount == 0)
        model.search(); try await wait("all pages") { model.report != nil && !model.busy }
        precondition(model.report!.hits.count == 40 && probe.pageCount == 1)
        let first = model.report!, target = first.target(for: first.hits[0])
        model.select(target); try await wait("retained match") { model.witness != nil && !model.busy }
        precondition(model.witness?.text == "old")
        probe.failSearch(); model.search(); try await wait("uncertain replacement") { !model.busy }
        precondition(model.report?.summary.identity == first.summary.identity && model.witness?.target == target)
        precondition(!model.canSelect && !model.canReadCurrentContext && model.coverage?.contains("read-only") == true)
        let hitCalls = probe.hitCount; model.select(target); precondition(probe.hitCount == hitCalls)
        probe.failPage(); model.search(); try await wait("late page rejection") { !model.busy }
        precondition(model.report?.summary.identity == first.summary.identity && !model.reportAuthorized)
        model.search(); try await wait("replacement accepted") { model.reportAuthorized && !model.busy }
        let current = model.report!, currentTarget = current.target(for: current.hits[0])
        model.select(currentTarget); try await wait("selected replacement") { model.witness != nil && !model.busy }

        let contextProbe = StreamContextProbe()
        model.readCurrentContext(transport: contextProbe.transport)
        let context = model.currentContext!; context.start()
        try await wait("new context observation") { context.model.page != nil && !context.model.busy }
        precondition(context.source.input.root == StreamFixture.root && context.model.info!.identity.owner != current.summary.identity.owner)
        precondition(contextProbe.requestedOffsets == [StreamFixture.start - 1024])
        precondition(context.model.searchSelection == NSRange(location: 1024, length: 3))
        contextProbe.change(); context.model.reopenCurrentFile()
        try await wait("changed current context") { !context.model.busy }
        precondition(context.model.externalContext == .changed && context.model.searchSelection == nil)
        model.queryText = "different"
        precondition(context.model.closed && model.currentContext == nil && model.report == nil && model.witness == nil)

        let lateSearch = ObservedReaderGate(); probe.blockSearch(lateSearch); model.search()
        try await wait("obsolete search entry") { lateSearch.entered }
        model.queryText = "old"; lateSearch.release()
        try await wait("obsolete search drain") { probe.activeCount == 0 }
        await Task.yield(); precondition(model.report == nil)
        model.search(); try await wait("new search") { model.report != nil && !model.busy }
        let latest = model.report!, hitGate = ObservedReaderGate(); probe.blockHit(hitGate)
        model.select(latest.target(for: latest.hits[0])); try await wait("obsolete hit entry") { hitGate.entered }
        model.scanBytes = 1 << 30; hitGate.release()
        try await wait("obsolete hit drain") { probe.activeCount == 0 }
        await Task.yield(); precondition(model.witness == nil && model.report == nil)

        let queueGate = ObservedReaderGate(); probe.blockSearch(queueGate); model.search()
        try await wait("bounded scan") { queueGate.entered }
        let before = probe.searchCount
        model.search(); await Task.yield(); model.search(); await Task.yield(); queueGate.release()
        try await wait("latest waiting scan") { !model.busy && model.reportAuthorized }
        precondition(probe.searchCount == before + 1 && probe.maximumActive == 1)
        let generation = model.report!.summary.identity.generation
        probe.repeatGeneration(); model.search(); try await wait("duplicate generation") { !model.busy }
        precondition(model.report?.summary.identity.generation == generation && !model.reportAuthorized)

        let oldOwner = model.catalog!.owner
        probe.failOpen(); model.refreshCatalog(); try await wait("failed refresh") { !model.busy }
        precondition(model.catalog?.owner == oldOwner && model.report == nil)
        model.refreshCatalog(); try await wait("fresh catalog") { !model.busy && model.catalog?.owner != oldOwner }
        model.search(); try await wait("fresh root search") { model.report != nil && !model.busy }
        model.clear(); precondition(model.report == nil && model.witness == nil)
        try await wait("clear receipt") { !model.busy }
        precondition(model.canSearch)
        model.close(); try await wait("model owner retirement") { probe.closedCount == probe.createCount }
    }

    @MainActor private static func scopedModelTests() async throws {
        let probe = StreamProbe(), model = AtlasWorkspaceStreamModel(transport: probe.transport, query: "old")
        let project = UUID(), types = try AtlasSearchFileScope(extensions: ["txt"])
        var selectedLocation = try AtlasSearchPathScope(kind: .directory, path: "src")
        let source = AtlasWorkspaceStreamSource(root: StreamFixture.root, project: project, browserOwner: 4,
            access: nil, fileScope: types, pathScope: selectedLocation)
        model.open(source)
        // Subsequent sidebar edits cannot mutate the scope of this sheet.
        selectedLocation = .workspace
        precondition(source.pathScope != selectedLocation && source.scopeDescription.contains("Folder:")
            && source.scopeDescription.contains(".txt"))
        try await wait("scoped catalog") { model.catalog != nil && !model.busy }
        model.search(); try await wait("scoped all pages") { model.report != nil && !model.busy }
        let first = model.report!
        precondition(first.summary.request.fileScope == types && first.summary.request.pathScope == source.pathScope
            && first.hits.allSatisfy { source.pathScope.admitsRawPath($0.path) && types.admitsRawPath($0.path) })
        model.select(first.target(for: first.hits[0]))
        try await wait("scoped witness") { model.witness != nil && !model.busy }
        model.queryText = "new"
        precondition(model.report == nil && model.witness == nil)
        model.search(); try await wait("same scope new literal") { model.report != nil && !model.busy }
        precondition(model.report?.summary.request.pathScope == source.pathScope)
        let owner = model.catalog!.owner
        model.refreshCatalog()
        try await wait("scope retained after catalog refresh") { model.catalog?.owner != owner && !model.busy }
        model.search(); try await wait("refreshed scoped results") { model.report != nil && !model.busy }
        precondition(model.report?.summary.request.fileScope == types && model.report?.summary.request.pathScope == source.pathScope)

        let gate = ObservedReaderGate(); probe.blockSearch(gate); model.search()
        try await wait("old scope work") { gate.entered }
        model.open(.init(root: StreamFixture.root, project: project, browserOwner: 4, access: nil))
        gate.release()
        try await wait("replacement scope catalog") { model.catalog != nil && !model.busy }
        precondition(model.report == nil && model.progress == nil, "Old scope work cannot publish after replacement")
        model.search(); try await wait("replacement workspace results") { model.report != nil && !model.busy }
        precondition(model.report!.summary.request.fileScope.isAll && model.report!.summary.request.pathScope.isWorkspace)
        model.close(); try await wait("scoped owners retired") { probe.closedCount == probe.createCount }
    }

    @MainActor private static func lifetimeTests() async throws {
        let probe = StreamProbe(), model = AtlasWorkspaceStreamModel(transport: probe.transport, query: "old")
        model.open(.init(root: StreamFixture.root, project: UUID(), browserOwner: 1, access: StreamGrant(probe, "drain")))
        try await wait("grant catalog") { !model.busy && model.catalog != nil }
        let gate = ObservedReaderGate(); probe.blockPage(gate); model.search()
        try await wait("entered page continuation") { gate.entered }
        model.cancel(); model.close()
        precondition(!probe.hasReleased("drain") && probe.closedCount == 0)
        gate.release(); try await wait("grant survives drain") { probe.hasReleased("drain") && probe.closedCount == 1 }
        precondition(model.closed && model.report == nil)

        probe.allowClose(false)
        model.open(.init(root: StreamFixture.root, project: UUID(), browserOwner: 2, access: StreamGrant(probe, "quarantine")))
        try await wait("quarantine catalog") { !model.busy && model.catalog != nil }
        model.close(); try await wait("failed retirement") { probe.failureCount > 0 }
        precondition(!probe.hasReleased("quarantine"))
        probe.allowClose(true); AtlasWorkspaceStreamModel.retryRetirements()
        try await wait("explicit bounded retirement retry") { probe.hasReleased("quarantine") && probe.closedCount == probe.createCount }
    }
}


private extension AtlasWorkspaceStreamTests {
    static func lowercaseWireTests() throws {
        let owner: UInt64 = 0xfedcba9876543210
        let catalog = try AtlasWorkspaceStreamCatalog.decode(StreamFixture.json(StreamFixture.catalog(owner)), owner: owner)
        let request = try AtlasWorkspaceStreamRequest(needle: "k", caseSensitive: false)
        let exact = try AtlasWorkspaceStreamRequest(needle: "k")
        precondition(request != exact && !request.caseSensitive)
        for encoding in AtlasObservedReaderEncoding.allCases {
            let wire = StreamFixture.report(owner, 1, request, count: 3, encoding: encoding, spellings: ["k", "K", "K"])
            func decode(_ value: [String: Any], selected: AtlasWorkspaceStreamRequest? = nil) throws -> AtlasWorkspaceStreamReport {
                let page = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(StreamFixture.page(value)),
                    catalog: catalog, root: catalog.root, request: selected ?? request)
                return try .init(summary: page.summary, hits: page.hits)
            }
            let report = try decode(wire)
            let bytes = ["k", "K", "K"].reduce(0) {
                $0 + AtlasObservedReaderSearchRequest.originalBytes($1, encoding: encoding).count
            }
            precondition(report.summary.witnessBytes == UInt64(bytes) && report.hits.count == 3)
            for (index, spelling) in ["k", "K", "K"].enumerated() {
                let hit = report.hits[index], target = report.target(for: hit)
                let witness = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(StreamFixture.witness(wire, index: index)),
                    report: report, root: catalog.root, target: target)
                precondition(witness.text.utf8.elementsEqual(spelling.utf8))
                precondition(witness.originalHex == ObservedReaderFixture.hex(
                    AtlasObservedReaderSearchRequest.originalBytes(spelling, encoding: encoding)))
                precondition(hit.range.count == UInt64(witness.originalHex.utf8.count / 2))
            }
            var invalid = wire; invalid["retained_witness_bytes"] = String(bytes + 1)
            rejects("lowercase per-occurrence witness accounting") { _ = try decode(invalid) }
            invalid = wire; invalid["case_sensitive"] = true
            rejects("wrong matching policy") { _ = try decode(invalid) }
            invalid = wire; invalid.removeValue(forKey: "case_sensitive")
            rejects("missing lowercase scope policy") { _ = try decode(invalid) }
            rejects("lowercase receipt admitted as legacy exact") { _ = try decode(wire, selected: .init(needle: "k")) }
            var badWitness = StreamFixture.witness(wire, index: 0)
            var row = badWitness["hit"] as! [String: Any]; row["witness_sha256"] = String(repeating: "c", count: 64)
            badWitness["hit"] = row
            rejects("witness from another occurrence") {
                _ = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(badWitness), report: report,
                    root: catalog.root, target: report.target(for: report.hits[0]))
            }
            if encoding != .utf8 {
                invalid = wire; var hits = invalid["hits"] as! [[String: Any]]
                hits[0]["original_range"] = ObservedReaderFixture.range(StreamFixture.start + 1, StreamFixture.start + 3)
                invalid["hits"] = hits
                rejects("misaligned UTF16 lowercase witness") { _ = try decode(invalid) }
            }
        }

        // Expansion evidence retains the entire contributing source scalar.
        // Distinct engine occurrence IDs must survive even when raw ranges
        // coincide; raw byte ranges are not an occurrence identity.
        let expanded = try AtlasWorkspaceStreamRequest(needle: "\u{307}", caseSensitive: false)
        var repeated = StreamFixture.report(owner, 2, expanded, count: 2, spellings: ["İ", "İ"])
        var rows = repeated["hits"] as! [[String: Any]]
        rows[1]["original_range"] = rows[0]["original_range"]
        rows[1]["witness_sha256"] = rows[0]["witness_sha256"]; repeated["hits"] = rows
        let page = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(StreamFixture.page(repeated)),
            catalog: catalog, root: catalog.root, request: expanded)
        let report = try AtlasWorkspaceStreamReport(summary: page.summary, hits: page.hits)
        precondition(report.hits.count == 2 && report.hits[0].range == report.hits[1].range
            && report.hits[0].occurrence != report.hits[1].occurrence && report.summary.witnessBytes == 4)
        let original = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(StreamFixture.witness(repeated, index: 1)),
            report: report, root: catalog.root, target: report.target(for: report.hits[1]))
        precondition(original.text == "İ" && original.originalHex == "c4b0")

        let counted = try AtlasWorkspaceStreamRequest(needle: "k", maximumMatches: 3, countAllMatches: true, caseSensitive: false)
        let full = StreamFixture.counted(StreamFixture.report(owner, 3, counted, count: 3, spellings: ["K", "k", "K"]),
            matches: 99, matchingFiles: 1)
        let countedPage = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(StreamFixture.page(full)),
            catalog: catalog, root: catalog.root, request: counted)
        let countedReport = try AtlasWorkspaceStreamReport(summary: countedPage.summary, hits: countedPage.hits)
        precondition(countedReport.summary.matchCountComplete && countedReport.summary.truncated
            && countedReport.summary.matchesSeen == 99 && countedReport.summary.witnessBytes == 5)
    }

    @MainActor static func lowercaseModelTests() async throws {
        let probe = StreamProbe(), model = AtlasWorkspaceStreamModel(transport: probe.progressiveTransport, query: "old")
        precondition(model.caseSensitive)
        model.open(.init(root: StreamFixture.root, project: UUID(), browserOwner: 1, access: nil))
        try await wait("lowercase catalog") { !model.busy && model.catalog != nil }
        model.caseSensitive = false; model.search()
        try await wait("lowercase report") { !model.busy && model.reportAuthorized }
        let report = model.report!, target = report.target(for: report.hits[0])
        precondition(!report.summary.request.caseSensitive && report.summary.witnessBytes == 120)
        model.select(target); try await wait("lowercase retained spelling") { !model.busy && model.witness != nil }
        let contextProbe = StreamContextProbe(); model.readCurrentContext(transport: contextProbe.transport)
        let context = model.currentContext!; context.start()
        try await wait("lowercase current context") { context.model.page != nil && !context.model.busy }
        model.caseSensitive = true
        precondition(model.report == nil && model.witness == nil && !model.canSelect
            && model.currentContext == nil && context.model.closed)
        let calls = probe.hitCount; model.select(target); precondition(probe.hitCount == calls)

        model.caseSensitive = false
        let late = ObservedReaderGate(); probe.blockProgress(late); model.search()
        try await wait("lowercase progressive work") { late.entered && model.progress != nil }
        model.caseSensitive = true
        precondition(!model.busy && model.progress == nil && model.report == nil)
        late.release(); try await wait("lowercase progressive drain") { probe.activeCount == 0 }
        await Task.yield(); precondition(model.report == nil && model.progress == nil)
        for policy in [true, false] {
            model.caseSensitive = policy; model.search()
            try await wait("policy-specific report before clear") { !model.busy && model.reportAuthorized }
            precondition(model.report?.summary.request.caseSensitive == policy)
            model.clear(); try await wait("clear has no query matching policy") { !model.busy }
            precondition(model.report == nil && model.witness == nil && model.canSearch
                && model.notice.contains("Retained search literals cleared"))
        }
        model.caseSensitive = false; model.countAllMatches = true; model.search()
        try await wait("counted lowercase inventory") { !model.busy && model.reportAuthorized }
        precondition(model.report?.summary.request.countAllMatches == true
            && model.report?.summary.request.caseSensitive == false
            && model.report?.summary.matchCountComplete == true && model.report?.summary.matchesSeen == 2_000
            && model.report?.summary.witnessBytes == 256 * 3)
        model.close(); try await wait("lowercase owner retirement") { probe.closedCount == probe.createCount }
    }
}


private extension AtlasWorkspaceStreamTests {
    static func occurrenceWireTests() throws {
        let owner: UInt64 = 0xfedcba9876543210
        let catalog = try AtlasWorkspaceStreamCatalog.decode(StreamFixture.json(StreamFixture.catalog(owner)), owner: owner)
        let request = try AtlasWorkspaceStreamRequest(needle: "old", mode: .occurrences)
        let literalRequest = try AtlasWorkspaceStreamRequest(needle: "old")
        precondition(request != literalRequest)
        rejects("case-insensitive identifier request") {
            _ = try AtlasWorkspaceStreamRequest(needle: "old", caseSensitive: false, mode: .occurrences)
        }
        for encoding in AtlasObservedReaderEncoding.allCases {
            let wire = StreamFixture.report(owner, 1, request, count: 3, encoding: encoding)
            func decode(_ value: [String: Any], selected: AtlasWorkspaceStreamRequest? = nil) throws -> AtlasWorkspaceStreamReport {
                let page = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(StreamFixture.page(value)),
                    catalog: catalog, root: catalog.root, request: selected ?? request)
                return try .init(summary: page.summary, hits: page.hits)
            }
            let report = try decode(wire)
            precondition(report.summary.request.mode == .occurrences && report.summary.matchCountComplete
                && report.summary.witnessBytes == UInt64(AtlasObservedReaderSearchRequest.originalBytes("old", encoding: encoding).count))
            for index in report.hits.indices {
                let witness = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(StreamFixture.witness(wire, index: index)),
                    report: report, root: catalog.root, target: report.target(for: report.hits[index]))
                precondition(witness.text == "old" && witness.originalHex == ObservedReaderFixture.hex(
                    AtlasObservedReaderSearchRequest.originalBytes("old", encoding: encoding)))
            }
            for (key, value) in [("mode", "exact-decoded-literal" as Any),
                ("evidence_level", "compiler-reference" as Any), ("includes_comments_and_literals", false as Any),
                ("compiler_resolved", true as Any), ("semantic_complete", true as Any), ("case_sensitive", false as Any)] {
                var bad = wire; bad[key] = value
                rejects("identifier report policy \(key)") { _ = try decode(bad) }
                var badWitness = StreamFixture.witness(wire, index: 0); badWitness[key] = value
                rejects("identifier witness policy \(key)") {
                    _ = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(badWitness), report: report,
                        root: catalog.root, target: report.target(for: report.hits[0]))
                }
            }
            for key in ["evidence_level", "includes_comments_and_literals", "compiler_resolved",
                "semantic_complete", "count_all_matches", "match_count_complete", "retained_matching_files",
                "search_scope", "case_sensitive", "terminal"] {
                var bad = wire; bad.removeValue(forKey: key)
                rejects("missing identifier contract \(key)") { _ = try decode(bad) }
            }
            rejects("identifier receipt admitted for a literal request") {
                _ = try decode(wire, selected: .init(needle: "old"))
            }
            let legacy = StreamFixture.report(owner, 1, try .init(needle: "old"), count: 3, encoding: encoding)
            rejects("legacy literal receipt admitted as identifiers") { _ = try decode(legacy) }
        }
        let counted = try AtlasWorkspaceStreamRequest(needle: "old", maximumMatches: 3, countAllMatches: true, mode: .occurrences)
        let full = StreamFixture.counted(StreamFixture.report(owner, 2, counted, count: 3), matches: 99, matchingFiles: 1)
        let page = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(StreamFixture.page(full)),
            catalog: catalog, root: catalog.root, request: counted)
        let report = try AtlasWorkspaceStreamReport(summary: page.summary, hits: page.hits)
        precondition(report.summary.matchesSeen == 99 && report.summary.truncated
            && report.summary.matchCountComplete && report.summary.witnessBytes == 3)

        let progressiveCatalog = try AtlasWorkspaceStreamCatalog.decode(StreamFixture.json(StreamProgressFixture.catalog(owner)), owner: owner)
        let middle = StreamProgressFixture.progress(owner, 3, request, sequence: 2)
        let progress = try AtlasWorkspaceStreamProgress.decode(StreamFixture.json(middle),
            catalog: progressiveCatalog, request: request)
        precondition(!progress.summary.matchCountComplete && progress.summary.request.mode == .occurrences)
        var wrongMode = middle; wrongMode["mode"] = "exact-decoded-literal"
        rejects("literal callback in an identifier search") {
            _ = try AtlasWorkspaceStreamProgress.decode(StreamFixture.json(wrongMode),
                catalog: progressiveCatalog, request: request)
        }
        let stopped = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(StreamFixture.page(
            StreamProgressFixture.report(owner, 3, request, stopped: true))), catalog: progressiveCatalog,
            root: progressiveCatalog.root, request: request, requireTerminal: true)
        try progress.validateTerminal(stopped)
        precondition(stopped.summary.stop == "user-stopped" && !stopped.summary.matchCountComplete)
    }

    @MainActor static func occurrenceModelTests() async throws {
        let probe = StreamProbe(), model = AtlasWorkspaceStreamModel(transport: probe.progressiveTransport,
            query: "old", caseSensitive: false)
        model.open(.init(root: StreamFixture.root, project: UUID(), browserOwner: 1, access: nil))
        try await wait("identifier catalog") { !model.busy && model.catalog != nil }
        model.search(); try await wait("initial lowercase text") { !model.busy && model.reportAuthorized }
        let literal = model.report!, oldTarget = literal.target(for: literal.hits[0])
        model.select(oldTarget); try await wait("old literal witness") { !model.busy && model.witness != nil }
        let contextProbe = StreamContextProbe(); model.readCurrentContext(transport: contextProbe.transport)
        let context = model.currentContext!; context.start()
        try await wait("old literal context") { context.model.page != nil && !context.model.busy }
        model.mode = .occurrences
        precondition(!model.caseSensitive && model.report == nil && model.witness == nil && !model.canSelect
            && model.currentContext == nil && context.model.closed)
        let calls = probe.hitCount; model.select(oldTarget); precondition(probe.hitCount == calls)
        model.search(); try await wait("whole identifier inventory") { !model.busy && model.reportAuthorized }
        let report = model.report!
        precondition(report.summary.request.mode == .occurrences && report.summary.request.caseSensitive
            && report.summary.witnessBytes == 3 && report.summary.coverage.contains("no compiler resolution"))
        model.select(report.target(for: report.hits[0]))
        try await wait("whole identifier witness") { !model.busy && model.witness != nil }
        precondition(model.witness?.text == "old")
        let late = ObservedReaderGate(); probe.blockProgress(late); model.search()
        try await wait("pending identifier progress") { late.entered && model.progress != nil }
        model.mode = .text
        precondition(!model.busy && model.progress == nil && model.report == nil && model.witness == nil)
        late.release(); try await wait("identifier mode change drains") { probe.activeCount == 0 }
        await Task.yield(); precondition(model.report == nil && model.progress == nil)
        model.search(); try await wait("restored text policy") { !model.busy && model.reportAuthorized }
        precondition(model.report?.summary.request.mode == .text && model.report?.summary.request.caseSensitive == false)
        model.mode = .occurrences; model.countAllMatches = true; model.search()
        try await wait("counted whole identifiers") { !model.busy && model.reportAuthorized }
        precondition(model.report?.summary.matchesSeen == 2_000 && model.report?.summary.matchCountComplete == true
            && model.report?.summary.retainedHits == 256 && model.report?.summary.witnessBytes == 3)
        model.clear(); try await wait("identifier clear") { !model.busy }
        precondition(model.canSearch && model.report == nil && model.witness == nil
            && model.notice.contains("Retained search literals cleared"))
        model.close(); try await wait("identifier owner retirement") { probe.closedCount == probe.createCount }
    }
}

private extension AtlasWorkspaceStreamTests {
    static let expressionQuery = "\"old\" required -forbidden path:src/"

    static func expressionWireTests() throws {
        let owner: UInt64 = 0xfedcba9876543210
        let catalog = try AtlasWorkspaceStreamCatalog.decode(StreamFixture.json(StreamFixture.catalog(owner)), owner: owner)
        let request = try AtlasWorkspaceStreamRequest(needle: expressionQuery, maximumMatches: 3, mode: .expression)
        precondition(request.countAllMatches && request.mode.wireMode(caseSensitive: false) == "decoded-text-expression")
        let explicitlyCounted = try AtlasWorkspaceStreamRequest(needle: expressionQuery, maximumMatches: 3,
            countAllMatches: true, mode: .expression)
        precondition(request == explicitlyCounted, "Expression predicates always require observation to the end")
        rejects("expression endpoint cannot resume a later match window") {
            _ = try AtlasWorkspaceStreamRequest(needle: expressionQuery, mode: .expression, pageStartAfter: 1)
        }
        let metadata = ["expression", "primary_needle", "query_syntax", "predicate_scope",
            "path_filter_domain", "expression_scope_files", "expression_excluded_files"]

        for encoding in AtlasObservedReaderEncoding.allCases {
            let wire = StreamFixture.report(owner, 1, request, count: 3, encoding: encoding, primaryNeedle: "old")
            func decode(_ value: [String: Any]) throws -> AtlasWorkspaceStreamReport {
                let page = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(StreamFixture.page(value)),
                    catalog: catalog, root: catalog.root, request: request)
                return try .init(summary: page.summary, hits: page.hits)
            }
            let report = try decode(wire)
            let target = report.target(for: report.hits[0])
            precondition(report.summary.primaryNeedle == "old" && report.summary.expression?.query == expressionQuery
                && report.summary.expression?.scopeFiles == 1 && report.summary.expression?.excludedFiles == 0)
            precondition(report.summary.matchCountComplete && report.summary.request.needle == expressionQuery)
            let witness = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(StreamFixture.witness(wire, index: 0)),
                report: report, root: catalog.root, target: target)
            precondition(witness.text == "old" && witness.originalHex == ObservedReaderFixture.hex(
                AtlasObservedReaderSearchRequest.originalBytes("old", encoding: encoding)))
            precondition(target.hit.range.count == UInt64(witness.originalHex.utf8.count / 2),
                "Ranges and selected evidence describe the primary literal, never the raw query")

            for key in metadata + ["count_all_matches", "match_count_complete", "retained_matching_files",
                "search_scope", "case_sensitive", "terminal", "selection_available"] {
                var bad = wire; bad.removeValue(forKey: key)
                rejects("missing expression contract \(key)") { _ = try decode(bad) }
            }
            let changed: [(String, Any)] = [
                ("expression", expressionQuery + " "), ("primary_needle", "new"),
                ("query_syntax", "fcb-bounded-expression/2"), ("predicate_scope", "same-path-multiple-reads"),
                ("path_filter_domain", "display-path"), ("expression_scope_files", "0"),
                ("expression_excluded_files", "1"), ("needle", expressionQuery),
                ("count_all_matches", false), ("mode", "exact-decoded-literal")]
            for (key, value) in changed {
                var bad = wire; bad[key] = value
                rejects("changed expression report \(key)") { _ = try decode(bad) }
                var selected = StreamFixture.witness(wire, index: 0); selected[key] = value
                rejects("changed expression witness \(key)") {
                    _ = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(selected), report: report,
                        root: catalog.root, target: target)
                }
            }
            for key in metadata {
                var selected = StreamFixture.witness(wire, index: 0); selected.removeValue(forKey: key)
                rejects("missing selected expression evidence \(key)") {
                    _ = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(selected), report: report,
                        root: catalog.root, target: target)
                }
            }
            var wrongRange = wire, wrongRows = wire["hits"] as! [[String: Any]]
            let rawBytes = AtlasObservedReaderSearchRequest.originalBytes(expressionQuery, encoding: encoding)
            wrongRows[0]["original_range"] = ObservedReaderFixture.range(StreamFixture.start,
                StreamFixture.start + UInt64(rawBytes.count)); wrongRange["hits"] = wrongRows
            rejects("expression-length witness range") { _ = try decode(wrongRange) }

            // A file stopped before its observed end cannot establish required
            // or absent terms, even if its primary literal has already matched.
            for state in ["match-limit", "byte-limit", "read-call-limit", "short-read", "failed"] {
                var unfinished = wire, rows = wire["hits"] as! [[String: Any]]
                unfinished["search_complete"] = false; unfinished["match_count_complete"] = false
                unfinished["incomplete_files"] = "1"
                for index in rows.indices { rows[index]["file_state"] = state; rows[index]["file_complete"] = false }
                unfinished["hits"] = rows
                rejects("expression matches from unfinished \(state) input") { _ = try decode(unfinished) }
            }
            // Reaching the observed end qualifies predicates over that stream.
            // Changed/unavailable metadata still keeps overall coverage partial.
            for consistency in ["changed-during-read", "metadata-unavailable"] {
                var changedSource = wire, rows = wire["hits"] as! [[String: Any]]
                changedSource["search_complete"] = false; changedSource["match_count_complete"] = false
                changedSource["incomplete_files"] = "1"
                for index in rows.indices {
                    rows[index]["consistency"] = consistency; rows[index]["file_complete"] = false
                    rows[index]["final_observed_length"] = consistency == "metadata-unavailable"
                        ? NSNull() : String(StreamFixture.length + 2) as Any
                }
                changedSource["hits"] = rows
                let observed = try decode(changedSource)
                precondition(!observed.summary.complete && !observed.hits[0].complete
                    && observed.hits[0].state == "complete-observed-input")
            }
            let full = StreamFixture.counted(wire, matches: 2_000, matchingFiles: 1)
            let counted = try decode(full)
            precondition(counted.summary.matchCountComplete && counted.summary.truncated
                && counted.hits.count == 3 && counted.summary.matchesSeen == 2_000)
        }

        // Literal and identifier receipts cannot be decorated with expression
        // evidence, including a null field that would otherwise look absent.
        for mode in [AtlasWorkspaceStreamMode.text, .occurrences] {
            let literalRequest = try AtlasWorkspaceStreamRequest(needle: "old", mode: mode)
            let literal = StreamFixture.report(owner, 1, literalRequest, count: 1)
            let first = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(StreamFixture.page(literal)),
                catalog: catalog, root: catalog.root, request: literalRequest)
            let report = try AtlasWorkspaceStreamReport(summary: first.summary, hits: first.hits)
            for key in metadata {
                var mixed = literal; mixed[key] = NSNull()
                rejects("mixed \(mode.rawValue) expression field \(key)") {
                    _ = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(StreamFixture.page(mixed)),
                        catalog: catalog, root: catalog.root, request: literalRequest)
                }
                var selected = StreamFixture.witness(literal, index: 0); selected[key] = NSNull()
                rejects("mixed selected literal expression field \(key)") {
                    _ = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(selected), report: report,
                        root: catalog.root, target: report.target(for: report.hits[0]))
                }
            }
        }

        let lower = try AtlasWorkspaceStreamRequest(needle: "\"k\" required -forbidden", maximumMatches: 3,
            caseSensitive: false, mode: .expression)
        for encoding in AtlasObservedReaderEncoding.allCases {
            let wire = StreamFixture.report(owner, 2, lower, count: 3, encoding: encoding,
                spellings: ["k", "K", "K"], primaryNeedle: "k")
            let page = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(StreamFixture.page(wire)),
                catalog: catalog, root: catalog.root, request: lower)
            let report = try AtlasWorkspaceStreamReport(summary: page.summary, hits: page.hits)
            for index in report.hits.indices {
                let selected = try AtlasWorkspaceStreamWitness.decode(StreamFixture.json(StreamFixture.witness(wire, index: index)),
                    report: report, root: catalog.root, target: report.target(for: report.hits[index]))
                precondition(selected.text.utf8.elementsEqual(["k", "K", "K"][index].utf8))
            }
        }
        try expressionCoverageAndProgress(owner: owner, request: request)

        for code in ["QUERY_EMPTY", "QUERY_SYNTAX_ERROR", "QUERY_REGEX_UNQUALIFIED", "QUERY_LIMIT_EXCEEDED"] {
            let refused: [String: Any] = ["schema": "fcb.atlas-stream-search/1", "status": "error",
                "owner": String(owner), "error": ["code": code]]
            do {
                _ = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(refused), catalog: catalog,
                    root: catalog.root, request: request)
                preconditionFailure("Expression refusal became an empty result")
            } catch AtlasWorkspaceStreamError.engine(let received) { precondition(received == code) }
            precondition(AtlasWorkspaceStreamError.engine(code).message == AtlasSearchError.query(code).message)
        }
    }

    static func expressionCoverageAndProgress(owner: UInt64, request: AtlasWorkspaceStreamRequest) throws {
        var catalogWire = StreamFixture.catalog(owner)
        catalogWire["catalogued_files"] = "4"; catalogWire["workspace_files"] = "4"
        let catalog = try AtlasWorkspaceStreamCatalog.decode(StreamFixture.json(catalogWire), owner: owner)
        let scoped = try AtlasWorkspaceStreamRequest(needle: expressionQuery, maximumMatches: 3,
            fileScope: .init(extensions: ["txt"]), mode: .expression)
        var wire = StreamFixture.report(owner, 3, scoped, count: 3, primaryNeedle: "old")
        wire["catalogued_files"] = "4"
        wire.merge(StreamFixture.scope(scoped, catalogued: 4, eligible: 3), uniquingKeysWith: { _, next in next })
        wire["expression_excluded_files"] = "2"
        func scopedPage(_ value: [String: Any]) throws -> AtlasWorkspaceStreamPage {
            try .decode(StreamFixture.json(StreamFixture.page(value)), catalog: catalog, root: catalog.root, request: scoped)
        }
        let filtered = try scopedPage(wire)
        precondition(filtered.summary.complete && filtered.summary.eligibleFiles == 3
            && filtered.summary.excludedFiles == 1 && filtered.summary.searchedFiles == 1
            && filtered.summary.expression?.excludedFiles == 2 && filtered.summary.examinedFiles == 1)
        var wrong = wire; wrong["files_examined"] = "3"
        rejects("expression filters charged as examined files") { _ = try scopedPage(wrong) }
        wrong = wire; wrong["files_unexamined"] = "2"
        rejects("expression-excluded files charged as unexamined") { _ = try scopedPage(wrong) }
        wrong = wire; wrong["expression_excluded_files"] = "4"
        rejects("expression exclusions exceed native scope") { _ = try scopedPage(wrong) }

        var empty = StreamFixture.report(owner, 4, scoped, count: 0, primaryNeedle: "old")
        empty["catalogued_files"] = "4"
        empty.merge(StreamFixture.scope(scoped, catalogued: 4, eligible: 3), uniquingKeysWith: { _, next in next })
        empty["expression_scope_files"] = "0"; empty["expression_excluded_files"] = "3"
        for key in ["files_examined", "payload_bytes_read", "scanned_input_bytes", "read_calls", "peak_input_buffer_bytes"] {
            empty[key] = "0"
        }
        let emptyPage = try scopedPage(empty)
        precondition(emptyPage.summary.complete && emptyPage.summary.matchCountComplete
            && emptyPage.summary.searchedFiles == 0 && emptyPage.summary.payloadBytes == 0)

        let progressiveCatalog = try AtlasWorkspaceStreamCatalog.decode(StreamFixture.json(StreamProgressFixture.catalog(owner)), owner: owner)
        let beginning = StreamProgressFixture.progress(owner, 5, request, sequence: 1, count: 0,
            examined: 0, payload: 16_384, primaryNeedle: "old")
        let first = try AtlasWorkspaceStreamProgress.decode(StreamFixture.json(beginning), catalog: progressiveCatalog, request: request)
        var replacement = beginning; replacement["progress_sequence"] = "2"
        replacement["needle"] = "new"; replacement["primary_needle"] = "new"
        rejects("parsed primary changes before any rows exist") {
            _ = try AtlasWorkspaceStreamProgress.decode(StreamFixture.json(replacement),
                catalog: progressiveCatalog, request: request, previous: first)
        }
        replacement = beginning; replacement["progress_sequence"] = "2"
        replacement["expression_scope_files"] = "1"; replacement["expression_excluded_files"] = "1"
        replacement["files_unexamined"] = "1"
        rejects("expression path-filter universe changes during progress") {
            _ = try AtlasWorkspaceStreamProgress.decode(StreamFixture.json(replacement),
                catalog: progressiveCatalog, request: request, previous: first)
        }
        let middle = StreamProgressFixture.progress(owner, 5, request, sequence: 2, count: 3, primaryNeedle: "old")
        let progress = try AtlasWorkspaceStreamProgress.decode(StreamFixture.json(middle),
            catalog: progressiveCatalog, request: request, previous: first)
        let stoppedWire = StreamProgressFixture.report(owner, 5, request, stopped: true, count: 3, primaryNeedle: "old")
        let stopped = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(StreamFixture.page(stoppedWire)),
            catalog: progressiveCatalog, root: progressiveCatalog.root, request: request, requireTerminal: true)
        try progress.validateTerminal(stopped)
        precondition(stopped.summary.stop == "user-stopped" && !stopped.summary.matchCountComplete
            && stopped.summary.examinedFiles == 1 && stopped.summary.unexaminedFiles == 1)

        let paged = try AtlasWorkspaceStreamRequest(needle: expressionQuery, mode: .expression)
        let pages = StreamProgressFixture.report(owner, 6, paged, stopped: false, primaryNeedle: "old")
        let initial = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(StreamFixture.page(pages)),
            catalog: progressiveCatalog, root: progressiveCatalog.root, request: paged)
        var last = StreamFixture.page(pages, offset: 32)
        last["needle"] = "new"; last["primary_needle"] = "new"
        rejects("parsed primary changes between terminal pages") {
            _ = try AtlasWorkspaceStreamPage.decode(StreamFixture.json(last), catalog: progressiveCatalog,
                root: progressiveCatalog.root, request: paged, previous: initial.summary, offset: 32)
        }
    }

    @MainActor static func expressionModelTests() async throws {
        let probe = StreamProbe(expressionPrimary: "old")
        let model = AtlasWorkspaceStreamModel(transport: probe.progressiveTransport,
            query: expressionQuery, caseSensitive: false, mode: .expression)
        precondition(!model.countAllMatches && model.effectiveCountAllMatches)
        model.open(.init(root: StreamFixture.root, project: UUID(), browserOwner: 1, access: nil))
        try await wait("expression catalog") { !model.busy && model.catalog != nil }
        let gate = ObservedReaderGate(); probe.blockProgress(gate); model.search()
        try await wait("expression primary progress") { gate.entered && model.progress?.summary.retainedHits == 256 }
        precondition(model.progress?.summary.primaryNeedle == "old"
            && model.progress?.summary.request.needle == expressionQuery
            && model.progress?.summary.request.countAllMatches == true && !model.canSelect)
        model.stopAndKeep(); gate.release()
        try await wait("stopped expression pages") { !model.busy && model.reportAuthorized }
        let report = model.report!, target = report.target(for: report.hits[0])
        precondition(report.hits.count == 256 && report.summary.matchesSeen == 1_000
            && report.summary.stop == "user-stopped" && !report.summary.matchCountComplete
            && probe.stopCount == 1 && !probe.stopCanceledWork)
        model.select(target); try await wait("expression primary witness") { !model.busy && model.witness != nil }
        precondition(model.witness?.text == "old")
        let contextProbe = StreamContextProbe(); model.readCurrentContext(transport: contextProbe.transport)
        let context = model.currentContext!; context.start()
        try await wait("expression current literal context") { context.model.page != nil && !context.model.busy }
        precondition(context.model.searchSelection == NSRange(location: 1024, length: 3))
        model.queryText = expressionQuery + " additional"
        precondition(context.model.closed && model.currentContext == nil && model.report == nil && model.witness == nil)
        let hitCalls = probe.hitCount; model.select(target); precondition(probe.hitCount == hitCalls)

        let late = ObservedReaderGate(); probe.blockProgress(late); model.search()
        try await wait("expression work before mode change") { late.entered && model.progress != nil }
        model.mode = .text
        precondition(!model.effectiveCountAllMatches && !model.countAllMatches && !model.busy
            && model.progress == nil && model.report == nil)
        late.release(); try await wait("expression revoked worker drains") { probe.activeCount == 0 }
        await Task.yield(); precondition(model.report == nil && model.progress == nil)

        model.queryText = "old"; model.search()
        try await wait("restored literal counting preference") { !model.busy && model.reportAuthorized }
        precondition(model.report?.summary.request.mode == .text && model.report?.summary.request.countAllMatches == false)
        model.mode = .expression; model.queryText = expressionQuery; model.search()
        try await wait("complete counted expression") { !model.busy && model.reportAuthorized }
        precondition(model.report?.summary.matchesSeen == 2_000 && model.report?.summary.matchCountComplete == true
            && model.report?.summary.retainedHits == 256 && model.report?.summary.expression != nil)
        precondition(!model.canScanLater && !model.canScanEarlier)
        let searches = probe.searchCount; model.scanLater(); model.scanEarlier()
        precondition(probe.searchCount == searches && model.reportAuthorized,
            "Expression results cannot dispatch to literal later-window scans")
        model.clear(); try await wait("clear expression evidence") { !model.busy }
        precondition(model.canSearch && model.report == nil && model.witness == nil)
        model.close(); try await wait("expression owner retirement") { probe.closedCount == probe.createCount }
    }
}
