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
        var scope: AtlasSavedStructureScope = .archive
    }
    let base: SavedRepositoryFixture
    init(nested: Bool = false) {
        let original = SavedRepositoryFixture(searchVectors: true)
        let member = original.members[1]
        base = nested ? SavedRepositoryFixture(searchVectors: true, sourceMemberOverride:
            .init(path: Array("src/".utf8) + member.path, display: "src/" + member.display,
                bytes: member.bytes, digest: member.digest)) : original
    }
    let entered = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var accepted: [Namespace: Scan] = [:]
    private var scans: [Scan] = []
    private var metadata: [String] = []
    private var fault: Fault?
    private var metadataChange: String?
    private var changedMetadata: String?
    private var wrongReaderScope = false
    private func locked<T>(_ body: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return body() }
    var calls: [Scan] { locked { scans } }
    var metadataCalls: [String] { locked { metadata } }
    var changedMetadataReply: String? { locked { changedMetadata } }
    func failNext(_ value: Fault) { locked { fault = value } }
    func changeNextMetadata(_ field: String) { locked { metadataChange = field } }
    func forgeNextReaderScope() { locked { wrongReaderScope = true } }

    private func addScope(_ scope: AtlasSavedStructureScope, to out: inout [String: Any]) {
        let selected = base.members.filter { scope.includes($0.path) }
        out["scope"] = scope == .archive ? "whole-saved-archive" : scope.code == 1 ? "saved-directory" : "saved-member"
        out["scope_path_hex"] = base.hex(scope.path)
        out["scope_known_members"] = String(selected.count)
        out["scope_captured_members"] = String(selected.filter { $0.bytes != nil }.count)
        out["scope_membership_complete"] = scope.code == 2 ? selected.count == 1 : out["discovery_complete"] as! Bool
    }

    private func render(_ reply: String, scan: Scan, start: UInt64 = 0, limit: UInt64 = 64) -> String {
        var out = try! JSONSerialization.jsonObject(with: Data(reply.utf8)) as! [String: Any]
        let original = out["hits"] as! [[String: Any]]
        let actual = original.filter { row in
            let member = Int(row["member"] as! String)!
            return scan.scope.includes(base.members[member].path)
        }.enumerated().map { index, row -> [String: Any] in
            var row = row; row["hit_id"] = String(index + 1); return row
        }
        precondition(!actual.isEmpty)
        let first = scan.offset == 0
        let retained: UInt64 = first ? 4096 : UInt64(actual.count)
        let omitted: UInt64 = first && scan.namespace == .structure ? 2 : 0
        let continuation = retained + omitted
        let seen: UInt64 = first
            ? continuation + (scan.counting ? (scan.scope == .archive ? 4 : UInt64(actual.count)) : 1)
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
            // These synthetic envelopes account for the full counted source
            // work, not only the two original witnesses used for activation.
            out["loaded_bytes"] = "65536"; out["member_bytes_read"] = "65536"
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
        if scan.scope != .archive {
            precondition(scan.namespace != .structure)
            addScope(scan.scope, to: &out)
            let members = UInt64(out["scope_known_members"] as! String)!
            let captured = UInt64(out["scope_captured_members"] as! String)!
            let complete = (out["scope_membership_complete"] as! Bool) && members == captured
                && (scan.counting || !first)
            out["unavailable_files"] = String(members - captured)
            out["matching_files"] = "1"; out["retained_matching_files"] = "1"
            out["matches_seen_exact"] = complete
            if scan.namespace == .expression {
                out["metadata_examined"] = String(members); out["scope_files"] = String(members)
                out["metadata_excluded"] = "0"; out["members_loaded"] = String(captured)
                out["predicate_scans"] = String(captured * 2); out["predicate_rejections"] = "0"
                out["primary_scans"] = String(captured); out["complete"] = complete
            } else {
                out["index_eliminated_files"] = "0"
                out["fallback_files"] = out["index_used"] as? Bool == true ? String(captured) : "0"
                out["search_complete"] = complete
            }
        }
        if scan.namespace == .expression, out["fallback_files"] != nil {
            out["fallback_files"] = out["index_used"] as? Bool == true ? out["members_loaded"] as! String : "0"
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
        let result = render(reply, scan: scan, start: start, limit: limit)
        let field = locked { () -> String? in
            let value = metadataChange; metadataChange = nil; return value
        }
        guard let field else { return result }
        var out = try! JSONSerialization.jsonObject(with: Data(result.utf8)) as! [String: Any]
        if field == "scope" {
            addScope(.archive, to: &out)
            let changed = base.encode(out)
            locked { changedMetadata = changed }
            return changed
        }
        precondition(namespace == .structure || (namespace == .expression && field == "posting_entries_visited"))
        let value = UInt64(out[field] as! String)!
        out[field] = String(["max_hits", "max_source_bytes", "files_searched"].contains(field) ? value - 1 : value + 1)
        if field == "loaded_bytes" { out["member_bytes_read"] = out[field] }
        let changed = base.encode(out)
        locked { changedMetadata = changed }
        return changed
    }

    private func reader(_ owner: UInt64, reader: UInt64, namespace: Namespace,
        generation: UInt64, hit: UInt64, upstream: AtlasSavedTransport) -> String? {
        guard let scan = locked({ accepted[namespace] }), scan.generation == generation else { return nil }
        let needle = namespace == .expression ? base.expressionPrimary(scan.query) : scan.query
        let candidates = base.searchHits(needle, caseSensitive: scan.matching).enumerated().filter {
            scan.scope.includes(base.members[Int($0.element.member)].path)
        }
        guard hit > 0, hit <= UInt64(candidates.count) else { return nil }
        let original = UInt64(candidates[Int(hit - 1)].offset + 1)
        let request: AtlasSavedRequest = namespace == .expression
            ? .openExpressionHit(reader: reader, generation: generation, hit: original)
            : .openHit(reader: reader, generation: generation, hit: original)
        guard let reply = upstream.run(owner, request) else { return nil }
        var out = try! JSONSerialization.jsonObject(with: Data(reply.utf8)) as! [String: Any]
        var selection = out["selection"] as! [String: Any]
        selection["hit_id"] = String(hit); out["selection"] = selection
        let forged = locked { let value = wrongReaderScope; wrongReaderScope = false; return value }
        if scan.scope != .archive || forged { addScope(forged ? .archive : scan.scope, to: &out) }
        if namespace == .expression, scan.scope != .archive, out["fallback_files"] != nil {
            out["fallback_files"] = out["index_used"] as? Bool == true ? out["scope_captured_members"] as! String : "0"
        }
        return base.encode(out)
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
            case .scopedSearchWindow(let generation, let query, let matching, let counting, let maximum, let offset, let scope):
                return search(owner, scan: Scan(namespace: .literal, generation: generation, offset: offset,
                    maximum: maximum, bytes: 0, query: query, matching: matching, counting: counting,
                    mode: nil, nameMode: nil, scope: scope), request: .matchingSearch(generation: generation, needle: query,
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
            case .scopedExpressionWindow(let generation, let query, let matching, let counting, let maximum, let bytes, let offset, let scope):
                return search(owner, scan: Scan(namespace: .expression, generation: generation, offset: offset,
                    maximum: maximum, bytes: bytes, query: query, matching: matching, counting: counting,
                    mode: nil, nameMode: nil, scope: scope), request: .matchingExpressionSearch(generation: generation,
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
            case .openHit(let destination, let generation, let hit):
                return reader(owner, reader: destination, namespace: .literal, generation: generation, hit: hit, upstream: upstream)
            case .openExpressionHit(let destination, let generation, let hit):
                return reader(owner, reader: destination, namespace: .expression, generation: generation, hit: hit, upstream: upstream)
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
        try await structuralMetadataCannotRewriteAcceptedCoverage()
        try await sharedScopesFreezeWindowsRecoveryAndReaders()
        try await indexedExpressionWindowsBindEachSubmittedIndex()
        await modernExpressionReceiptsCannotDowngradeAcrossGenerations()
    }

    @MainActor private static func literalWindowsKeepPoliciesAndExactReaders() async throws {
        let f = SavedResultWindowFixture(), model = await opened(f)
        let pin = String(repeating: "a", count: 64)
        model.attachIndex(path: "/saved/index.fcbd", pin: pin, accessLease: nil)
        await wait("window fixture index did not attach") { !model.busy }
        precondition(model.index?.generation == 1)
        model.searchInput = "ANA"; model.searchCaseSensitive = false; model.countAllMatches = true
        model.search(); await wait("first literal window did not finish") { !model.busy }
        let first = model.searchPage!
        precondition(first.generation == 1 && first.retained == 4096 && first.hits.count == 64)
        precondition(!first.caseSensitive && first.indexGeneration == 1 && first.indexPin == pin)
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
        precondition(metadata.indexGeneration == 1 && metadata.indexPin == pin)
        precondition(f.metadataCalls == ["literal:1:64"])
        model.loadSearchWindow(offset: 4096, from: first)
        precondition(!model.busy && f.calls.count == scans, "An older metadata-page action remained current")
        model.loadSearchWindow(offset: 4096, from: metadata)
        await wait("later literal window did not finish") { !model.busy }
        let later = model.searchPage!, submitted = f.calls.last!
        precondition(later.generation == 2 && later.window.offset == 4096 && later.window.skipped == 4096)
        precondition(later.retained == 4 && later.seen == 4100 && later.window.next == nil && later.window.earlier == 0)
        precondition(later.truncated && !later.window.hasMore && !later.complete)
        precondition(!later.caseSensitive && later.indexGeneration == 1 && later.indexPin == pin)
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
        precondition(model.searchPage?.indexGeneration == 1 && model.searchPage?.indexPin == pin)
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
        let occurrenceQuery = "É" + String(repeating: "X", count: 200) + "BANANA"
        model.searchInput = occurrenceQuery; model.search()
        await wait("first occurrence window did not finish") { !model.busy }
        let firstStructure = model.structurePage!
        precondition(firstStructure.nameMode == .exact && firstStructure.metadataOmitted == 2)
        precondition(!firstStructure.policy.caseSensitive && firstStructure.policy.countAllMatches && firstStructure.seen == 4102)
        precondition(firstStructure.window.next == 4098,
            "A continuation must include qualified names omitted from retained metadata")
        model.searchInput += "x"
        model.loadStructureWindow(offset: 4098, from: firstStructure)
        precondition(!model.busy && f.calls.count == 4)
        model.searchInput = occurrenceQuery; f.failNext(.lost)
        model.loadStructureWindow(offset: 4098, from: firstStructure)
        await wait("occurrence window did not reconcile") { !model.busy }
        let structure = model.structurePage!, structureCall = f.calls.last!
        precondition(structure.generation == 2 && structure.window.offset == 4098 && structure.window.earlier == 2)
        precondition(structure.retained == 2 && structure.window.next == nil && structure.metadataOmitted == 0)
        precondition(structureCall.mode == .occurrences && structureCall.nameMode == .exact
            && structureCall.query == firstStructure.needle && structureCall.bytes == firstStructure.sourceLimit
            && structureCall.offset == 4098 && !structureCall.matching && structureCall.counting)
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


    @MainActor private static func indexedExpressionWindowsBindEachSubmittedIndex() async throws {
        let f = SavedResultWindowFixture()
        f.base.expressionIndexReceipts = true
        let model = await opened(f), member = AtlasSavedStructureScope.member(f.base.members[2].path)
        let pin = String(repeating: "a", count: 64), replacementPin = String(repeating: "b", count: 64)
        model.attachIndex(path: "/saved/first.fcbd", pin: pin, accessLease: nil)
        await wait("scoped expression index did not attach") { !model.busy }
        model.searchMode = .expression; model.structureScope = member
        model.searchInput = "ANA +BANANA -UNSAFE"; model.searchCaseSensitive = false; model.countAllMatches = true
        model.search(); await wait("first indexed expression window did not finish") { !model.busy }
        let first = model.expressionPage!
        precondition(first.complete && first.scope.members == 1 && first.index.generation == 1
            && first.index.fallbackFiles == 1 && first.window.next == 4096)
        f.failNext(.lost); model.loadExpressionWindow(offset: 4096, from: first)
        await wait("indexed expression window did not recover") { !model.busy }
        let later = model.expressionPage!
        precondition(later.generation == 2 && later.scope == first.scope && later.index == first.index
            && later.window.offset == 4096 && later.hits.count == 2 && f.calls.count == 2)
        precondition(f.metadataCalls == ["expression:2:0"])
        model.detachIndex(); await wait("index did not detach between expression windows") { !model.busy }
        model.loadExpressionResults(start: 1, from: later)
        await wait("detached-index expression metadata did not load") { !model.busy }
        let metadata = model.expressionPage!
        precondition(metadata.start == 1 && metadata.index == first.index && model.index == nil && f.calls.count == 2)
        let retainedReader = try await model.openExpressionHit(metadata.hits[0], from: metadata,
            readerTransport: f.base.readerTransport())
        precondition(retainedReader.match?.start == 410 && retainedReader.match?.end == 416
            && retainedReader.match?.originalHex == "61006e006100")
        if case .expressionHit(_, _, _, _, _, let scope, let index) = retainedReader.origin {
            precondition(scope == member && index == first.index)
        } else { preconditionFailure("scoped indexed reader lost query-time provenance") }
        f.changeNextMetadata("posting_entries_visited")
        model.loadExpressionResults(start: 0, from: metadata)
        await wait("altered expression index metadata did not drain") { !model.busy }
        precondition(model.expressionPage?.start == 1 && model.expressionPage?.index == first.index)

        // Index attachment may change between bounded scans without changing
        // the accepted expression, case policy, counting mode or raw scope.
        model.loadExpressionWindow(offset: 0, from: metadata)
        await wait("new direct expression window after detach did not finish") { !model.busy }
        let direct = model.expressionPage!
        precondition(direct.generation == 3 && direct.index.hasReceipt && direct.index.generation == nil
            && direct.scope == first.scope && direct.window.offset == 0 && f.calls.count == 3)
        precondition(direct.expression == first.expression && !direct.caseSensitive && direct.counting.all)
        model.attachIndex(path: "/saved/replacement.fcbd", pin: replacementPin, accessLease: nil)
        await wait("replacement index did not attach between expression windows") { !model.busy }
        let replacementGeneration = model.index!.generation
        f.failNext(.blocked); model.loadExpressionWindow(offset: 4096, from: direct)
        await wait("indexed expression window did not enter cancelable worker") { f.entered.wait(timeout: .now()) == .success }
        model.structureScope = .archive; model.cancel(); f.resume.signal()
        await wait("canceled indexed expression window did not recover") { !model.busy }
        let recovered = model.expressionPage!
        precondition(recovered.generation == 4 && recovered.window.offset == 4096 && recovered.scope == first.scope
            && recovered.index.generation == replacementGeneration && recovered.index.pin == replacementPin)
        precondition(!model.expressionIsCurrent && f.calls.count == 4
            && f.calls.filter { $0.namespace == .expression && $0.generation == 4 }.count == 1)
        model.structureScope = member
        let reader = try await model.openExpressionHit(recovered.hits[0], from: recovered,
            readerTransport: f.base.readerTransport())
        precondition(reader.match?.start == 406 && reader.match?.originalHex == "61006e006100")
        model.closeArchive()
        precondition(retainedReader.canUseMatch && reader.canUseMatch && !f.base.calls.contains("LIVE-OPEN"))
        _ = try await retainedReader.withMatchReader { $0 }
        retainedReader.reader.close(); reader.reader.close(); model.close()
    }

    @MainActor private static func modernExpressionReceiptsCannotDowngradeAcrossGenerations() async {
        let f = SavedResultWindowFixture()
        f.base.expressionIndexReceipts = true
        let model = await opened(f), pin = String(repeating: "a", count: 64)
        model.attachIndex(path: "/saved/modern.fcbd", pin: pin, accessLease: nil)
        await wait("modern expression index did not attach") { !model.busy }
        model.searchMode = .expression; model.searchInput = "ANA +BANANA -UNSAFE"
        model.searchCaseSensitive = false; model.search()
        await wait("modern expression receipt did not arrive") { !model.busy }
        let modern = model.expressionPage!
        precondition(modern.index.hasReceipt && modern.index.generation == 1)
        // Strip the entire additive receipt from both the accepted scan reply
        // and retained results. Partial-field rejection alone cannot catch this.
        f.base.expressionIndexReceipts = false
        model.loadExpressionWindow(offset: 4096, from: modern)
        await wait("downgraded expression window did not drain") { !model.busy }
        precondition(model.expressionPage == nil && model.expressionGeneration == nil && f.calls.count == 2)
        precondition(f.metadataCalls == ["expression:2:0"])
        model.reloadSearchResults(); await wait("downgraded recovery did not drain") { !model.busy }
        precondition(model.expressionPage == nil && f.calls.count == 2)
        f.base.expressionIndexReceipts = true
        model.reloadSearchResults(); await wait("restored modern receipt did not recover") { !model.busy }
        precondition(model.expressionGeneration == 2 && model.expressionPage?.index == modern.index
            && model.expressionPage?.window.offset == 4096 && f.calls.count == 2)

        // Clearing result rows does not let the same handle shed its known
        // receipt contract. A genuinely newly opened legacy service still works.
        model.clearSearch(); await wait("modern expression clear did not finish") { !model.busy }
        f.base.expressionIndexReceipts = false
        model.searchInput = modern.expression; model.search()
        await wait("post-clear receipt downgrade did not drain") { !model.busy }
        precondition(model.expressionPage == nil && model.expressionGeneration == nil && f.calls.count == 3)
        model.open(path: "/saved/legacy.fcbs", accessLease: nil)
        await wait("legacy archive did not open") { !model.busy }
        model.attachIndex(path: "/saved/legacy.fcbd", pin: pin, accessLease: nil)
        await wait("legacy archive index did not attach") { !model.busy }
        model.searchInput = modern.expression; model.search()
        await wait("legacy direct expression did not finish") { !model.busy }
        precondition(model.expressionPage?.index.hasReceipt == false && model.expressionPage?.index.generation == nil
            && model.expressionGeneration == 1 && model.expressionIsCurrent && model.index?.generation == 1)
        precondition(!f.base.calls.contains("LIVE-OPEN"))
        model.close()
    }

    @MainActor private static func sharedScopesFreezeWindowsRecoveryAndReaders() async throws {
        let f = SavedResultWindowFixture(nested: true), model = await opened(f)
        let directory = AtlasSavedStructureScope.directory(Array("src".utf8))
        let member = AtlasSavedStructureScope.member(f.base.members[2].path)
        model.attachIndex(path: "/saved/index.fcbd", pin: String(repeating: "a", count: 64), accessLease: nil)
        await wait("scoped fixture index did not attach") { !model.busy }
        model.structureScope = directory; model.searchInput = "ANA"
        model.searchCaseSensitive = false; model.countAllMatches = true
        model.search(); await wait("scoped literal query did not finish") { !model.busy }
        let first = model.searchPage!
        precondition(first.scope.scope == directory && first.scope.members == 2 && first.scope.captured == 2)
        precondition(first.complete && first.counting.exact && first.retained == 4096 && first.seen == 4098)
        precondition(first.indexGeneration == 1 && first.unavailable == 0 && first.fallbackFiles == 2)
        let scans = f.calls.count
        model.structureScope = .archive
        model.loadSearchWindow(offset: 4096, from: first)
        await refused("A different draft scope authorized a retained match") {
            _ = try await model.openHit(first.hits[0], from: first, readerTransport: f.base.readerTransport())
        }
        precondition(!model.busy && !model.searchIsCurrent && f.calls.count == scans)
        model.structureScope = directory
        model.loadSearchResults(start: 64, from: first)
        await wait("scoped metadata page did not finish") { !model.busy }
        let metadata = model.searchPage!
        precondition(metadata.scope == first.scope && metadata.start == 64 && f.calls.count == scans)
        f.changeNextMetadata("scope")
        model.loadSearchResults(start: 128, from: metadata)
        await wait("wrong-scope metadata did not drain") { !model.busy }
        precondition(model.searchPage?.start == 64 && model.searchPage!.hasSameSummary(as: metadata))
        model.loadSearchWindow(offset: 4096, from: metadata)
        await wait("scoped later window did not finish") { !model.busy }
        let later = model.searchPage!
        precondition(later.scope == first.scope && later.window.offset == 4096 && later.hits.count == 2)
        precondition(f.calls.last?.scope == directory && f.calls.last?.matching == false
            && f.calls.last?.counting == true && f.calls.count == scans + 1)
        f.forgeNextReaderScope()
        await refused("A reader receipt with another raw scope was accepted") {
            _ = try await model.openHit(later.hits[1], from: later, readerTransport: f.base.readerTransport())
        }
        let rawReader = try await model.openHit(later.hits[1], from: later, readerTransport: f.base.readerTransport())
        precondition(rawReader.match?.originalHex == "616e61" && rawReader.match?.start == 205)
        if case .hit(_, _, _, _, let scope) = rawReader.origin { precondition(scope == directory) }
        else { preconditionFailure("literal reader lost its accepted scope") }

        // An index change does not rewrite the query-time membership receipt.
        model.detachIndex(); await wait("scoped index detach did not finish") { !model.busy }
        precondition(model.searchPage!.hasSameSummary(as: later) && model.searchIsCurrent && rawReader.canUseMatch)

        model.searchMode = .expression; model.structureScope = member
        model.searchInput = "ANA +BANANA -UNSAFE"; model.search()
        await wait("scoped expression did not finish") { !model.busy }
        let expressionFirst = model.expressionPage!
        precondition(expressionFirst.scope.scope == member && expressionFirst.scope.members == 1
            && expressionFirst.complete && expressionFirst.scopeFiles == 1)
        let beforeRecovery = f.calls.count
        f.failNext(.lost); model.loadExpressionWindow(offset: 4096, from: expressionFirst)
        model.structureScope = directory
        await wait("scoped expression did not recover its lost reply") { !model.busy }
        let expression = model.expressionPage!
        precondition(expression.scope == expressionFirst.scope && expression.window.offset == 4096
            && expression.hits.count == 2 && !model.expressionIsCurrent && f.calls.count == beforeRecovery + 1)
        precondition(f.calls.last?.scope == member && f.metadataCalls.last == "expression:2:0")
        model.structureScope = member; precondition(model.expressionIsCurrent)
        f.forgeNextReaderScope()
        await refused("An expression reader with a different scope was accepted") {
            _ = try await model.openExpressionHit(expression.hits[1], from: expression, readerTransport: f.base.readerTransport())
        }
        let utf16Reader = try await model.openExpressionHit(expression.hits[1], from: expression,
            readerTransport: f.base.readerTransport())
        precondition(utf16Reader.match?.start == 410 && utf16Reader.match?.end == 416
            && utf16Reader.match?.originalHex == "61006e006100")
        if case .expressionHit(_, _, _, _, _, let scope, _) = utf16Reader.origin { precondition(scope == member) }
        else { preconditionFailure("expression reader lost its exact raw member scope") }

        f.failNext(.blocked); model.loadExpressionWindow(offset: 0, from: expression)
        await wait("scoped replacement did not enter its worker") { f.entered.wait(timeout: .now()) == .success }
        model.structureScope = .archive; model.cancel(); f.resume.signal()
        await wait("canceled scoped replacement did not reconcile") { !model.busy }
        precondition(model.expressionPage?.scope.scope == member && model.expressionPage?.window.offset == 0
            && !model.expressionIsCurrent && f.calls.filter { $0.namespace == .expression && $0.generation == 3 }.count == 1)
        model.structureScope = member; precondition(model.expressionIsCurrent)
        await refused("A replaced scoped generation authorized its old hit") {
            _ = try await model.openExpressionHit(expression.hits[0], from: expression, readerTransport: f.base.readerTransport())
        }
        model.open(path: "/saved/replacement.fcbs", accessLease: nil)
        await wait("archive replacement did not finish") { !model.busy }
        precondition(model.structureScope == .archive && model.searchPage == nil && model.expressionPage == nil)
        precondition(rawReader.canUseMatch && utf16Reader.canUseMatch)
        _ = try await utf16Reader.withMatchReader { $0 }
        precondition(!f.base.calls.contains("LIVE-OPEN"))
        rawReader.reader.close(); utf16Reader.reader.close(); model.close()
    }

    @MainActor private static func structuralMetadataCannotRewriteAcceptedCoverage() async throws {
        let f = SavedResultWindowFixture(), model = await opened(f)
        model.searchMode = .occurrences; model.searchInput = f.base.occurrenceNeedle; model.search()
        await wait("structural summary query did not finish") { !model.busy }
        model.loadStructureWindow(offset: 4098, from: model.structurePage!)
        await wait("structural summary tail did not finish") { !model.busy }
        let accepted = model.structurePage!
        precondition(accepted.maximum == 4096 && accepted.hits.count == 2)
        let reader = try await model.openStructureHit(accepted.hits[1], from: accepted,
            readerTransport: f.base.readerTransport())
        let scans = f.calls.count
        for field in ["max_hits", "max_source_bytes", "loaded_bytes", "peak_source_bytes",
            "member_read_calls", "metadata_bytes", "files_searched"] {
            f.changeNextMetadata(field)
            model.loadStructureResults(start: 0, from: accepted)
            await wait("changed structural \(field) did not drain") { !model.busy }
            // Each changed page is individually valid. Its failure is the
            // attempt to rewrite this already accepted query's frozen facts.
            let changed = try AtlasSavedStructurePage.decode(f.changedMetadataReply,
                identity: accepted.identity, generation: accepted.generation, mode: accepted.mode,
                nameMode: accepted.nameMode, needle: accepted.needle, start: 0, limit: 64,
                resultOffset: accepted.window.offset, policy: accepted.policy)
            precondition(!changed.hasSameSummary(as: accepted))
            precondition(model.structurePage!.hasSameSummary(as: accepted) && model.structurePage?.start == 0
                && model.structureIsCurrent && reader.canUseMatch && f.calls.count == scans,
                "Metadata pagination rewrote accepted \(field)")
            let pages = f.metadataCalls.count, handoffs = f.base.calls.filter { $0.hasPrefix("handoff:") }.count
            model.loadStructureResults(start: 1, from: changed)
            model.loadStructureWindow(offset: 0, from: changed)
            await refused("Changed structural summary authorized source activation") {
                _ = try await model.openStructureHit(changed.hits[0], from: changed,
                    readerTransport: f.base.readerTransport())
            }
            precondition(!model.busy && f.calls.count == scans && f.metadataCalls.count == pages
                && f.base.calls.filter { $0.hasPrefix("handoff:") }.count == handoffs)
        }
        model.loadStructureResults(start: 1, from: accepted)
        await wait("unchanged structural metadata did not finish") { !model.busy }
        precondition(model.structurePage?.start == 1 && model.structurePage!.hasSameSummary(as: accepted)
            && f.calls.count == scans, "A valid metadata page was refused")
        model.closeArchive()
        precondition(reader.canUseMatch && reader.match?.originalHex == f.base.hex(f.base.searchUTF16[2..<416]))
        reader.reader.close(); model.close()
    }
}
