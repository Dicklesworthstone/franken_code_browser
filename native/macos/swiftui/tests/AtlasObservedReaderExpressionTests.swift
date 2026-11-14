import Foundation

/// Fixed engine receipts exercise the native admission and scheduling boundary.
/// This suite does not parse expressions, search source text, hash witnesses, or
/// qualify the Rust scanner, filesystem policy, or native text rendering.
private enum ObservedExpressionWire {
    static let query = "need +present -forbidden"
    static let digest = String(repeating: "b", count: 64)

    static func evidence(_ query: String, primary: String, qualified: Bool?,
                         scope: String = "same-observed-stream") -> [String: Any] {
        var out: [String: Any] = [
            "requested_mode": "expression", "realized_mode": "decoded-text-expression",
            "expression": query, "primary_needle": primary, "query_syntax": "fcb-bounded-expression/1",
            "predicate_scope": scope, "expression_qualified": qualified.map { $0 as Any } ?? NSNull()
        ]
        if scope == "original-search-observation" { out["expression_revalidated"] = false }
        return out
    }

    static func report(_ owner: UInt64, _ input: AtlasObservedReaderInput,
                       _ request: AtlasObservedReaderSearchRequest, observation: UInt64 = 2,
                       primary: String = "need", offsets: [UInt64] = [4], length: UInt64 = 64,
                       qualified: Bool? = true, state: String = "complete-observed-input",
                       total: UInt64? = nil, spellings: [String]? = nil) -> [String: Any] {
        var out = ObservedReaderFixture.common(owner, input, "find")
        out.merge(evidence(request.needle, primary: primary, qualified: qualified)) { _, next in next }
        let bytes = min(length, request.maximumScanBytes)
        let complete = state == "complete-observed-input"
        let seen = total ?? (qualified == true ? request.pageStartAfter + UInt64(offsets.count) : 0)
        let skipped = min(request.pageStartAfter, seen)
        let texts = spellings ?? Array(repeating: primary, count: offsets.count)
        precondition(texts.count == offsets.count)
        let original = AtlasObservedReaderSearchRequest.originalBytes(primary, encoding: request.encoding)
        out["query_generation"] = String(request.generation); out["search_observation"] = String(observation)
        out["needle"] = request.needle; out["encoding"] = request.encoding.rawValue
        out["encoding_basis"] = "explicit-for-this-search"; out["mode"] = request.wireMode
        out["case_sensitive"] = request.caseSensitive; out["retention"] = "literal-witnesses-only"
        out["literal_text"] = primary
        out["literal_original_hex"] = offsets.isEmpty || !request.caseSensitive
            ? NSNull() : ObservedReaderFixture.hex(original) as Any
        out["literal_sha256"] = offsets.isEmpty || !request.caseSensitive ? NSNull() : digest as Any
        let retainedBytes = request.caseSensitive ? (offsets.isEmpty ? 0 : original.count)
            : texts.reduce(0) { $0 + AtlasObservedReaderSearchRequest.originalBytes($1, encoding: request.encoding).count }
        out["retained_witness_bytes"] = String(retainedBytes)
        out["observed_length"] = String(length); out["final_observed_length"] = String(length)
        out["state"] = state; out["consistency"] = "unchanged-metadata-not-atomic"
        out["input_complete"] = complete; out["search_complete"] = complete
        out["count_all_matches"] = request.countAllMatches; out["match_count_complete"] = complete
        out["max_matches"] = String(request.maximumMatches); out["max_scan_bytes"] = String(request.maximumScanBytes)
        out["max_read_calls"] = String(AtlasObservedReaderSearchLimits.readCalls)
        out["payload_bytes_read"] = String(bytes); out["scanned_input_bytes"] = String(bytes)
        out["read_calls"] = bytes == 0 ? "0" : "1"
        out["peak_input_buffer_bytes"] = String(min(bytes, AtlasObservedReaderSearchLimits.inputBufferBytes))
        out["matches_seen"] = String(seen); out["retained_hits"] = String(offsets.count)
        out["truncated"] = seen > UInt64(offsets.count); out["unsupported_at"] = NSNull()
        out["page_start_after"] = String(request.pageStartAfter); out["skipped_matches"] = String(skipped)
        out["page_has_more_matches"] = seen - skipped > UInt64(offsets.count)
        out["additional_source_bytes_read"] = String(bytes); out["additional_read_calls"] = out["read_calls"]
        out["hits"] = offsets.enumerated().map { index, start in
            let encoded = AtlasObservedReaderSearchRequest.originalBytes(texts[index], encoding: request.encoding)
            var row: [String: Any] = [
                "index": String(index), "occurrence_id": String(request.pageStartAfter + UInt64(index) + 1),
                "original_range": ObservedReaderFixture.range(start, start + UInt64(encoded.count)),
                "fixture_spelling": texts[index]
            ]
            if !request.caseSensitive { row["witness_sha256"] = digest }
            return row
        }
        return out
    }

    static func witness(_ report: [String: Any], index: Int = 0) -> [String: Any] {
        var out = report
        out.removeValue(forKey: "needle")
        let row = (report["hits"] as! [[String: Any]])[index]
        let spelling = row["fixture_spelling"] as! String
        let encoding = AtlasObservedReaderEncoding(rawValue: report["encoding"] as! String)!
        out["command"] = "search-hit"
        for (key, value) in row { out[key] = value }
        out["text"] = spelling
        out["original_hex"] = ObservedReaderFixture.hex(
            AtlasObservedReaderSearchRequest.originalBytes(spelling, encoding: encoding))
        out["witness_sha256"] = digest
        out["additional_source_bytes_read"] = "0"; out["additional_read_calls"] = "0"
        return out
    }

    static func context(_ owner: UInt64, _ input: AtlasObservedReaderInput,
                        _ witness: AtlasObservedReaderSearchWitness, observation: UInt64,
                        request: AtlasObservedReaderRequest) -> [String: Any] {
        let expression = witness.expression!
        precondition(request.offset <= witness.target.hit.range.start)
        // New context deliberately contains the excluded term. Only equality
        // of the selected primary bytes is established by this fresh receipt.
        let prefix = String(repeating: " ", count: Int(witness.target.hit.range.start - request.offset))
        let text = prefix + witness.text + " forbidden\n"
        let original = AtlasObservedReaderSearchRequest.originalBytes(text, encoding: request.encoding)
        var out = ObservedReaderFixture.page(owner, input, observation, request,
            length: request.offset + UInt64(original.count), text: text,
            originalHex: ObservedReaderFixture.hex(original))
        out["command"] = "observe-hit"
        var hit = evidence(expression.query, primary: expression.primaryNeedle,
            qualified: true, scope: "original-search-observation")
        hit.merge([
            "query_generation": String(witness.target.generation),
            "search_observation": String(witness.target.observation),
            "hit_index": String(witness.target.hit.index), "occurrence_id": String(witness.target.hit.occurrence),
            "page_start_after": String(witness.target.hit.occurrence - witness.target.hit.index - 1),
            "original_range": ObservedReaderFixture.range(witness.target.hit.range.start, witness.target.hit.range.end),
            "decoded_utf8_range": ObservedReaderFixture.range(UInt64(prefix.utf8.count),
                UInt64(prefix.utf8.count + witness.text.utf8.count)),
            "witness_verified": true, "verification": "equal-original-bytes-in-new-observation",
            "mode": "decoded-text-expression", "case_sensitive": witness.caseSensitive,
            "token_boundaries_verified": false
        ]) { _, next in next }
        out["find_hit"] = hit
        return out
    }
}

/// A bounded collection of predetermined results supplies 256 rows, then four
/// later rows. No fixture matcher or expression evaluator is involved.
private final class ObservedExpressionProbe: @unchecked Sendable {
    let lifetime = ObservedReaderProbe()
    private let lock = NSLock()
    private var inputs: [UInt64: AtlasObservedReaderInput] = [:]
    private var reports: [UInt64: [String: Any]] = [:]
    private var contexts: [UInt64: [UInt64: [String: Any]]] = [:]
    private var requests: [AtlasObservedReaderSearchRequest] = []
    private var nextObservation: UInt64 = 100
    private var nextGate: ObservedReaderGate?
    private var active = 0, peak = 0, hits = 0
    private func locked<T>(_ body: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return body() }
    var findRequests: [AtlasObservedReaderSearchRequest] { locked { requests } }
    var activeCount: Int { locked { active } }
    var peakCount: Int { locked { peak } }
    var hitCount: Int { locked { hits } }
    func blockNextReply(_ gate: ObservedReaderGate) { locked { nextGate = gate } }

    var transport: AtlasObservedReaderTransport {
        let base = lifetime.transport
        return .init(create: base.create, open: { owner, input, flag in
            let reply = try base.open(owner, input, flag)
            self.locked { self.inputs[owner] = input }
            return reply
        }, info: base.info, window: base.window, retained: { owner, id in
            if let page = self.locked({ self.contexts[owner]?[id] }) {
                return try ObservedReaderFixture.encode(ObservedReaderFixture.retained(page))
            }
            return try base.retained(owner, id)
        }, copy: base.copy, cancel: base.cancel, close: base.close, retirementFailed: base.retirementFailed,
        search: .init(find: { owner, request, _ in try self.find(owner, request) },
            hit: { owner, generation, index in try self.hit(owner, generation, index) },
            observeHit: { owner, witness, request, _ in try self.context(owner, witness, request) },
            findPage: { owner, request, _ in try self.find(owner, request) }))
    }

    private func find(_ owner: UInt64, _ request: AtlasObservedReaderSearchRequest) throws -> String? {
        lock.lock()
        guard let input = inputs[owner] else { lock.unlock(); return nil }
        precondition(request.mode == .expression)
        nextObservation += 1; let id = nextObservation, gate = nextGate
        nextGate = nil; requests.append(request); active += 1; peak = max(peak, active)
        lock.unlock()
        defer { locked { active -= 1 } }
        let skipped = min(request.pageStartAfter, 260)
        let count = min(request.maximumMatches, 260 - skipped)
        let offsets = (0..<Int(count)).map { (skipped + UInt64($0)) * 8 }
        let raw = ObservedExpressionWire.report(owner, input, request, observation: id,
            offsets: offsets, length: 4096, total: 260,
            spellings: Array(repeating: request.caseSensitive ? "need" : "NEED", count: Int(count)))
        locked { reports[owner] = raw }
        gate?.wait()
        return try ObservedReaderFixture.encode(raw)
    }

    private func hit(_ owner: UInt64, _ generation: UInt64, _ index: UInt64) throws -> String? {
        let raw: [String: Any]? = locked {
            hits += 1
            guard let raw = reports[owner], raw["query_generation"] as? String == String(generation),
                  index < UInt64((raw["hits"] as! [[String: Any]]).count) else { return nil }
            return raw
        }
        guard let raw else { return nil }
        return try ObservedReaderFixture.encode(ObservedExpressionWire.witness(raw, index: Int(index)))
    }

    private func context(_ owner: UInt64, _ witness: AtlasObservedReaderSearchWitness,
                         _ request: AtlasObservedReaderRequest) throws -> String? {
        let state: (AtlasObservedReaderInput, UInt64)? = locked {
            guard let input = inputs[owner], reports[owner]?["query_generation"] as? String == String(witness.target.generation)
                else { return nil }
            nextObservation += 1
            return (input, nextObservation)
        }
        guard let (input, id) = state else { return nil }
        let page = ObservedExpressionWire.context(owner, input, witness, observation: id, request: request)
        locked { contexts[owner, default: [:]][id] = page }
        return try ObservedReaderFixture.encode(page)
    }
}

@main private enum AtlasObservedReaderExpressionTests {
    private static let owner: UInt64 = UInt64.max - 31
    private static func input() throws -> AtlasObservedReaderInput {
        try .init(root: "/repo", relativePath: Array("large.txt".utf8))
    }
    private static func request(generation: UInt64 = 7, query: String = ObservedExpressionWire.query,
                                encoding: AtlasObservedReaderEncoding = .utf8,
                                caseSensitive: Bool = true, scan: UInt64 = 4096,
                                limit: UInt64 = 256, offset: UInt64 = 0,
                                countAll: Bool = true) throws -> AtlasObservedReaderSearchRequest {
        try .init(generation: generation, needle: query, encoding: encoding,
            maximumMatches: limit, maximumScanBytes: scan, countAllMatches: countAll,
            caseSensitive: caseSensitive, mode: .expression, pageStartAfter: offset)
    }
    private static func decode(_ raw: [String: Any], _ request: AtlasObservedReaderSearchRequest) throws
        -> AtlasObservedReaderSearchReport {
        try .decode(ObservedReaderFixture.encode(raw), handle: owner, input: input(), request: request)
    }
    private static func rejects(_ label: String, _ body: () throws -> Void) {
        do { try body(); preconditionFailure("Accepted invalid expression receipt: " + label) }
        catch {}
    }
    @MainActor private static func wait(_ description: String, _ condition: @MainActor () -> Bool) async throws {
        for _ in 0..<4000 {
            if condition() { return }
            try await Task.sleep(for: .milliseconds(1))
        }
        preconditionFailure("Timed out: " + description)
    }

    private static func admissionAndExactIdentity() throws {
        let q = "need   +\"e\u{301}\" -forbidden"
        let r = try request(query: q, caseSensitive: false, countAll: false)
        precondition(r.mode.abi == 2 && r.wireMode == "decoded-text-expression")
        precondition(r.needle.utf8.elementsEqual(q.utf8) && !r.caseSensitive && !r.countAllMatches)
        _ = try request(query: String(repeating: "é", count: 512))
        for bad in ["", "need\0excluded", String(repeating: "é", count: 513)] {
            rejects("expression byte admission") { _ = try request(query: bad) }
        }
        _ = try request(query: "need +")
        let composed = try request(query: "é +present"), decomposed = try request(query: "e\u{301} +present")
        precondition(composed != decomposed,
            "Native admission preserves raw spelling and leaves syntax interpretation to Rust")
        let a = AtlasObservedReaderExpression(query: "é +present", primaryNeedle: "é", qualified: true)
        let b = AtlasObservedReaderExpression(query: "e\u{301} +present", primaryNeedle: "e\u{301}", qualified: true)
        precondition(a != b)
    }

    private static func qualificationAndAtomicEvidence() throws {
        let r = try request()
        let raw = ObservedExpressionWire.report(owner, try input(), r, offsets: [4, 20])
        let report = try decode(raw, r)
        precondition(report.primaryNeedle == "need" && report.expression?.query == r.needle)
        precondition(report.expression?.qualified == true && report.matchesSeen == 2 && report.matchCountComplete)
        precondition(report.hits[0].range.count == 4 && report.retainedWitnessBytes == 4)
        for key in ["needle", "requested_mode", "realized_mode", "expression", "primary_needle",
                    "query_syntax", "predicate_scope", "expression_qualified"] {
            var bad = raw; bad.removeValue(forKey: key)
            rejects("missing " + key) { _ = try decode(bad, r) }
        }
        for (key, value) in [
            ("needle", "need +different"), ("expression", "need +different"),
            ("primary_needle", "different"), ("literal_text", r.needle),
            ("query_syntax", "foreign-expression/1"), ("predicate_scope", "original-search-observation"),
            ("requested_mode", "text"), ("realized_mode", "exact-decoded-text")
        ] {
            var bad = raw; bad[key] = value
            rejects("mismatched " + key) { _ = try decode(bad, r) }
        }
        let invalidQualifications: [Any] = [false, NSNull(), "true"]
        for value in invalidQualifications {
            var bad = raw; bad["expression_qualified"] = value
            rejects("candidate hits without qualification") { _ = try decode(bad, r) }
        }
        var revalidated = raw; revalidated["expression_revalidated"] = false
        rejects("navigation-only evidence in scan") { _ = try decode(revalidated, r) }

        let noMatch = ObservedExpressionWire.report(owner, try input(), r, offsets: [], qualified: false)
        let empty = try decode(noMatch, r)
        precondition(empty.searchComplete && empty.matchesSeen == 0 && empty.hits.isEmpty)
        precondition(empty.expression?.qualified == false && !empty.countLabel.contains("not yet"))
        let partialRequest = try request(scan: 32)
        let pending = ObservedExpressionWire.report(owner, try input(), partialRequest,
            offsets: [], length: 64, qualified: nil, state: "byte-limit")
        let partial = try decode(pending, partialRequest)
        precondition(!partial.searchComplete && partial.expression?.qualified == nil && partial.hits.isEmpty)
        precondition(partial.countLabel.contains("not yet"))
        var forged = pending; forged["expression_qualified"] = false
        rejects("early negative decision") { _ = try decode(forged, partialRequest) }

        let noCounting = try request(limit: 1, countAll: false)
        let allObserved = ObservedExpressionWire.report(owner, try input(), noCounting, offsets: [4], total: 3)
        let bounded = try decode(allObserved, noCounting)
        precondition(bounded.matchesSeen == 3 && bounded.hits.count == 1 && bounded.pageHasMoreMatches)
        var early = allObserved
        early["state"] = "match-limit"; early["input_complete"] = false
        early["search_complete"] = false; early["match_count_complete"] = false
        rejects("primary-hit cap cannot establish predicates") { _ = try decode(early, noCounting) }
    }

    private static func witnessesKeepPrimarySpellingAndEncoding() throws {
        for encoding in [AtlasObservedReaderEncoding.utf8, .utf16le, .utf16be] {
            for sensitive in [true, false] {
                let r = try request(encoding: encoding, caseSensitive: sensitive)
                let spelling = sensitive ? "need" : "NEED"
                let raw = ObservedExpressionWire.report(owner, try input(), r, spellings: [spelling])
                let report = try decode(raw, r), target = report.target(for: report.hits[0])
                let rawWitness = ObservedExpressionWire.witness(raw)
                let witness = try AtlasObservedReaderSearchWitness.decode(ObservedReaderFixture.encode(rawWitness),
                    handle: owner, input: input(), report: report, target: target)
                precondition(witness.text == spelling && witness.mode == .expression && witness.caseSensitive == sensitive)
                precondition(witness.expression == report.expression && witness.text != r.needle)
                precondition(witness.originalHex == ObservedReaderFixture.hex(
                    AtlasObservedReaderSearchRequest.originalBytes(spelling, encoding: encoding)))
                for (key, value) in [("needle", "need +different"), ("expression", "need +different"), ("primary_needle", "NEED"),
                                    ("predicate_scope", "original-search-observation"), ("text", r.needle)] {
                    var bad = rawWitness; bad[key] = value
                    rejects("foreign retained witness " + key) {
                        _ = try AtlasObservedReaderSearchWitness.decode(ObservedReaderFixture.encode(bad),
                            handle: owner, input: input(), report: report, target: target)
                    }
                }
                var readAgain = rawWitness; readAgain["additional_source_bytes_read"] = "1"
                rejects("witness selection must not reread source") {
                    _ = try AtlasObservedReaderSearchWitness.decode(ObservedReaderFixture.encode(readAgain),
                        handle: owner, input: input(), report: report, target: target)
                }
            }
        }
    }

    private static func navigationDoesNotRequalifyExpression() throws {
        let r = try request()
        let raw = ObservedExpressionWire.report(owner, try input(), r)
        let report = try decode(raw, r), target = report.target(for: report.hits[0])
        let witness = try AtlasObservedReaderSearchWitness.decode(
            ObservedReaderFixture.encode(ObservedExpressionWire.witness(raw)),
            handle: owner, input: input(), report: report, target: target)
        let pageRequest = try AtlasObservedReaderRequest(offset: 0, bytes: 64)
        let context = ObservedExpressionWire.context(owner, try input(), witness, observation: 3, request: pageRequest)
        let page = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(context),
            handle: owner, input: input(), request: pageRequest, hit: witness)
        precondition(page.text.contains("forbidden") && page.findHit?.expression == witness.expression)
        let expected = AtlasObservedReaderSearchContext.expressionPrimaryVerified(selection: NSRange(location: 4, length: 4))
        precondition(witness.verify(context: page) == expected && expected.selection == page.verifiedSearchSelection)
        precondition(expected.message.contains("original") && expected.message.contains("not rechecked"))
        var ordinary = context; ordinary["command"] = "window"; ordinary.removeValue(forKey: "find_hit")
        let current = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(ordinary),
            handle: owner, input: input(), request: pageRequest)
        precondition(witness.verify(context: current) == expected)
        let invalidNavigation: [(String, Any)] = [
            ("expression_revalidated", true), ("predicate_scope", "same-observed-stream"),
            ("expression_qualified", false), ("expression", "need +different"),
            ("primary_needle", "NEED"), ("case_sensitive", false), ("token_boundaries_verified", true)
        ]
        for (key, value) in invalidNavigation {
            var bad = context, hit = context["find_hit"] as! [String: Any]
            hit[key] = value; bad["find_hit"] = hit
            rejects("new context cannot rewrite expression authority " + key) {
                _ = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(bad),
                    handle: owner, input: input(), request: pageRequest, hit: witness)
            }
        }
        var missing = context, hit = context["find_hit"] as! [String: Any]
        hit.removeValue(forKey: "expression_revalidated"); missing["find_hit"] = hit
        rejects("missing non-revalidation evidence") {
            _ = try AtlasObservedReaderPage.decode(ObservedReaderFixture.encode(missing),
                handle: owner, input: input(), request: pageRequest, hit: witness)
        }
    }

    @MainActor private static func modelRetainsAuthorityAcrossRescansAndCancellation() async throws {
        let probe = ObservedExpressionProbe(), model = AtlasObservedReaderModel(transport: probe.transport)
        model.open(.init(input: try input(), project: UUID(), access: nil))
        try await wait("initial observed page") { !model.busy && model.page != nil }
        model.searchMode = .expression; model.searchText = ObservedExpressionWire.query
        model.searchCaseSensitive = false; model.countAllSearchMatches = false
        precondition(model.effectiveSearchCountAllMatches && model.canSearch)
        model.find()
        try await wait("first expression window") { !model.busy && model.searchAuthorized }
        let first = model.searchReport!, oldTarget = first.target(for: first.hits[0])
        precondition(first.request.countAllMatches && !first.request.caseSensitive)
        precondition(first.matchesSeen == 260 && first.hits.count == 256 && model.canNextSearchPage)
        model.selectSearchHit(oldTarget)
        try await wait("primary witness selected") { !model.busy && model.selectedSearchWitness != nil }
        precondition(model.selectedSearchWitness?.text == "NEED")
        model.goToVerifiedMatch()
        try await wait("primary-only new context") { !model.busy && model.page?.findHit != nil }
        precondition(model.searchContext == .expressionPrimaryVerified(selection: NSRange(location: 0, length: 4)))
        precondition(model.page!.text.contains("forbidden"))
        let retainedPage = model.page!.observation
        let retainedExpression = model.page!.findHit!.expression

        model.nextSearchPage()
        try await wait("later qualified occurrences") { !model.busy && model.searchReport?.request.pageStartAfter == 256 }
        let later = model.searchReport!
        precondition(later.request.generation > first.request.generation && later.observation > first.observation)
        precondition(later.request.needle.utf8.elementsEqual(first.request.needle.utf8))
        precondition(later.request.countAllMatches && !later.request.caseSensitive && later.hits.count == 4)
        precondition(later.hits.first?.occurrence == 257 && !later.pageHasMoreMatches && model.canPreviousSearchPage)
        precondition(model.page?.observation == retainedPage && model.page?.findHit?.expression == retainedExpression)
        let hitCalls = probe.hitCount; model.selectSearchHit(oldTarget)
        precondition(probe.hitCount == hitCalls, "A prior window cannot lend its row index to a fresh expression scan")

        model.previousSearchPage()
        try await wait("first positions rescanned") { !model.busy && model.searchReport?.request.pageStartAfter == 0 }
        let accepted = model.searchReport!
        let gate = ObservedReaderGate(); probe.blockNextReply(gate); model.find()
        try await wait("replacement accepted before reply") { gate.entered }
        model.cancel()
        precondition(!model.searchAuthorized && model.searchReport?.observation == accepted.observation)
        precondition(model.page?.observation == retainedPage)
        gate.release(); model.find()
        try await wait("fresh replacement after canceled delivery") {
            !model.busy && model.searchAuthorized && (model.searchReport?.request.generation ?? 0) > accepted.request.generation + 1
        }
        precondition(probe.activeCount == 0 && probe.peakCount == 1)
        precondition(probe.findRequests.allSatisfy {
            $0.mode == .expression && $0.countAllMatches && !$0.caseSensitive
                && $0.needle.utf8.elementsEqual(ObservedExpressionWire.query.utf8)
        })
        precondition(probe.findRequests.map(\.pageStartAfter) == [0, 256, 0, 0, 0])
        let newest = model.searchReport!, newestTarget = newest.target(for: newest.hits[0])
        model.searchMode = .text
        precondition(model.searchReport == nil && !model.searchAuthorized)
        precondition(model.page?.observation == retainedPage && model.page?.findHit?.expression == retainedExpression)
        model.selectSearchHit(newestTarget)
        precondition(model.selectedSearchWitness == nil)
        model.close()
        try await wait("observed owner retired") { probe.lifetime.closedCount == probe.lifetime.createCount }
    }

    @MainActor static func main() async throws {
        try admissionAndExactIdentity()
        try qualificationAndAtomicEvidence()
        try witnessesKeepPrimarySpellingAndEncoding()
        try navigationDoesNotRequalifyExpression()
        try await modelRetainsAuthorityAcrossRescansAndCancellation()
        print("Observed-reader expression receipt and lifecycle regression sources passed")
    }
}
