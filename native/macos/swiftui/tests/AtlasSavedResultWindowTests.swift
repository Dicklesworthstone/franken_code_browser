import Foundation
import Dispatch

/// Fixed producer envelopes exercise the real model, receipt admission and
/// retained-reader worker. The first 4,096-row window supplies only its requested
/// metadata page; later windows use the shared fixture's exact source witnesses.
/// Rust integration tests independently prove real >4,096-match extraction.
private final class SavedResultWindowFixture: @unchecked Sendable {
    enum Namespace: String, Sendable { case literal, expression, structure }
    enum Fault { case refused, lost, blocked }
    struct Scan: Sendable {
        let namespace: Namespace
        let generation, offset, maximum, bytes: UInt64
        let query: String
        let matching, counting: Bool
        let mode: AtlasSavedSearchMode?
        let nameMode: AtlasSearchNameMode?
    }
    let base = SavedRepositoryFixture(searchVectors: true)
    let entered = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var accepted: [Namespace: Scan] = [:]
    private var scans: [Scan] = []
    private var metadata: [String] = []
    private var fault: Fault?
    private func locked<T>(_ body: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return body() }
    var calls: [Scan] { locked { scans } }
    var metadataCalls: [String] { locked { metadata } }
    func failNext(_ value: Fault) { locked { fault = value } }

    private func render(_ reply: String, scan: Scan, start: UInt64 = 0, limit: UInt64 = 64) -> String {
        var out = try! JSONSerialization.jsonObject(with: Data(reply.utf8)) as! [String: Any]
        let actual = out["hits"] as! [[String: Any]]
        precondition(!actual.isEmpty)
        let first = scan.offset == 0
        let retained: UInt64 = first ? 4096 : UInt64(actual.count)
        let omitted: UInt64 = first && scan.namespace == .structure ? 2 : 0
        let continuation = retained + omitted
        let seen: UInt64 = first
            ? continuation + (scan.counting ? 4 : 1)
            : scan.offset + retained
        let end = min(retained, start + limit)
        precondition(start <= retained)
        let rows: [[String: Any]]
        if first {
            rows = (start..<end).map { position in
                var row = actual[0]
                row["hit_id"] = String(position + 1)
                let original = row["original_range"] as! [String: String]
                let width = UInt64(original["end"]!)! - UInt64(original["start"]!)!
                row["original_range"] = ["start": String(position), "end": String(position + width)]
                if scan.namespace == .structure {
                    var evidence = row["structure"] as! [String: Any]
                    evidence["candidate_id"] = String(position + 1)
                    evidence["evidence_range"] = row["original_range"]
                    row["structure"] = evidence
                }
                return row
            }
        } else {
            rows = Array(actual[Int(start)..<Int(end)])
        }
        out["retained_hits"] = String(retained); out["matches_seen"] = String(seen)
        out["hits"] = rows; out["next_offset"] = end < retained ? String(end) as Any : NSNull()
        out["result_offset"] = String(scan.offset); out["skipped_matches"] = String(scan.offset)
        out["has_more_matches"] = first
        out["next_match_offset"] = first ? String(continuation) as Any : NSNull()
        out["matches_seen_exact"] = false
        if scan.namespace == .structure {
            out["state"] = first && !scan.counting ? "truncated" : "complete"
            out["truncated"] = first; out["complete"] = false
            out["metadata_omitted"] = String(omitted)
            if first { out["metadata_bytes"] = "1048576" }
        } else {
            out["truncated"] = seen > retained
            out["count_all_matches"] = scan.counting
            out["matching_files"] = "2"; out["retained_matching_files"] = "2"
            // Counted totals are bounded by decoded work, independently of the
            // bounded metadata-page vector supplied to the native decoder.
            out["member_bytes_read"] = "65536"; out["scanned_bytes"] = "65536"
            if scan.namespace == .expression {
                out["loaded_bytes"] = "65536"
                out["state"] = first && !scan.counting ? "truncated" : "complete"
                out["complete"] = false
            } else {
                out["search_complete"] = false
            }
        }
        return base.encode(out)
    }

    private func search(_ owner: UInt64, scan: Scan, request: AtlasSavedRequest,
        upstream: AtlasSavedTransport) -> String? {
        let fault = locked { () -> Fault? in
            scans.append(scan)
            let value = self.fault; self.fault = nil
            return value
        }
        if case .refused? = fault { return nil }
        guard let reply = upstream.run(owner, request) else { return nil }
        locked { accepted[scan.namespace] = scan }
        let result = render(reply, scan: scan)
        if case .blocked? = fault { entered.signal(); resume.wait() }
        if case .lost? = fault { return nil }
        return result
    }

    private func results(_ owner: UInt64, namespace: Namespace, generation: UInt64,
        start: UInt64, limit: UInt64, request: AtlasSavedRequest, upstream: AtlasSavedTransport) -> String? {
        guard let scan = locked({ accepted[namespace] }), scan.generation == generation else { return nil }
        guard let reply = upstream.run(owner, request) else { return nil }
        locked { metadata.append("\(namespace.rawValue):\(generation):\(start)") }
        return render(reply, scan: scan, start: start, limit: limit)
    }

    var transport: AtlasSavedTransport {
        let upstream = base.savedTransport
        return AtlasSavedTransport(create: upstream.create, run: { [self] owner, request in
            switch request {
            case .search(let generation, let query, let maximum),
                 .countedSearch(let generation, let query, let maximum):
                let counting: Bool
                if case .countedSearch = request { counting = true } else { counting = false }
                return search(owner, scan: Scan(namespace: .literal, generation: generation, offset: 0,
                    maximum: maximum, bytes: 0, query: query, matching: true, counting: counting,
                    mode: nil, nameMode: nil), request: request, upstream: upstream)
            case .matchingSearch(let generation, let query, let matching, let counting, let maximum):
                return search(owner, scan: Scan(namespace: .literal, generation: generation, offset: 0,
                    maximum: maximum, bytes: 0, query: query, matching: matching, counting: counting,
                    mode: nil, nameMode: nil), request: request, upstream: upstream)
            case .searchWindow(let generation, let query, let matching, let counting, let maximum, let offset):
                return search(owner, scan: Scan(namespace: .literal, generation: generation, offset: offset,
                    maximum: maximum, bytes: 0, query: query, matching: matching, counting: counting,
                    mode: nil, nameMode: nil), request: .matchingSearch(generation: generation, needle: query,
                        caseSensitive: matching, countAllMatches: counting, matches: maximum), upstream: upstream)
            case .expressionSearch(let generation, let query, let maximum, let bytes),
                 .countedExpressionSearch(let generation, let query, let maximum, let bytes):
                let counting: Bool
                if case .countedExpressionSearch = request { counting = true } else { counting = false }
                return search(owner, scan: Scan(namespace: .expression, generation: generation, offset: 0,
                    maximum: maximum, bytes: bytes, query: query, matching: true, counting: counting,
                    mode: nil, nameMode: nil), request: request, upstream: upstream)
            case .matchingExpressionSearch(let generation, let query, let matching, let counting, let maximum, let bytes):
                return search(owner, scan: Scan(namespace: .expression, generation: generation, offset: 0,
                    maximum: maximum, bytes: bytes, query: query, matching: matching, counting: counting,
                    mode: nil, nameMode: nil), request: request, upstream: upstream)
            case .expressionWindow(let generation, let query, let matching, let counting, let maximum, let bytes, let offset):
                return search(owner, scan: Scan(namespace: .expression, generation: generation, offset: offset,
                    maximum: maximum, bytes: bytes, query: query, matching: matching, counting: counting,
                    mode: nil, nameMode: nil), request: .matchingExpressionSearch(generation: generation,
                        query: query, caseSensitive: matching, countAllMatches: counting,
                        matches: maximum, scanBytes: bytes), upstream: upstream)
            case .structureSearch(let generation, let mode, let nameMode, let query, let maximum, let bytes, let policy):
                return search(owner, scan: Scan(namespace: .structure, generation: generation, offset: 0,
                    maximum: maximum, bytes: bytes, query: query, matching: policy.caseSensitive, counting: policy.countAllMatches,
                    mode: mode, nameMode: nameMode), request: request, upstream: upstream)
            case .structureWindow(let generation, let mode, let nameMode, let query, let maximum, let bytes, let offset, let policy):
                return search(owner, scan: Scan(namespace: .structure, generation: generation, offset: offset,
                    maximum: maximum, bytes: bytes, query: query, matching: policy.caseSensitive, counting: policy.countAllMatches,
                    mode: mode, nameMode: nameMode), request: .structureSearch(generation: generation,
                        mode: mode, nameMode: nameMode, query: query, matches: maximum, sourceBytes: bytes, policy: policy),
                    upstream: upstream)
            case .results(let generation, let start, let limit):
                return results(owner, namespace: .literal, generation: generation, start: start, limit: limit,
                    request: .results(generation: generation, start: 0, limit: 64), upstream: upstream)
            case .expressionResults(let generation, let start, let limit):
                return results(owner, namespace: .expression, generation: generation, start: start, limit: limit,
                    request: .expressionResults(generation: generation, start: 0, limit: 64), upstream: upstream)
            case .structureResults(let generation, let start, let limit):
                return results(owner, namespace: .structure, generation: generation, start: start, limit: limit,
                    request: .structureResults(generation: generation, start: 0, limit: 64), upstream: upstream)
            default: return upstream.run(owner, request)
            }
        }, cancel: upstream.cancel, close: upstream.close, retirementFailed: upstream.retirementFailed,
            readerDomain: upstream.readerDomain)
    }
}

enum AtlasSavedResultWindowTests {
    @MainActor private static func wait(_ message: String, _ ready: () -> Bool) async {
        for _ in 0..<2000 {
            if ready() { return }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        preconditionFailure(message)
    }
    @MainActor private static func refused(_ message: String, _ work: @MainActor () async throws -> Void) async {
        do { try await work(); preconditionFailure(message) } catch {}
    }
    @MainActor private static func opened(_ fixture: SavedResultWindowFixture) async -> AtlasSavedRepositoryModel {
        let model = AtlasSavedRepositoryModel(transport: fixture.transport)
        model.open(path: "/saved/repository.fcbs", accessLease: nil)
        await wait("window fixture archive did not open") { !model.busy }
        precondition(model.info?.identity.owner == 40)
        return model
    }
    @MainActor static func run() async throws {
        try await literalWindowsKeepPoliciesAndExactReaders()
        await refusedLostAndCanceledWindowsReconcileOnce()
        try await expressionAndStructureWindowsKeepIndependentAuthority()
    }

    @MainActor private static func literalWindowsKeepPoliciesAndExactReaders() async throws {
        let f = SavedResultWindowFixture(), model = await opened(f)
        model.searchInput = "ANA"; model.searchCaseSensitive = false; model.countAllMatches = true
        model.search(); await wait("first literal window did not finish") { !model.busy }
        let first = model.searchPage!
        precondition(first.generation == 1 && first.retained == 4096 && first.hits.count == 64)
        precondition(first.window.offset == 0 && first.window.next == 4096 && first.window.earlier == nil)
        let scans = f.calls.count
        model.searchInput = "edited"; model.loadSearchWindow(offset: 4096, from: first)
        model.searchInput = "ANA"; model.searchCaseSensitive = true
        model.loadSearchWindow(offset: 4096, from: first)
        model.searchCaseSensitive = false; model.countAllMatches = false
        model.loadSearchWindow(offset: 4096, from: first)
        model.countAllMatches = true; model.loadSearchWindow(offset: 1, from: first)
        precondition(!model.busy && f.calls.count == scans, "Draft controls or an unproved offset authorized a scan")

        model.loadSearchResults(start: 64, from: first)
        await wait("first-window metadata page did not finish") { !model.busy }
        let metadata = model.searchPage!
        precondition(metadata.start == 64 && metadata.window == first.window && f.calls.count == scans)
        precondition(f.metadataCalls == ["literal:1:64"])
        model.loadSearchWindow(offset: 4096, from: first)
        precondition(!model.busy && f.calls.count == scans, "An older metadata-page action remained current")
        model.loadSearchWindow(offset: 4096, from: metadata)
        await wait("later literal window did not finish") { !model.busy }
        let later = model.searchPage!, submitted = f.calls.last!
        precondition(later.generation == 2 && later.window.offset == 4096 && later.window.skipped == 4096)
        precondition(later.retained == 4 && later.seen == 4100 && later.window.next == nil && later.window.earlier == 0)
        precondition(later.truncated && !later.window.hasMore && !later.complete)
        precondition(submitted.query == "ANA" && !submitted.matching && submitted.counting
            && submitted.maximum == 4096 && submitted.offset == 4096)
        let reader = try await model.openHit(later.hits[3], from: later, readerTransport: f.base.readerTransport())
        precondition(reader.match?.start == 410 && reader.match?.end == 416
            && reader.match?.originalHex == "61006e006100" && reader.canUseMatch)
        await refused("A previous result window reopened a stale hit") {
            _ = try await model.openHit(first.hits[0], from: first, readerTransport: f.base.readerTransport())
        }
        model.loadSearchResults(start: 2, from: later)
        await wait("later-window metadata page did not finish") { !model.busy }
        precondition(model.searchPage?.hits.map(\.id) == [3, 4] && model.searchPage?.window == later.window)
        precondition(f.calls.count == scans + 1)
        model.loadSearchWindow(offset: 0, from: model.searchPage!)
        await wait("first-window return did not finish") { !model.busy }
        precondition(model.queryGeneration == 3 && model.searchPage?.window.offset == 0)
        await refused("A replaced later window authorized its former row") {
            _ = try await model.openHit(later.hits[3], from: later, readerTransport: f.base.readerTransport())
        }
        model.closeArchive()
        precondition(reader.canUseMatch); _ = try await reader.withMatchReader { $0 }
        precondition(!f.base.calls.contains("LIVE-OPEN"))
        reader.reader.close(); model.close()
    }

    @MainActor private static func refusedLostAndCanceledWindowsReconcileOnce() async {
        let f = SavedResultWindowFixture(), model = await opened(f)
        model.searchInput = "ana"; model.search()
        await wait("recovery first window did not finish") { !model.busy }
        let first = model.searchPage!
        f.failNext(.refused); model.loadSearchWindow(offset: 4096, from: first)
        await wait("refused window did not reconcile") { !model.busy }
        precondition(model.queryGeneration == 1 && model.searchPage?.window.offset == 0 && model.searchIsCurrent)
        precondition(f.calls.filter { $0.generation == 2 }.count == 1 && f.metadataCalls.isEmpty)

        f.failNext(.lost); model.loadSearchWindow(offset: 4096, from: model.searchPage!)
        await wait("lost later-window reply did not reconcile") { !model.busy }
        precondition(model.queryGeneration == 3 && model.searchPage?.window.offset == 4096
            && model.searchPage?.window.next == nil && model.searchIsCurrent)
        precondition(f.calls.filter { $0.generation == 3 }.count == 1)
        precondition(f.metadataCalls == ["literal:3:0"])

        f.failNext(.blocked); model.loadSearchWindow(offset: 0, from: model.searchPage!)
        await wait("replacement did not enter its foreign worker") { f.entered.wait(timeout: .now()) == .success }
        model.searchInput = "edited after submission"; model.cancel(); f.resume.signal()
        await wait("canceled accepted window did not reconcile") { !model.busy }
        precondition(model.queryGeneration == 4 && model.searchPage?.window.offset == 0 && !model.searchIsCurrent)
        precondition(model.searchPage?.needle == "ana" && f.calls.filter { $0.generation == 4 }.count == 1)
        precondition(f.metadataCalls == ["literal:3:0", "literal:4:0"])
        model.searchInput = "ana"; precondition(model.searchIsCurrent)
        model.close()
    }

    @MainActor private static func expressionAndStructureWindowsKeepIndependentAuthority() async throws {
        let f = SavedResultWindowFixture(), model = await opened(f)
        model.searchInput = "ana"; model.search()
        await wait("independent literal query did not finish") { !model.busy }
        let literalGeneration = model.queryGeneration

        model.searchMode = .expression; model.searchInput = "ANA +BANANA -UNSAFE"
        model.searchCaseSensitive = false; model.countAllMatches = true; model.search()
        await wait("first expression window did not finish") { !model.busy }
        let firstExpression = model.expressionPage!
        precondition(firstExpression.window.next == 4096)
        model.countAllMatches = false
        model.loadExpressionWindow(offset: 4096, from: firstExpression)
        precondition(!model.busy && f.calls.count == 2)
        model.countAllMatches = true; f.failNext(.lost)
        model.loadExpressionWindow(offset: 4096, from: firstExpression)
        await wait("expression window did not reconcile") { !model.busy }
        let expression = model.expressionPage!, expressionCall = f.calls.last!
        precondition(expression.generation == 2 && expression.window.offset == 4096 && expression.hits.count == 4)
        precondition(expression.counting.all && !expression.caseSensitive && expression.window.next == nil)
        precondition(expressionCall.query == firstExpression.expression && expressionCall.bytes == firstExpression.scanLimit
            && expressionCall.maximum == firstExpression.counting.maximum && expressionCall.offset == 4096)
        precondition(model.queryGeneration == literalGeneration && f.metadataCalls == ["expression:2:0"])
        let expressionReader = try await model.openExpressionHit(expression.hits[2], from: expression,
            readerTransport: f.base.readerTransport())
        precondition(expressionReader.match?.originalHex == "61006e006100")
        model.loadExpressionResults(start: 2, from: expression)
        await wait("expression metadata page did not finish") { !model.busy }
        precondition(model.expressionPage?.window == expression.window && f.calls.count == 3)

        model.searchMode = .occurrences; model.searchNameMode = .prefix
        model.searchInput = f.base.occurrenceNeedle; model.search()
        await wait("first occurrence window did not finish") { !model.busy }
        let firstStructure = model.structurePage!
        precondition(firstStructure.nameMode == .exact && firstStructure.metadataOmitted == 2)
        precondition(firstStructure.policy.caseSensitive && firstStructure.policy.countAllMatches && firstStructure.seen == 4102)
        precondition(firstStructure.window.next == 4098,
            "A continuation must include qualified names omitted from retained metadata")
        model.searchInput += "x"
        model.loadStructureWindow(offset: 4098, from: firstStructure)
        precondition(!model.busy && f.calls.count == 4)
        model.searchInput = f.base.occurrenceNeedle; f.failNext(.lost)
        model.loadStructureWindow(offset: 4098, from: firstStructure)
        await wait("occurrence window did not reconcile") { !model.busy }
        let structure = model.structurePage!, structureCall = f.calls.last!
        precondition(structure.generation == 2 && structure.window.offset == 4098 && structure.window.earlier == 2)
        precondition(structure.retained == 2 && structure.window.next == nil && structure.metadataOmitted == 0)
        precondition(structureCall.mode == .occurrences && structureCall.nameMode == .exact
            && structureCall.query == firstStructure.needle && structureCall.bytes == firstStructure.sourceLimit
            && structureCall.offset == 4098 && structureCall.matching && structureCall.counting)
        precondition(model.queryGeneration == literalGeneration && model.expressionGeneration == 2)
        precondition(f.metadataCalls.last == "structure:2:0" && f.calls.count == 5)
        let structuralReader = try await model.openStructureHit(structure.hits[1], from: structure,
            readerTransport: f.base.readerTransport())
        precondition(structuralReader.match?.start == 2 && structuralReader.match?.end == 416
            && structuralReader.match?.originalHex == f.base.hex(f.base.searchUTF16[2..<416]))
        await refused("An old structural window remained actionable") {
            _ = try await model.openStructureHit(firstStructure.hits[0], from: firstStructure,
                readerTransport: f.base.readerTransport())
        }
        model.loadStructureResults(start: 1, from: structure)
        await wait("structural metadata page did not finish") { !model.busy }
        precondition(model.structurePage?.hits.map(\.hit.id) == [2]
            && model.structurePage?.window == structure.window && f.calls.count == 5)
        model.closeArchive()
        precondition(expressionReader.canUseMatch && structuralReader.canUseMatch)
        _ = try await structuralReader.withMatchReader { $0 }
        precondition(!f.base.calls.contains("LIVE-OPEN"))
        expressionReader.reader.close(); structuralReader.reader.close(); model.close()
    }
}
