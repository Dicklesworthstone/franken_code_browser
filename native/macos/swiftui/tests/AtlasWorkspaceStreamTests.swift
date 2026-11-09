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
    static func report(_ owner: UInt64, _ generation: UInt64, _ request: AtlasWorkspaceStreamRequest,
                       count: Int = 40, encoding: AtlasObservedReaderEncoding = .utf8) -> [String: Any] {
        var value = common(owner, generation, "search")
        let bytes = AtlasObservedReaderSearchRequest.originalBytes(request.needle, encoding: encoding)
        value["needle"] = request.needle; value["discovery_complete"] = true
        value["search_complete"] = true; value["truncated"] = false; value["stop_reason"] = NSNull()
        value["files_examined"] = "1"; value["files_unexamined"] = "0"
        value["incomplete_files"] = "0"; value["unavailable_files"] = "0"
        value["unsupported_files"] = "0"; value["failed_files"] = "0"
        value["matching_files"] = count == 0 ? "0" : "1"
        value["retained_hits"] = String(count); value["matches_seen"] = String(count)
        value["payload_bytes_read"] = String(length); value["scanned_input_bytes"] = String(length)
        value["read_calls"] = "256"; value["interrupted_read_calls"] = "0"
        value["peak_input_buffer_bytes"] = "16384"
        value["retained_witness_bytes"] = String(count == 0 ? 0 : bytes.count)
        value["limits"] = ["max_matches": String(request.maximumMatches),
            "max_scan_bytes": String(request.maximumScanBytes), "max_read_calls": String(request.maximumReadCalls)]
        value["hits"] = (0..<count).map { index -> [String: Any] in
            ["index": String(index), "occurrence_id": String(index + 1), "file_id": "1", "node_ordinal": "5",
             "source_revision": String(generation), "path": path([0x73, 0x72, 0x63, 0x2f, 0xff, 0x2e, 0x74, 0x78, 0x74]),
             "original_range": ObservedReaderFixture.range(start + UInt64(index * 8), start + UInt64(index * 8 + bytes.count)),
             "encoding": encoding.rawValue, "observed_length": String(length), "final_observed_length": String(length),
             "consistency": "unchanged-metadata-not-atomic", "file_state": "complete-observed-input", "file_complete": true,
             "witness_sha256": String(repeating: "b", count: 64)]
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
        let text = report["needle"] as! String, encoding = AtlasObservedReaderEncoding(rawValue: row["encoding"] as! String)!
        row["text"] = text
        row["original_hex"] = ObservedReaderFixture.hex(AtlasObservedReaderSearchRequest.originalBytes(text, encoding: encoding))
        value["hit"] = row; value.removeValue(forKey: "hits")
        value["additional_source_bytes_read"] = "0"; value["additional_read_calls"] = "0"
        return value
    }
}

private final class StreamProbe: @unchecked Sendable {
    private let lock = NSLock()
    private var nextOwner: UInt64 = 100
    private var generations: [UInt64: UInt64] = [:]
    private var reports: [UInt64: [String: Any]] = [:]
    private var searchGate: ObservedReaderGate?, hitGate: ObservedReaderGate?, pageGate: ObservedReaderGate?
    private var badSearch = false, badPage = false, badOpen = false, staleSearch = false
    private var searches = 0, hits = 0, pages = 0, active = 0, mostActive = 0, created = 0
    private var closed = Set<UInt64>(), released = Set<String>()
    private var closeAllowed = true, retirementFailures = 0
    private func locked<T>(_ work: () throws -> T) rethrows -> T {
        lock.lock(); defer { lock.unlock() }; return try work()
    }
    var transport: AtlasWorkspaceStreamTransport {
        .init(create: { self.locked { self.nextOwner += 1; self.created += 1; return self.nextOwner } },
            open: { owner, _, _ in try self.open(owner) },
            search: { owner, request, _ in try self.search(owner, request) },
            page: { owner, generation, start, count in try self.page(owner, generation, start, count) },
            hit: { owner, generation, index in try self.hit(owner, generation, index) },
            clear: { owner in try self.clear(owner) }, cancel: { _ in true },
            close: { owner in self.locked { guard self.closeAllowed else { return false }; self.closed.insert(owner); return true } },
            retirementFailed: { self.locked { self.retirementFailures += 1 } })
    }
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
        let (gate, fails, stale, generation) = locked {
            let result = (searchGate, badSearch, staleSearch, (generations[owner] ?? 0) + 1)
            searchGate = nil; badSearch = false; staleSearch = false
            searches += 1; generations[owner] = result.3
            return result
        }
        gate?.wait()
        var report = StreamFixture.report(owner, stale ? generation - 1 : generation, request)
        locked { reports[owner] = report }
        if fails { report["scope"] = "visible-only" } // Backend already replaced before bad delivery.
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
        try await modelTests()
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
