import Foundation
import Dispatch

/// Fixed producer vectors exercise the production decoder, saved model and
/// reader handoff. They do not implement a second path matcher or ranker; the
/// Rust shared-engine fixtures own matching, ranking and metadata-only I/O.
private final class SavedPathFixture: @unchecked Sendable {
    struct Query: Sendable {
        let generation: UInt64
        let bytes: [UInt8]
        var mode: AtlasSavedPathMode = .fuzzy
        var caseSensitive = false
        var results: UInt64 = 256
        var work: UInt64 = AtlasSavedPathWire.defaultWork
        var required: UInt64 = 1000
        var complete: Bool { work >= required }
    }
    let base = SavedRepositoryFixture(searchVectors: true)
    let entered = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var accepted: Query?
    private var attempt: UInt64 = 0
    private var required: UInt64 = 1000
    private var fault: String?
    private var blocked = false
    private var requests: [Query] = []
    private var operations: [String] = []
    private func locked<T>(_ body: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return body() }
    var searches: [Query] { locked { requests } }
    var calls: [String] { locked { operations } }
    func corrupt(_ value: String) { locked { fault = value } }
    func blockNextSearch() { locked { blocked = true } }
    func requireMoreWork(_ enabled: Bool) {
        locked { required = enabled ? AtlasSavedPathWire.defaultWork * 2 : 1000 }
    }
    func object(_ json: String?) -> [String: Any] {
        try! JSONSerialization.jsonObject(with: Data(json!.utf8)) as! [String: Any]
    }
    private func vector(_ query: Query) -> [(UInt64, String)] {
        if query.bytes == Array("rs".utf8) { return [(3, "filename-subsequence"), (1, "filename-subsequence")] }
        if query.bytes == Array("raw".utf8) { return [(1, "filename-prefix")] }
        if query.bytes == [255] { return [(1, "filename-subsequence")] }
        if query.bytes == Array("lib.rs".utf8) { return [(3, "exact-filename")] }
        if query.bytes == Array("unavailable".utf8) { return [(4, "filename-prefix")] }
        if query.bytes == Array("md".utf8) {
            return [(0, "filename-subsequence"), (2, "filename-subsequence"), (4, "filename-subsequence")]
        }
        return []
    }
    private func observed(_ query: Query) -> [(UInt64, String)] {
        if query.complete { return vector(query) }
        if query.work == 0 { return [] }
        // A bounded pass saw the earlier raw-name member; the globally better
        // lib.rs row arrives only after searching the rest of the directory.
        return query.bytes == Array("rs".utf8) ? [(1, "filename-subsequence")] : Array(vector(query).prefix(1))
    }
    func page(_ owner: UInt64, query: Query, start: UInt64 = 0, limit: UInt64 = 64) -> [String: Any] {
        let seen = observed(query)
        let retained = Array(seen.prefix(Int(query.results)))
        let rows = retained.enumerated().dropFirst(Int(start)).prefix(Int(limit)).map { position, item in
            var row = base.memberObject(item.0)
            row["hit_id"] = String(position + 1); row["match_kind"] = item.1
            return row
        }
        let examined: UInt64 = query.complete ? 5 : (query.work == 0 ? 0 : 2)
        var out = base.header(owner, "paths")
        out.merge([
            "selection_namespace": "saved-paths", "path_generation": String(query.generation),
            "query_hex": base.hex(query.bytes), "mode": query.mode.rawValue,
            "case": query.caseSensitive ? "sensitive" : "unicode-lowercase", "key_version": "1",
            "metadata_scope": "archive-directory", "identity_scope": "retained-saved-query",
            "ordering": "ranked-archive-paths", "unicode_normalization": "none",
            "source_payload_read": false, "live_roots_accessed": false, "persistent_index_used": false,
            "retained_source_payload_bytes": "0", "member_bytes_read": "0", "member_read_calls": "0",
            "search_complete": query.complete, "matches_seen_exact": query.complete, "top_k_complete": query.complete,
            "truncated": seen.count > retained.count, "work_limited": !query.complete,
            "state": query.complete ? "complete" : "work-limit", "max_results": String(query.results),
            "max_work_units": String(query.work), "matches_seen": String(seen.count), "retained_hits": String(retained.count),
            "files_examined": String(examined), "candidates_examined": String(examined),
            "work_units": String(query.complete ? query.required : query.work),
            "metadata_path_bytes": String(base.members.reduce(0) { $0 + $1.path.count }),
            "offset": String(start), "limit": String(limit), "hits": Array(rows),
            "next_offset": start + UInt64(rows.count) < UInt64(retained.count)
                ? String(start + UInt64(rows.count)) as Any : NSNull()
        ]) { _, next in next }
        return out
    }
    var transport: AtlasSavedTransport {
        let saved = base.savedTransport
        return AtlasSavedTransport(create: saved.create, run: { [self] owner, request in
            if case .pathSearch = request {
                let pause = locked { let pause = blocked; blocked = false; return pause }
                if pause {
                    entered.signal()
                    precondition(resume.wait(timeout: .now() + .seconds(5)) == .success)
                }
            }
            switch request {
            case .open, .info:
                guard let reply = saved.run(owner, request) else { return nil }
                return locked {
                    if case .info = request, fault == "info-lost" { fault = nil; return nil }
                    var out = object(reply)
                    out["accepted_path_generation"] = accepted.map { String($0.generation) as Any } ?? NSNull()
                    out["last_path_attempt"] = String(attempt)
                    return base.encode(out)
                }
            case .pathSearch(let generation, let bytes, let mode, let sensitive, let results, let work):
                return locked {
                    let query = Query(generation: generation, bytes: bytes, mode: mode, caseSensitive: sensitive,
                        results: results, work: work, required: required)
                    requests.append(query)
                    guard generation > attempt else { return nil }; attempt = generation
                    if fault == "refused" { fault = nil; return nil }
                    accepted = query
                    if fault == "lost" { fault = nil; return nil }
                    if fault == "unknown" { fault = "info-lost"; return nil }
                    return base.encode(page(owner, query: query))
                }
            case .pathResults(let generation, let start, let limit):
                return locked {
                    guard let query = accepted, query.generation == generation else { return nil }
                    operations.append("page:\(generation):\(start)")
                    return base.encode(page(owner, query: query, start: start, limit: limit))
                }
            case .clearPaths(let generation):
                return locked {
                    guard generation > attempt else { return nil }; attempt = generation; accepted = nil
                    operations.append("clear:\(generation)")
                    if fault == "clear-lost" { fault = nil; return nil }
                    var out = base.header(owner, "paths-clear")
                    out["selection_namespace"] = "saved-paths"; out["path_generation"] = String(generation)
                    out["accepted_path_generation"] = NSNull(); out["retained_hits"] = "0"
                    return base.encode(out)
                }
            case .openPathHit(let reader, let generation, let hit):
                guard let query = locked({ accepted }), generation == query.generation, hit > 0 else { return nil }
                let retained = Array(observed(query).prefix(Int(query.results)))
                guard hit <= UInt64(retained.count) else { return nil }
                let selected = retained[Int(hit - 1)].0
                guard base.members[Int(selected)].bytes != nil else { return nil }
                let corrupt = locked { let value = fault; fault = nil; return value }
                let ordinal: UInt64 = corrupt == "reader-other" ? (selected == 3 ? 1 : 3) : selected
                guard let reply = saved.run(owner, .openMember(reader: reader, member: ordinal)) else { return nil }
                return locked {
                    operations.append("open:\(generation):\(hit):\(selected)")
                    if corrupt == "reader-lost" { return nil }
                    var out = object(reply)
                    if corrupt == "reader-selection" { out["selection"] = ["start": "0", "end": "1"] }
                    return base.encode(out)
                }
            default: return saved.run(owner, request)
            }
        }, cancel: saved.cancel, close: saved.close, retirementFailed: saved.retirementFailed, readerDomain: base.domain)
    }
}

@main private enum AtlasSavedPathTests {
    @MainActor private static func wait(_ message: String, _ ready: () -> Bool) async {
        for _ in 0..<2000 { if ready() { return }; try? await Task.sleep(nanoseconds: 1_000_000) }
        preconditionFailure(message)
    }
    private static func reject(_ work: () throws -> Void) {
        do { try work(); preconditionFailure("invalid saved path receipt accepted") } catch {}
    }
    @MainActor static func main() async throws {
        let f = SavedPathFixture()
        let browser = AtlasSavedRepositoryModel(transport: f.transport)
        browser.open(path: "/saved/repository.fcbs", accessLease: nil)
        await wait("saved archive open") { !browser.busy }
        let identity = browser.info!.identity
        let query = SavedPathFixture.Query(generation: 1, bytes: Array("rs".utf8))
        let decode: ([String: Any]) throws -> AtlasSavedPaths = {
            try AtlasSavedPaths.decode(f.base.encode($0), identity: identity, generation: 1,
                query: query.bytes, mode: .fuzzy, caseSensitive: false,
                maxResults: 256, maxWorkUnits: query.work, start: 0, limit: 64)
        }
        let ranked = try decode(f.page(identity.owner, query: query))
        precondition(ranked.hits.map(\.member.id) == [3, 1], "ranked IDs must not be treated as member ordinals")
        precondition(ranked.complete && !ranked.truncated && !ranked.canSearchMore)
        for (field, value) in [("owner", "99" as Any), ("path_generation", "2"), ("query_hex", "726177"),
            ("source_payload_read", true), ("member_read_calls", "1"), ("retained_source_payload_bytes", "1"),
            ("persistent_index_used", true), ("matches_seen_exact", false), ("top_k_complete", false),
            ("work_limited", true), ("files_examined", "6"), ("candidates_examined", "4"),
            ("matches_seen", "6"), ("truncated", true), ("next_offset", "1"), ("limit", "63")] {
            var invalid = f.page(identity.owner, query: query); invalid[field] = value
            reject { _ = try decode(invalid) }
        }
        for (field, value) in [("hit_id", "2" as Any), ("file_id", "99"),
            ("member", "5"), ("match_kind", "compiler-symbol"), ("source_digest", "wrong")] {
            var invalid = f.page(identity.owner, query: query)
            var rows = invalid["hits"] as! [[String: Any]]; rows[0][field] = value; invalid["hits"] = rows
            reject { _ = try decode(invalid) }
        }
        var duplicate = f.page(identity.owner, query: query)
        var rows = duplicate["hits"] as! [[String: Any]]
        rows[1] = rows[0]; rows[1]["hit_id"] = "2"; duplicate["hits"] = rows
        reject { _ = try decode(duplicate) }
        var short = f.page(identity.owner, query: query)
        short["hits"] = [(short["hits"] as! [[String: Any]])[0]]; short["next_offset"] = "1"
        let shortPage = try decode(short)
        precondition(shortPage.next == 1, "response-size pages use actual emitted row count")

        let rawQuery = SavedPathFixture.Query(generation: 1, bytes: [255])
        let raw = try AtlasSavedPaths.decode(f.base.encode(f.page(identity.owner, query: rawQuery)),
            identity: identity, generation: 1, query: [255], start: 0, limit: 64)
        precondition(raw.query == [255] && raw.hits[0].member.path.bytes == f.base.rawPath)
        var names = f.page(identity.owner, query: .init(generation: 1, bytes: Array("md".utf8)))
        names["discovery_complete"] = false
        let historical = try JSONDecoder().decode(AtlasSavedIdentity.self, from: Data(f.base.encode(names).utf8))
        let closedNames = try AtlasSavedPaths.decode(f.base.encode(names), identity: historical, generation: 1, start: 0, limit: 64)
        precondition(closedNames.complete && !closedNames.identity.discoveryComplete && closedNames.identity.captured < closedNames.identity.members)
        precondition(closedNames.hits.last?.member.captured == false && closedNames.hits.last?.member.reason == "READ_DENIED")

        let retainedCaps: [UInt64] = [0, 1]
        for cap in retainedCaps {
            var limited = query; limited.results = cap
            let page = try AtlasSavedPaths.decode(f.base.encode(f.page(identity.owner, query: limited)),
                identity: identity, generation: 1, start: 0, limit: 64)
            precondition(page.complete && page.truncated && page.seen == 2 && page.retained == cap && !page.canSearchMore)
        }
        var zeroWork = query; zeroWork.work = 0
        let incomplete = try AtlasSavedPaths.decode(f.base.encode(f.page(identity.owner, query: zeroWork)),
            identity: identity, generation: 1, start: 0, limit: 64)
        precondition(!incomplete.complete && incomplete.workLimited && incomplete.seen == 0 && incomplete.canSearchMore)
        precondition(incomplete.expandedWork == AtlasSavedPathWire.defaultWork)
        var exhausted = query; exhausted.work = AtlasSavedPathWire.maximumWork; exhausted.required = exhausted.work + 1
        let stopped = try AtlasSavedPaths.decode(f.base.encode(f.page(identity.owner, query: exhausted)),
            identity: identity, generation: 1, start: 0, limit: 64)
        precondition(stopped.workLimited && !stopped.canSearchMore && stopped.expandedWork == AtlasSavedPathWire.maximumWork)

        browser.pathInput = "rs"; browser.searchPaths(); await wait("ranked filename search") { !browser.busy }
        let first = browser.pathPage!, clicked = first.hits[0]
        browser.selectPathHit(clicked, from: first)
        precondition(browser.selectedMember?.id == 3)
        let reader = try await browser.openPathHit(clicked, from: first, readerTransport: f.base.readerTransport())
        precondition(reader.member.id == 3 && reader.reader.page?.identity.encoding == "utf16be")
        precondition(f.calls.contains("open:1:1:3"), "the clicked ranked hit must resolve through its accepted query")
        let rawReader = try await browser.openPathHit(first.hits[1], from: first, readerTransport: f.base.readerTransport())
        precondition(rawReader.member.path.bytes == f.base.rawPath && rawReader.reader.page?.identity.pathBytes == f.base.rawPath)
        for fault in ["reader-other", "reader-selection", "reader-lost"] {
            f.corrupt(fault)
            do {
                _ = try await browser.openPathHit(clicked, from: first, readerTransport: f.base.readerTransport())
                preconditionFailure("invalid path reader accepted: \(fault)")
            } catch {}
            precondition(reader.reader.canPin && rawReader.reader.canPin)
        }
        let sourceLoads = f.base.calls.filter { $0.hasPrefix("handoff:") }.count
        let searches = f.searches.count
        browser.loadPaths(start: 1); await wait("retained name page") { !browser.busy }
        precondition(browser.pathPage?.hits.first?.id == 2 && f.searches.count == searches)
        precondition(f.base.calls.filter { $0.hasPrefix("handoff:") }.count == sourceLoads)
        browser.pathInput = "unavailable"; browser.searchPaths(); await wait("unavailable filename") { !browser.busy }
        let unavailable = browser.pathPage!, missing = unavailable.hits[0]
        browser.selectPathHit(missing, from: unavailable)
        precondition(browser.selectedMember?.id == 4 && !missing.member.captured)
        do {
            _ = try await browser.openPathHit(missing, from: unavailable, readerTransport: f.base.readerTransport())
            preconditionFailure("unavailable source opened")
        } catch {}
        precondition(f.base.calls.filter { $0.hasPrefix("handoff:") }.count == sourceLoads)
        browser.pathInput = "raw"; browser.searchPaths(); await wait("raw path filename") { !browser.busy }
        let rawPage = browser.pathPage!
        browser.pathMode = .exact
        precondition(!browser.pathSearchIsCurrent)
        do {
            _ = try await browser.openPathHit(rawPage.hits[0], from: rawPage, readerTransport: f.base.readerTransport())
            preconditionFailure("a result under a different mode navigated")
        } catch {}
        browser.pathMode = .fuzzy
        browser.searchInput = "ana"; browser.search(); await wait("independent source query") { !browser.busy }
        let sourceGeneration = browser.queryGeneration

        f.requireMoreWork(true)
        browser.pathInput = "rs"; browser.searchPaths(); await wait("work-limited filename search") { !browser.busy }
        let partial = browser.pathPage!
        precondition(partial.workLimited && partial.hits.first?.member.id == 1 && partial.canSearchMore)
        let beforeMore = f.searches.count
        browser.pathInput = "raw"; browser.searchMorePaths()
        precondition(f.searches.count == beforeMore, "Search more must bind to the visible frozen query")
        browser.pathInput = "rs"; browser.searchMorePaths(); await wait("expanded directory search") { !browser.busy }
        let expanded = browser.pathPage!, submitted = f.searches.last!
        precondition(expanded.complete && expanded.hits.map(\.member.id) == [3, 1])
        precondition(submitted.bytes == partial.query && submitted.mode == partial.mode
            && submitted.caseSensitive == partial.caseSensitive && submitted.work == partial.expandedWork)
        precondition(expanded.generation > partial.generation && browser.queryGeneration == sourceGeneration)
        do {
            _ = try await browser.openPathHit(partial.hits[0], from: partial, readerTransport: f.base.readerTransport())
            preconditionFailure("the previous partial ranking navigated after replacement")
        } catch {}
        f.requireMoreWork(false)
        f.corrupt("lost"); browser.searchPaths(); await wait("lost search reply reconciliation") { !browser.busy }
        let recovered = browser.pathGeneration!
        precondition(browser.pathSearchIsCurrent && browser.pathPage?.generation == recovered)
        f.corrupt("refused"); browser.searchPaths(); await wait("failed replacement reconciliation") { !browser.busy }
        precondition(browser.pathGeneration == recovered && browser.queryGeneration == sourceGeneration)
        f.blockNextSearch(); browser.searchPaths()
        await wait("filename worker entered") { f.entered.wait(timeout: .now()) == .success }
        let entering = f.searches.count
        browser.pathInput = "raw"; browser.cancel(); browser.searchPaths()
        precondition(browser.busy && f.searches.count == entering)
        f.resume.signal(); await wait("canceled filename worker drained") { !browser.busy }
        precondition(browser.pathGeneration! > recovered && browser.pathPage?.query == Array("rs".utf8))
        precondition(!browser.pathSearchIsCurrent && reader.reader.canPin && rawReader.reader.canPin)
        browser.pathInput = "rs"
        f.corrupt("unknown"); browser.searchPaths(); await wait("unknown accepted path generation") { !browser.busy }
        precondition(browser.pathPage == nil && browser.pathGeneration == nil && browser.queryGeneration == sourceGeneration)
        browser.reloadPaths(); await wait("recover retained path generation") { !browser.busy }
        precondition(browser.pathSearchIsCurrent && browser.pathPage?.complete == true)
        f.corrupt("clear-lost"); browser.clearPaths(); await wait("lost clear reconciliation") { !browser.busy }
        precondition(browser.pathPage == nil && browser.pathGeneration == nil && browser.queryGeneration == sourceGeneration)
        browser.searchPaths(); await wait("filename search before empty input") { !browser.busy }
        browser.pathInput = ""; browser.searchPaths(); await wait("empty input clears names") { !browser.busy }
        precondition(browser.pathPage == nil && browser.queryGeneration == sourceGeneration)
        browser.closeArchive()
        precondition(reader.reader.canPin && rawReader.reader.canPin && !f.base.calls.contains("LIVE-OPEN"))
        reader.reader.close(); rawReader.reader.close(); browser.close()
        print("AtlasSavedPathTests: raw names, metadata completeness, bounded rank pages, exact clicked members, search-more recovery and independent reader ownership passed")
    }
}
