import Foundation

/// Injected wire/model evidence only. These fixtures do not replace the Rust
/// streaming tests or claim native rendering/large-workspace qualification.
private enum ObservedSearchFixture {
    static func report(_ owner: UInt64, _ input: AtlasObservedReaderInput,
        _ request: AtlasObservedReaderSearchRequest, observation: UInt64 = 2,
        length: UInt64 = 64, offsets: [UInt64] = [4], state: String = "complete-observed-input",
        consistency: String = "unchanged-metadata-not-atomic") -> [String: Any] {
        var out = ObservedReaderFixture.common(owner, input, "find")
        let bytes = min(length, request.maximumScanBytes)
        let literalBytes = UInt64(request.originalHex.utf8.count / 2)
        out["query_generation"] = String(request.generation); out["search_observation"] = String(observation)
        out["encoding"] = request.encoding.rawValue; out["encoding_basis"] = "explicit-for-this-search"
        out["mode"] = "exact-decoded-text"; out["case_sensitive"] = true
        out["retention"] = "literal-witnesses-only"; out["literal_text"] = request.needle
        out["literal_original_hex"] = offsets.isEmpty ? NSNull() : request.originalHex as Any
        out["literal_sha256"] = offsets.isEmpty ? NSNull() : String(repeating: "b", count: 64) as Any
        out["observed_length"] = String(length); out["final_observed_length"] = String(length)
        out["state"] = state; out["consistency"] = consistency
        out["input_complete"] = state == "complete-observed-input"
        out["search_complete"] = state == "complete-observed-input" && consistency == "unchanged-metadata-not-atomic"
        out["max_matches"] = String(request.maximumMatches); out["max_scan_bytes"] = String(request.maximumScanBytes)
        out["max_read_calls"] = String(AtlasObservedReaderSearchLimits.readCalls)
        out["payload_bytes_read"] = String(bytes); out["scanned_input_bytes"] = String(bytes)
        out["read_calls"] = bytes == 0 ? "0" : "1"
        out["peak_input_buffer_bytes"] = String(min(bytes, AtlasObservedReaderSearchLimits.inputBufferBytes))
        out["matches_seen"] = String(offsets.count + (state == "match-limit" ? 1 : 0))
        out["retained_hits"] = String(offsets.count); out["truncated"] = state == "match-limit"
        out["unsupported_at"] = NSNull()
        out["additional_source_bytes_read"] = String(bytes); out["additional_read_calls"] = out["read_calls"]
        out["hits"] = offsets.enumerated().map { index, start in
            ["index": String(index), "occurrence_id": String(index + 1),
             "original_range": ObservedReaderFixture.range(start, start + literalBytes)] as [String: Any]
        }
        return out
    }
    static func witness(_ report: [String: Any], index: Int = 0) -> [String: Any] {
        var out = report; out["command"] = "search-hit"
        let hits = report["hits"] as! [[String: Any]]
        for (key, value) in hits[index] { out[key] = value }
        out["text"] = report["literal_text"]; out["original_hex"] = report["literal_original_hex"]
        out["witness_sha256"] = report["literal_sha256"]
        out["additional_source_bytes_read"] = "0"; out["additional_read_calls"] = "0"
        return out
    }
}

private final class ObservedSearchProbe: @unchecked Sendable {
    let lifetime = ObservedReaderProbe()
    private let verifiedNavigation: Bool
    private let lock = NSLock()
    private var inputs: [UInt64: AtlasObservedReaderInput] = [:]
    private var revisions: [UInt64: UInt64] = [:]
    private var reports: [UInt64: [String: Any]] = [:]
    private var pages: [UInt64: [UInt64: [String: Any]]] = [:]
    private var content = "old\n"
    private var nextFindGate: ObservedReaderGate?, nextHitGate: ObservedReaderGate?, nextWindowGate: ObservedReaderGate?
    private var nextFindReplyGate: ObservedReaderGate?
    private var findFailure = false, staleFind = false, staleWindow = false
    private var verifiedFailure = false, malformedNeighbor = false
    private var active = 0, mostActive = 0, windows = 0, hits = 0
    private var verifiedJumps = 0
    private var generations: [UInt64] = []

    init(verifiedNavigation: Bool = false) { self.verifiedNavigation = verifiedNavigation }

    var transport: AtlasObservedReaderTransport {
        let base = lifetime.transport
        let verified: (@Sendable (UInt64, AtlasObservedReaderSearchWitness, AtlasObservedReaderRequest,
            AtlasSearchCancellation) throws -> String?)? = verifiedNavigation ? { owner, witness, request, _ in
                try self.verifiedJump(owner, witness, request)
            } : nil
        return .init(create: base.create,
            open: { owner, input, flag in
                let reply = try base.open(owner, input, flag)
                self.recordOpen(owner, input)
                return reply
            }, info: base.info,
            window: { owner, request, _ in try self.window(owner, request) },
            retained: { owner, observation in try self.retained(owner, observation) },
            copy: { owner, observation, start, end in try self.copy(owner, observation, start, end) },
            cancel: base.cancel, close: base.close, retirementFailed: base.retirementFailed,
            search: .init(find: { owner, request, _ in try self.find(owner, request) },
                hit: { owner, generation, index in try self.hit(owner, generation, index) }, observeHit: verified))
    }
    private func recordOpen(_ owner: UInt64, _ input: AtlasObservedReaderInput) {
        lock.lock(); defer { lock.unlock() }; inputs[owner] = input; revisions[owner] = 0
    }
    func setContent(_ value: String) { lock.lock(); content = value; lock.unlock() }
    func blockFind(_ gate: ObservedReaderGate) { lock.lock(); nextFindGate = gate; lock.unlock() }
    func blockFindReply(_ gate: ObservedReaderGate) { lock.lock(); nextFindReplyGate = gate; lock.unlock() }
    func blockHit(_ gate: ObservedReaderGate) { lock.lock(); nextHitGate = gate; lock.unlock() }
    func blockWindow(_ gate: ObservedReaderGate) { lock.lock(); nextWindowGate = gate; lock.unlock() }
    func failFind() { lock.lock(); findFailure = true; lock.unlock() }
    func failVerifiedJump() { lock.lock(); verifiedFailure = true; lock.unlock() }
    func useMalformedNeighbor() { lock.lock(); malformedNeighbor = true; lock.unlock() }
    func duplicateFindObservation() { lock.lock(); staleFind = true; lock.unlock() }
    func duplicateWindowObservation() { lock.lock(); staleWindow = true; lock.unlock() }
    var activeCount: Int { lock.lock(); defer { lock.unlock() }; return active }
    var maximumActive: Int { lock.lock(); defer { lock.unlock() }; return mostActive }
    var windowCount: Int { lock.lock(); defer { lock.unlock() }; return windows }
    var hitCount: Int { lock.lock(); defer { lock.unlock() }; return hits }
    var verifiedJumpCount: Int { lock.lock(); defer { lock.unlock() }; return verifiedJumps }
    var findGenerations: [UInt64] { lock.lock(); defer { lock.unlock() }; return generations }
    private func finish() { lock.lock(); active -= 1; lock.unlock() }
    private func revision(_ owner: UInt64) -> UInt64 {
        let next = (revisions[owner] ?? 0) + 1; revisions[owner] = next; return next
    }
    private func window(_ owner: UInt64, _ request: AtlasObservedReaderRequest) throws -> String? {
        lock.lock()
        guard let input = inputs[owner] else { lock.unlock(); return nil }
        let bytes = AtlasObservedReaderSearchRequest.originalBytes(content, encoding: request.encoding)
        let id = revision(owner), stale = staleWindow, gate = nextWindowGate
        staleWindow = false; nextWindowGate = nil; windows += 1; active += 1; mostActive = max(mostActive, active)
        lock.unlock(); defer { finish() }; gate?.wait()
        guard request.offset <= UInt64(bytes.count) else {
            return try ObservedReaderFixture.failure(owner, "OBSERVED_READER_OFFSET")
        }
        let end = Int(min(UInt64(bytes.count), request.offset + request.bytes))
        let selected = Array(bytes[Int(request.offset)..<end])
        let encoding: String.Encoding = switch request.encoding {
        case .utf8: .utf8
        case .utf16le: .utf16LittleEndian
        case .utf16be: .utf16BigEndian
        }
        let text = String(bytes: selected, encoding: encoding) ?? ""
        let page = ObservedReaderFixture.page(owner, input, stale ? id - 1 : id, request,
            length: UInt64(bytes.count), text: text, originalHex: ObservedReaderFixture.hex(selected))
        lock.lock(); pages[owner, default: [:]][id] = page; lock.unlock()
        return try ObservedReaderFixture.encode(page)
    }
    private func find(_ owner: UInt64, _ request: AtlasObservedReaderSearchRequest) throws -> String? {
        lock.lock()
        guard let input = inputs[owner] else { lock.unlock(); return nil }
        let id = revision(owner), fail = findFailure, stale = staleFind, gate = nextFindGate, text = content
        let replyGate = nextFindReplyGate
        findFailure = false; staleFind = false; nextFindGate = nil; nextFindReplyGate = nil
        generations.append(request.generation); active += 1; mostActive = max(mostActive, active)
        lock.unlock(); defer { finish() }; gate?.wait()
        if fail { return try ObservedReaderFixture.failure(owner, "OBSERVED_READER_SEARCH_FAILED") }
        let bytes = AtlasObservedReaderSearchRequest.originalBytes(text, encoding: request.encoding)
        let needle = AtlasObservedReaderSearchRequest.originalBytes(request.needle, encoding: request.encoding)
        let count = min(bytes.count, Int(request.maximumScanBytes))
        var offsets: [UInt64] = []
        if count >= needle.count {
            for offset in 0...(count - needle.count) where bytes[offset..<(offset + needle.count)].elementsEqual(needle) {
                offsets.append(UInt64(offset))
                if offsets.count > Int(request.maximumMatches) { break }
            }
        }
        let state: String
        if offsets.count > Int(request.maximumMatches) { offsets.removeLast(); state = "match-limit" }
        else { state = count == bytes.count ? "complete-observed-input" : "byte-limit" }
        let report = ObservedSearchFixture.report(owner, input, request, observation: stale ? id - 1 : id,
            length: UInt64(bytes.count), offsets: offsets, state: state)
        lock.lock(); reports[owner] = report; lock.unlock()
        replyGate?.wait()
        return try ObservedReaderFixture.encode(report)
    }

    private func verifiedJump(_ owner: UInt64, _ witness: AtlasObservedReaderSearchWitness,
                              _ request: AtlasObservedReaderRequest) throws -> String? {
        lock.lock()
        guard let input = inputs[owner], let report = reports[owner],
              report["query_generation"] as? String == String(witness.target.generation) else {
            lock.unlock(); return try ObservedReaderFixture.failure(owner, "OBSERVED_READER_STALE_QUERY")
        }
        let id = revision(owner), text = content, fail = verifiedFailure, malformed = malformedNeighbor
        verifiedFailure = false; verifiedJumps += 1
        lock.unlock()
        if fail { return try ObservedReaderFixture.failure(owner, "OBSERVED_READER_SEARCH_HIT_CHANGED") }
        var bytes = AtlasObservedReaderSearchRequest.originalBytes(text, encoding: request.encoding)
        if malformed && request.encoding == .utf8 && !bytes.isEmpty { bytes[0] = 0xff }
        let hit = witness.target.hit.range
        guard request.offset <= hit.start, hit.end <= UInt64(bytes.count),
              ObservedReaderFixture.hex(Array(bytes[Int(hit.start)..<Int(hit.end)])) == witness.originalHex else {
            return try ObservedReaderFixture.failure(owner, "OBSERVED_READER_SEARCH_HIT_CHANGED")
        }
        let end = Int(min(UInt64(bytes.count), request.offset + request.bytes))
        guard hit.end <= UInt64(end) else { return try ObservedReaderFixture.failure(owner, "OBSERVED_READER_SEARCH_HIT_CHANGED") }
        let selected = Array(bytes[Int(request.offset)..<end])
        let encoding: String.Encoding = switch request.encoding {
        case .utf8: .utf8
        case .utf16le: .utf16LittleEndian
        case .utf16be: .utf16BigEndian
        }
        let decoded = request.encoding == .utf8 ? String(decoding: selected, as: UTF8.self)
            : (String(bytes: selected, encoding: encoding) ?? "")
        let prefix = Array(bytes[Int(request.offset)..<Int(hit.start)])
        let decodedPrefix = request.encoding == .utf8 ? String(decoding: prefix, as: UTF8.self)
            : (String(bytes: prefix, encoding: encoding) ?? "")
        let decodedStart = UInt64(decodedPrefix.utf8.count)
        var page = ObservedReaderFixture.page(owner, input, id, request, length: UInt64(bytes.count),
            text: decoded, originalHex: ObservedReaderFixture.hex(selected))
        page["command"] = "observe-hit"; page["has_replacements"] = malformed
        page["find_hit"] = ["query_generation": String(witness.target.generation),
            "search_observation": String(witness.target.observation), "hit_index": String(witness.target.hit.index),
            "occurrence_id": String(witness.target.hit.occurrence), "original_range": ObservedReaderFixture.range(hit.start, hit.end),
            "decoded_utf8_range": ObservedReaderFixture.range(decodedStart, decodedStart + UInt64(witness.text.utf8.count)),
            "witness_verified": true, "verification": "equal-original-bytes-in-new-observation"]
        lock.lock(); pages[owner, default: [:]][id] = page; lock.unlock()
        return try ObservedReaderFixture.encode(page)
    }
    private func hit(_ owner: UInt64, _ generation: UInt64, _ index: UInt64) throws -> String? {
        lock.lock()
        guard let report = reports[owner], report["query_generation"] as? String == String(generation),
              let rows = report["hits"] as? [[String: Any]], index < UInt64(rows.count) else {
            lock.unlock(); return try ObservedReaderFixture.failure(owner, "OBSERVED_READER_STALE_QUERY")
        }
        let gate = nextHitGate; nextHitGate = nil
        hits += 1; active += 1; mostActive = max(mostActive, active)
        lock.unlock(); defer { finish() }; gate?.wait()
        return try ObservedReaderFixture.encode(ObservedSearchFixture.witness(report, index: Int(index)))
    }
    private func retained(_ owner: UInt64, _ observation: UInt64) throws -> String? {
        lock.lock(); defer { lock.unlock() }
        guard let page = pages[owner]?[observation] else { return nil }
        return try ObservedReaderFixture.encode(ObservedReaderFixture.retained(page))
    }
    private func copy(_ owner: UInt64, _ observation: UInt64, _ start: UInt64, _ end: UInt64) throws -> String? {
        lock.lock(); defer { lock.unlock() }
        guard let page = pages[owner]?[observation],
              let range = page["visible_range"] as? [String: String],
              range == ObservedReaderFixture.range(start, end) else { return nil }
        return try ObservedReaderFixture.encode(ObservedReaderFixture.copy(page))
    }
}

@main private enum AtlasObservedReaderSearchTests {
    @MainActor private static func wait(_ description: String, _ condition: @MainActor () -> Bool) async throws {
        for _ in 0..<4000 {
            if condition() { return }
            try await Task.sleep(for: .milliseconds(1))
        }
        preconditionFailure("Timed out: " + description)
    }
    private static func rejects(_ label: String, _ operation: () throws -> Void) {
        do { try operation(); preconditionFailure("Accepted invalid search receipt: " + label) }
        catch { }
    }
    private static func wireTests() throws {
        let input = try AtlasObservedReaderInput(root: "/repo", relativePath: Array("large.txt".utf8))
        let owner = UInt64.max - 7
        let request = try AtlasObservedReaderSearchRequest(generation: UInt64.max - 8, needle: "old", encoding: .utf8)
        for needle in ["", "bad\0query", String(repeating: "é", count: 2049)] {
            rejects("needle bounds") { _ = try AtlasObservedReaderSearchRequest(generation: 1, needle: needle, encoding: .utf8) }
        }
        for generation in [UInt64(0)] {
            rejects("zero generation") { _ = try AtlasObservedReaderSearchRequest(generation: generation, needle: "x", encoding: .utf8) }
        }
        for bytes in [UInt64(0), (1 << 32) + 1] {
            rejects("scan bounds") { _ = try AtlasObservedReaderSearchRequest(generation: 1, needle: "x", encoding: .utf8, maximumScanBytes: bytes) }
        }
        for matches in [UInt64(0), 257] {
            rejects("match bounds") { _ = try AtlasObservedReaderSearchRequest(generation: 1, needle: "x", encoding: .utf8, maximumMatches: matches) }
        }
        let composed = try AtlasObservedReaderSearchRequest(generation: 1, needle: "é", encoding: .utf8)
        let decomposed = try AtlasObservedReaderSearchRequest(generation: 1, needle: "e\u{301}", encoding: .utf8)
        precondition(composed != decomposed, "Exact query equality must not normalize spelling")

        let base = ObservedSearchFixture.report(owner, input, request)
        func decode(_ value: [String: Any]) throws -> AtlasObservedReaderSearchReport {
            try .decode(ObservedReaderFixture.encode(value), handle: owner, input: input, request: request)
        }
        let report = try decode(base)
        precondition(report.request.generation == UInt64.max - 8 && report.identity.owner == owner)
        precondition(report.searchComplete && report.hits.count == 1 && report.originalHex == "6f6c64")
        let changes: [(String, Any)] = [
            ("owner", "1"), ("file_id", "2"), ("command", "window"), ("schema", "fcb.observed-reader/2"),
            ("source_revision", "2"), ("capture_sha256", String(repeating: "a", count: 64)),
            ("whole_file_immutable", true), ("native_presented", true), ("query_generation", "01"),
            ("query_generation", 1), ("search_observation", "0"), ("encoding", "utf16le"),
            ("encoding_basis", "autodetected"), ("retention", "complete-file"), ("state", "pending"),
            ("mode", "regex"), ("case_sensitive", false), ("literal_text", "OLD"),
            ("input_complete", false), ("search_complete", false), ("consistency", "atomic"),
            ("final_observed_length", "65"), ("max_matches", "255"), ("max_scan_bytes", "8"),
            ("max_read_calls", "4096"), ("payload_bytes_read", "65"), ("scanned_input_bytes", "65"),
            ("read_calls", "0"), ("peak_input_buffer_bytes", "65"), ("additional_source_bytes_read", "0"),
            ("additional_read_calls", "0"), ("matches_seen", "2"), ("retained_hits", "2"),
            ("truncated", true), ("unsupported_at", "0"), ("literal_original_hex", "4f4c44"),
            ("literal_sha256", String(repeating: "B", count: 64)),
        ]
        for (key, value) in changes {
            var bad = base; bad[key] = value
            rejects(key) { _ = try decode(bad) }
        }
        let badRows: [[String: Any]] = [
            ["index": "1", "occurrence_id": "1", "original_range": ObservedReaderFixture.range(4, 7)],
            ["index": "0", "occurrence_id": "2", "original_range": ObservedReaderFixture.range(4, 7)],
            ["index": "0", "occurrence_id": "1", "original_range": ObservedReaderFixture.range(63, 66)],
            ["index": "0", "occurrence_id": "1", "original_range": ObservedReaderFixture.range(4, 8)],
        ]
        for badRow in badRows {
            var bad = base; bad["hits"] = [badRow]
            rejects("row range/ordinal") { _ = try decode(bad) }
        }
        let target = report.target(for: report.hits[0])
        let witnessJSON = ObservedSearchFixture.witness(base)
        let witness = try AtlasObservedReaderSearchWitness.decode(ObservedReaderFixture.encode(witnessJSON),
            handle: owner, input: input, report: report, target: target)
        precondition(witness.originalHex == request.originalHex && witness.text == "old")
        for (key, value) in [("query_generation", "2"), ("search_observation", "3"), ("index", "1"),
            ("occurrence_id", "2"), ("original_hex", "6e6577"), ("text", "new"),
            ("witness_sha256", String(repeating: "c", count: 64)), ("additional_source_bytes_read", "1"),
            ("additional_read_calls", "1")] {
            var bad = witnessJSON; bad[key] = value
            rejects("witness " + key) {
                _ = try AtlasObservedReaderSearchWitness.decode(ObservedReaderFixture.encode(bad),
                    handle: owner, input: input, report: report, target: target)
            }
        }

        for encoding in AtlasObservedReaderEncoding.allCases {
            let unicode = try AtlasObservedReaderSearchRequest(generation: 7, needle: "é😀\u{feff}", encoding: encoding)
            let json = ObservedSearchFixture.report(owner, input, unicode)
            let decoded = try AtlasObservedReaderSearchReport.decode(ObservedReaderFixture.encode(json),
                handle: owner, input: input, request: unicode)
            precondition(decoded.originalHex == unicode.originalHex && decoded.hits[0].range.start == 4)
        }
        let capped = try AtlasObservedReaderSearchRequest(generation: 8, needle: "old", encoding: .utf8, maximumScanBytes: 8)
        let partial = ObservedSearchFixture.report(owner, input, capped, length: 64, offsets: [0], state: "byte-limit")
        let limited = try AtlasObservedReaderSearchReport.decode(ObservedReaderFixture.encode(partial),
            handle: owner, input: input, request: capped)
        precondition(!limited.searchComplete && !limited.inputComplete && limited.countLabel.contains("partial"))
        for consistency in ["changed-during-read", "metadata-unavailable"] {
            var changed = base; changed["consistency"] = consistency; changed["search_complete"] = false
            let accepted = try decode(changed)
            precondition(accepted.inputComplete && !accepted.searchComplete && accepted.finalObservedLength == 64)
        }
        let empty = try decode(ObservedSearchFixture.report(owner, input, request, offsets: []))
        precondition(empty.hits.isEmpty && empty.originalHex == nil && empty.searchComplete)
        var unsupported = ObservedSearchFixture.report(owner, input, request, offsets: [], state: "unsupported-text")
        unsupported["unsupported_at"] = "5"
        let unsupportedReport = try decode(unsupported)
        precondition(unsupportedReport.hits.isEmpty)
        let one = try AtlasObservedReaderSearchRequest(generation: 8, needle: "old", encoding: .utf8, maximumMatches: 1)
        let truncated = try AtlasObservedReaderSearchReport.decode(ObservedReaderFixture.encode(
            ObservedSearchFixture.report(owner, input, one, offsets: [0], state: "match-limit")),
            handle: owner, input: input, request: one)
        precondition(truncated.matchesSeen == 2 && truncated.countLabel.contains("At least 2"))

        let atZero = ObservedSearchFixture.report(owner, input, request, length: 4, offsets: [0])
        let zeroReport = try decode(atZero), zeroTarget = zeroReport.target(for: zeroReport.hits[0])
        let zeroWitness = try AtlasObservedReaderSearchWitness.decode(ObservedReaderFixture.encode(ObservedSearchFixture.witness(atZero)),
            handle: owner, input: input, report: zeroReport, target: zeroTarget)
        let pageRequest = try AtlasObservedReaderRequest(offset: 0, bytes: 4)
        func context(_ text: String, observation: UInt64 = 3, replacement: Bool = false) throws -> AtlasObservedReaderPage {
            var json = ObservedReaderFixture.page(owner, input, observation, pageRequest, length: 4, text: text)
            json["has_replacements"] = replacement
            return try .decode(ObservedReaderFixture.encode(json), handle: owner, input: input, request: pageRequest)
        }
        let verifiedContext = try zeroWitness.verify(context: context("old\n"))
        let changedContext = try zeroWitness.verify(context: context("new\n"))
        let staleContext = try zeroWitness.verify(context: context("old\n", observation: 2))
        let replacementContext = try zeroWitness.verify(context: context("old\n", replacement: true))
        precondition(verifiedContext == .verified(selection: NSRange(location: 0, length: 3)))
        precondition(changedContext == .changed)
        precondition(staleContext == .unavailable)
        precondition(replacementContext == .verified(selection: nil))

        let verifiedRequest = try AtlasObservedReaderRequest(offset: 0, bytes: 16)
        var verifiedJSON = ObservedReaderFixture.page(owner, input, 3, verifiedRequest, length: 4, text: "old\n")
        verifiedJSON["command"] = "observe-hit"
        let receipt: [String: Any] = ["query_generation": String(request.generation), "search_observation": "2",
            "hit_index": "0", "occurrence_id": "1", "original_range": ObservedReaderFixture.range(0, 3),
            "decoded_utf8_range": ObservedReaderFixture.range(0, 3), "witness_verified": true,
            "verification": "equal-original-bytes-in-new-observation"]
        verifiedJSON["find_hit"] = receipt
        func decodeVerifiedPage(_ value: [String: Any]) throws -> AtlasObservedReaderPage {
            try .decode(ObservedReaderFixture.encode(value), handle: owner, input: input,
                request: verifiedRequest, hit: zeroWitness)
        }
        let verifiedPage = try decodeVerifiedPage(verifiedJSON)
        precondition(verifiedPage.verifiedSearchSelection == NSRange(location: 0, length: 3))
        for (key, value) in [("query_generation", "1" as Any), ("search_observation", "1" as Any),
            ("hit_index", "1" as Any), ("decoded_utf8_range", ObservedReaderFixture.range(0, 4) as Any),
            ("witness_verified", false as Any)] {
            var forged = verifiedJSON, changed = receipt
            changed[key] = value; forged["find_hit"] = changed
            rejects("verified hit " + key) { _ = try decodeVerifiedPage(forged) }
        }
        var ordinary = verifiedJSON; ordinary["command"] = "window"
        rejects("ordinary window cannot claim a verified hit") {
            _ = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(ordinary),
                handle: owner, input: input, request: verifiedRequest)
        }
        let retainedVerified = ObservedReaderFixture.retained(verifiedJSON)
        let replay = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(retainedVerified),
            handle: owner, input: input, retained: verifiedPage)
        precondition(replay.matches(verifiedPage))
        var alteredReplay = retainedVerified, alteredReceipt = receipt
        alteredReceipt["query_generation"] = "1"; alteredReplay["find_hit"] = alteredReceipt
        rejects("retained hit provenance changed") {
            _ = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(alteredReplay),
                handle: owner, input: input, retained: verifiedPage)
        }
    }

    @MainActor private static func modelTests() async throws {
        let input = try AtlasObservedReaderInput(root: "/repo", relativePath: Array("large.txt".utf8))
        let probe = ObservedSearchProbe(), model = AtlasObservedReaderModel(transport: probe.transport)
        model.pageBytes = 4
        model.open(.init(input: input, project: UUID(), access: nil))
        try await wait("initial page") { model.page != nil && !model.busy }
        model.searchText = "old"; model.find()
        try await wait("first search") { model.searchReport != nil && !model.busy }
        let first = model.searchReport!, firstTarget = first.target(for: first.hits[0])
        precondition(first.observation > model.page!.observation.id && probe.windowCount == 1)
        model.selectSearchHit(firstTarget)
        try await wait("retained literal") { model.selectedSearchWitness != nil && !model.busy }
        precondition(probe.windowCount == 1 && model.selectedSearchWitness?.text == "old")
        let pageID = model.page!.observation.id
        probe.failFind(); model.find()
        try await wait("failed replacement") { !model.busy }
        precondition(model.searchReport?.request.generation == first.request.generation)
        precondition(model.selectedSearchWitness?.target == firstTarget && model.page?.observation.id == pageID)
        precondition(!model.canSelectSearchHit && !model.canReadSearchContext,
            "A failed replacement cannot prove the host still owns the visible query")
        model.readSearchContext()
        precondition(probe.windowCount == 1 && model.searchCoverage?.contains("read-only") == true)
        model.find()
        try await wait("accepted replacement restores authority") { model.searchAuthorized && !model.busy }
        let refreshed = model.searchReport!
        model.selectSearchHit(refreshed.target(for: refreshed.hits[0]))
        try await wait("refreshed retained literal") { model.selectedSearchWitness != nil && !model.busy }

        model.readSearchContext()
        try await wait("new context") { !model.busy }
        precondition(model.searchSelection == NSRange(location: 0, length: 3))
        precondition(model.page!.observation.id > first.observation)
        probe.setContent("new\n"); model.readSearchContext()
        try await wait("changed context") { !model.busy }
        precondition(model.searchContext == .changed && model.searchSelection == nil)
        precondition(model.page?.text == "new\n" && model.selectedSearchWitness?.text == "old")

        let retainedID = model.page!.observation.id
        probe.duplicateWindowObservation(); model.readSearchContext()
        try await wait("stale context observation") { !model.busy }
        precondition(model.page?.observation.id == retainedID && model.searchSelection == nil)
        let oldCalls = probe.hitCount
        model.searchText = "new"
        precondition(model.searchReport == nil && model.selectedSearchWitness == nil && model.searchSelection == nil)
        model.selectSearchHit(firstTarget)
        precondition(probe.hitCount == oldCalls, "A stale row index cannot select in another query")

        let gate = ObservedReaderGate(); probe.blockFind(gate); model.find()
        try await wait("blocked obsolete find") { gate.entered }
        model.searchText = "different"
        precondition(!model.busy && model.searchReport == nil)
        gate.release()
        try await wait("obsolete find drain") { probe.activeCount == 0 }
        await Task.yield()
        precondition(model.searchReport == nil, "Late successful Find may not resurrect edited query targets")

        model.searchText = "new"; model.find()
        try await wait("new query") { model.searchReport != nil && !model.busy }
        let current = model.searchReport!, target = current.target(for: current.hits[0])
        let hitGate = ObservedReaderGate(); probe.blockHit(hitGate); model.selectSearchHit(target)
        try await wait("blocked witness") { hitGate.entered }
        model.encoding = .utf16le
        precondition(model.searchReport == nil && model.selectedSearchWitness == nil)
        hitGate.release()
        try await wait("witness drain") { probe.activeCount == 0 }
        await Task.yield()
        precondition(model.selectedSearchWitness == nil)

        model.encoding = .utf8; model.find()
        try await wait("restore searchable query") { model.searchReport != nil && !model.busy }
        let acceptedGeneration = model.searchReport!.request.generation
        let pageGate = ObservedReaderGate(); probe.blockFind(pageGate); model.find()
        try await wait("find before page replacement") { pageGate.entered }
        model.observe(offset: 0); await Task.yield(); pageGate.release()
        try await wait("replacement range") { !model.busy && probe.activeCount == 0 }
        precondition(model.searchReport?.request.generation == acceptedGeneration)
        precondition(model.page?.text == "new\n" && probe.maximumActive == 1)

        let queueGate = ObservedReaderGate(); probe.blockFind(queueGate); model.find()
        try await wait("bounded active find") { queueGate.entered }
        let before = probe.findGenerations.count
        model.find(); await Task.yield(); model.find(); await Task.yield(); queueGate.release()
        try await wait("latest queued find") { !model.busy && probe.activeCount == 0 }
        precondition(probe.findGenerations.count == before + 1 && probe.maximumActive == 1)

        let oldPage = model.page!.observation.id
        let beforeDuplicate = model.searchReport!.request.generation
        probe.duplicateFindObservation(); model.find()
        try await wait("duplicate accepted revision") { !model.busy }
        precondition(model.page?.observation.id == oldPage && model.searchReport?.request.generation == beforeDuplicate)
        let beforeReopen = model.info!.identity.owner
        probe.lifetime.failOpen(); model.reopenCurrentFile()
        precondition(model.searchReport == nil && model.selectedSearchWitness == nil)
        try await wait("failed reopen") { !model.busy }
        precondition(model.info?.identity.owner == beforeReopen && model.page?.observation.id == oldPage)
        model.reopenCurrentFile()
        try await wait("successful reopen") { !model.busy && model.info?.identity.owner != beforeReopen }
        precondition(model.searchReport == nil && model.page?.observation.id == 1)
        model.close()
        try await wait("owners retire") { probe.lifetime.closedCount == probe.lifetime.createCount }
    }

    @MainActor private static func cancellationLifetimeTests() async throws {
        let probe = ObservedSearchProbe(), model = AtlasObservedReaderModel(transport: probe.transport)
        let input = try AtlasObservedReaderInput(root: "/repo", relativePath: Array("large.txt".utf8))
        model.pageBytes = 4
        model.open(.init(input: input, project: UUID(), access: ObservedReaderGrant(probe.lifetime, "search")))
        try await wait("grant opening") { model.page != nil && !model.busy }
        let gate = ObservedReaderGate(); probe.blockFind(gate)
        model.searchText = "old"; model.find()
        try await wait("entered scan") { gate.entered }
        let accepted = model.page!.observation.id
        model.cancel()
        precondition(!model.busy && model.page?.observation.id == accepted && model.searchReport == nil)
        model.close()
        precondition(!probe.lifetime.hasReleased("search") && probe.lifetime.closedCount == 0,
            "Closing UI cannot release the grant while the entered scan still uses its descriptor")
        gate.release()
        try await wait("grant scan drain") { probe.lifetime.hasReleased("search") && probe.lifetime.closedCount == 1 }
        precondition(model.closed && model.searchReport == nil && model.page == nil)
        model.open(.init(input: input, project: UUID(), access: nil))
        try await wait("reopen after canceled scan") { model.page != nil && !model.busy }
        precondition(model.searchReport == nil)
        model.close()
        try await wait("last owner retires") { probe.lifetime.closedCount == 2 }
    }

    @MainActor private static func verifiedNavigationTests() async throws {
        let input = try AtlasObservedReaderInput(root: "/repo", relativePath: Array("large.txt".utf8))
        let probe = ObservedSearchProbe(verifiedNavigation: true)
        probe.setContent("old\nterm\nend\n")
        let model = AtlasObservedReaderModel(transport: probe.transport)
        model.pageBytes = 4
        model.open(.init(input: input, project: UUID(), access: nil))
        try await wait("verified navigation initial page") { model.page != nil && !model.busy }
        let initial = model.page!
        model.searchText = "term"; model.find()
        try await wait("verified navigation query") { model.searchReport != nil && !model.busy }
        model.nextSearchHit()
        try await wait("verified navigation witness") { model.selectedSearchWitness != nil && !model.busy }
        precondition(model.canGoToVerifiedMatch)
        model.goToVerifiedMatch()
        try await wait("verified new context") { !model.busy }
        let accepted = model.page!
        precondition(accepted.findHit != nil && accepted.observation.id > model.searchReport!.observation
            && model.searchSelection == NSRange(location: 4, length: 4) && probe.verifiedJumpCount == 1)
        let windows = probe.windowCount
        model.back(); try await wait("verified context back") { !model.busy }
        precondition(model.page!.matches(initial) && model.searchSelection == nil)
        model.forward(); try await wait("verified context forward") { !model.busy }
        precondition(model.page!.matches(accepted) && model.searchSelection == NSRange(location: 4, length: 4)
            && probe.windowCount == windows && probe.verifiedJumpCount == 1,
            "History restores the accepted verified selection without a new source read")

        probe.failVerifiedJump(); model.goToVerifiedMatch()
        try await wait("failed verified jump") { !model.busy }
        precondition(model.page!.matches(accepted) && model.searchSelection == NSRange(location: 4, length: 4)
            && model.notice.contains("changed or disappeared"))
        probe.setContent("old\nxxxx\nend\n"); model.goToVerifiedMatch()
        try await wait("changed verified bytes") { !model.busy }
        precondition(model.page!.matches(accepted) && model.selectedSearchWitness?.text == "term",
            "Changed live bytes cannot replace accepted history or the old retained literal")

        // Ordinary current-context reading intentionally remains independent:
        // it may display the new bytes while disclosing the changed match.
        model.readSearchContext(); try await wait("explicit changed context") { !model.busy }
        precondition(model.page?.text == "old\nxxxx\nend\n" && model.searchContext == .changed)
        model.back(); try await wait("recover verified history") { !model.busy }
        precondition(model.page!.matches(accepted) && model.searchSelection == NSRange(location: 4, length: 4))

        probe.setContent("old\nterm\nend\n"); probe.useMalformedNeighbor(); model.goToVerifiedMatch()
        try await wait("verified match beside malformed bytes") { !model.busy }
        precondition(model.page?.hasReplacements == true && model.page?.text.hasPrefix("\u{fffd}") == true
            && model.page?.findHit?.decodedRange.start == 6
            && model.searchSelection == NSRange(location: 4, length: 4),
            "An exact engine receipt selects the unaffected match despite malformed neighboring bytes")
        let earlierPage = model.page!
        model.searchText = "end"; model.find()
        try await wait("another query before retained history") { model.searchReport != nil && !model.busy }
        model.nextSearchHit(); try await wait("another query witness") { model.selectedSearchWitness != nil && !model.busy }
        model.goToVerifiedMatch(); try await wait("another query verified page") { !model.busy }
        precondition(model.searchSelection == NSRange(location: 9, length: 3) && model.searchContext != nil)
        model.back(); try await wait("earlier query verified page") { !model.busy }
        precondition(model.page!.matches(earlierPage) && model.searchSelection == NSRange(location: 4, length: 4)
            && model.selectedSearchWitness?.text == "end" && model.searchContext == nil
            && model.verifiedPageNotice?.contains("earlier Find") == true,
            "Historical verified selection retains its provenance without claiming the current query's context")
        model.close(); try await wait("verified owner retirement") { probe.lifetime.closedCount == probe.lifetime.createCount }
    }

    @MainActor private static func canceledReplacementAuthorityTests() async throws {
        let input = try AtlasObservedReaderInput(root: "/repo", relativePath: Array("large.txt".utf8))
        let probe = ObservedSearchProbe(verifiedNavigation: true)
        let model = AtlasObservedReaderModel(transport: probe.transport)
        model.pageBytes = 4
        model.open(.init(input: input, project: UUID(), access: nil))
        try await wait("replacement authority opening") { model.page != nil && !model.busy }
        model.searchText = "old"; model.find()
        try await wait("replacement authority query") { model.searchReport != nil && !model.busy }
        model.nextSearchHit(); try await wait("replacement authority witness") { model.selectedSearchWitness != nil && !model.busy }
        model.goToVerifiedMatch(); try await wait("replacement authority page") { !model.busy }
        let accepted = model.searchReport!, target = accepted.target(for: accepted.hits[0]), page = model.page!
        let gate = ObservedReaderGate(); probe.blockFindReply(gate); model.find()
        try await wait("host committed replacement before delivery") { gate.entered }
        model.cancel()
        precondition(!model.busy && !model.searchAuthorized && !model.canSelectSearchHit
            && !model.canReadSearchContext && !model.canGoToVerifiedMatch
            && model.searchReport?.request == accepted.request && model.page!.matches(page)
            && model.selectedSearchWitness?.text == "old")
        let hits = probe.hitCount, windows = probe.windowCount, verified = probe.verifiedJumpCount
        model.selectSearchHit(target); model.nextSearchHit(); model.previousSearchHit()
        model.readSearchContext(); model.goToVerifiedMatch()
        precondition(probe.hitCount == hits && probe.windowCount == windows && probe.verifiedJumpCount == verified,
            "A canceled reply cannot revive old-query hit or context actions after host replacement")
        gate.release()
        model.back(); try await wait("history after canceled replacement") { !model.busy }
        model.forward(); try await wait("verified history after canceled replacement") { !model.busy }
        precondition(model.page!.matches(page) && model.searchSelection == NSRange(location: 0, length: 3)
            && model.searchReport?.request == accepted.request && !model.searchAuthorized)
        model.find(); try await wait("new accepted receipt restores actions") { model.searchAuthorized && !model.busy }
        precondition(model.searchReport?.request.generation == accepted.request.generation + 2 && model.canSelectSearchHit)
        model.close(); try await wait("replacement authority retirement") { probe.lifetime.closedCount == probe.lifetime.createCount }
    }

    @MainActor static func main() async throws {
        try wireTests()
        try await modelTests()
        try await cancellationLifetimeTests()
        try await verifiedNavigationTests()
        try await canceledReplacementAuthorityTests()
        print("Observed-reader exact Find wire, retained literal, fresh context, stale delivery, queue and grant lifetime fixtures passed.")
    }
}
