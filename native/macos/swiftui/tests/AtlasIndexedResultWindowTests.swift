// Production worker, window decoder, source capabilities and mode/refresh
// lifecycle. Scripted engine envelopes do not qualify Rust or physical Macs.
import Foundation

@main private enum AtlasIndexedResultWindowTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ value: @autoclosure () -> Bool, _ label: String) {
        precondition(value(), label); checks += 1
    }
    private static func firstInput(countAll: Bool = true, mode: AtlasSearchMode = .text,
                                   caseSensitive: Bool = true) throws -> AtlasSearchInput {
        try AtlasSearchInput(root: "/repo", query: "x", mode: mode,
            caseSensitive: caseSensitive, countAllMatches: countAll)
    }
    private static func input(_ request: AtlasSearchResultWindowRequest) throws -> AtlasSearchInput {
        let source = request.source
        return try AtlasSearchInput(root: source.root, query: source.query, fileScope: source.fileScope,
            pathScope: source.pathScope, mode: source.mode, caseSensitive: source.caseSensitive,
            countAllMatches: source.countAllMatches, discoveryPolicy: source.discoveryPolicy, resultWindow: request)
    }
    private static func run(_ worker: AtlasIndexedSearch, _ input: AtlasSearchInput,
                            grant: AnyObject? = nil, flag: AtlasSearchCancellation = AtlasSearchCancellation(),
                            progress: @Sendable (AtlasSearchReport) -> Void = { _ in }) throws -> AtlasSearchReport {
        try worker.runScoped(input: input, cancellation: flag, access: AtlasSearchAccessLease(grant), progress: progress)
    }
    private static func fixture(files: Int = 1, count: Int = 3025) -> IndexedFixture {
        let fixture = IndexedFixture()
        fixture.configure { $0.files = files; $0.hitCount = count; $0.captureBytes = 8192 }
        return fixture
    }

    @MainActor static func main() throws {
        do {
            let f = fixture(), worker = AtlasIndexedSearch(transport: f.windowTransport)
            let first = try run(worker, firstInput())
            check(first.complete && first.matchCountComplete && first.matchesSeen == 3025
                && first.hits.count == 1000, "The first window keeps a complete count and bounded witnesses")
            check(first.resultWindow?.previous == nil && first.resultWindow?.next?.offset == 1000,
                "Only a proven following occurrence enables Next")
            let selected = first.capture!.target(first.hits[999])!
            let second = try run(worker, input(first.resultWindow!.next!))
            let third = try run(worker, input(second.resultWindow!.next!))
            let last = try run(worker, input(third.resultWindow!.next!))
            check(second.hits.first?.start == 2000 && third.hits.first?.start == 4000
                && last.hits.first?.start == 6000 && last.hits.count == 25,
                "Later windows preserve original ranges beyond the former first-1000 ceiling")
            check(last.complete && last.truncated && last.resultWindow?.next == nil
                && last.resultWindow?.previous?.offset == 2000,
                "An exhaustive last window distinguishes preceding omissions from later matches")
            check(last.summary.contains("3025 exact matches") && last.summary.contains("3026") == false
                && last.summary.contains("3001–3025"), "The final count and displayed ordinal range are independent")
            check(first.streamID != second.streamID && second.streamID != third.streamID,
                "Window-local hit IDs always receive distinct presentation identities")
            check(first.capture?.target(second.hits[0]) == nil && second.capture?.target(first.hits[0]) == nil,
                "A reused row ordinal cannot lend another window's exact witness")
            let context = AtlasSearchContext(root: "/repo", query: "x", loadGeneration: UUID(),
                atlasRevision: UUID(), scope: "All", customExtensions: "", countAllMatches: true)
            let old = AtlasSearchPresentation(context: context, hits: first.hits, verifiedHitIDs: [],
                id: first.streamID!)
            let current = AtlasSearchPresentation(context: context, hits: second.hits, verifiedHitIDs: [],
                id: second.streamID!)
            check(current.captureCandidate(for: old.rowID(for: 0), in: context) == nil,
                "A delayed selection from the preceding window cannot activate current row zero")
            let back = try run(worker, input(second.resultWindow!.previous!))
            check(back.hits.first?.start == 0 && back.hits.last?.start == 1998
                && back.resultWindow?.offset == 0 && back.resultWindow?.previous == nil,
                "Previous revisits the same exact ordered captures")
            _ = try selected.openReader(100)
            check(f.count("create:") == 1 && f.count("open:") == 1 && f.count("build-step:") == 1
                && f.count("source:") == 1, "All windows and an earlier selected source reuse one retained capture")
        }
        for mode in [AtlasSearchMode.text, .expression] {
            let f = fixture(count: 2001)
            f.configure { $0.encoding = "utf16le"; $0.insensitiveRangeBytes = 2 }
            let worker = AtlasIndexedSearch(transport: f.windowTransport)
            let first = try run(worker, firstInput(countAll: false, mode: mode, caseSensitive: false))
            check(!first.complete && !first.matchCountComplete && first.matchesSeen == 1001
                && first.resultWindow?.next?.offset == 1000,
                "Bounded noncounted \(mode.rawValue) verifies a lookahead without claiming an exhaustive total")
            let second = try run(worker, input(first.resultWindow!.next!))
            let last = try run(worker, input(second.resultWindow!.next!))
            check(last.hits.count == 1 && last.matchesSeen == 2001 && last.complete && last.matchCountComplete
                && last.resultWindow?.next == nil && last.caseSensitive == false && last.mode == mode,
                "Final noncounted \(mode.rawValue) can prove completion despite preceding omitted rows")
            check(last.hits[0].actualText == "X" && last.capture!.target(last.hits[0])!.needle == "X"
                && last.hits[0].end - last.hits[0].start == 2,
                "Insensitive navigation retains source spelling and original UTF-16 byte widths on every window")
            let reader = try last.capture!.target(last.hits[0])!.openReader(100)
            check(reader.contains("utf16le"), "Later witnesses retain the original reader encoding")
        }
        do {
            let f = fixture(count: 1000), worker = AtlasIndexedSearch(transport: f.windowTransport)
            let exact = try run(worker, firstInput(countAll: false))
            check(exact.complete && exact.hits.count == 1000 && exact.resultWindow?.next == nil,
                "An exactly full final window does not invent another page")
            let beyond = AtlasSearchResultWindowRequest(source: exact.resultWindow!.source, offset: 2000)
            let empty = try run(worker, input(beyond))
            check(empty.hits.isEmpty && empty.matchesSeen == 1000 && empty.resultWindow?.skipped == 1000
                && empty.resultWindow?.offset == 2000 && empty.resultWindow?.next == nil && empty.complete,
                "An overshoot is explicitly empty; it never aliases the last nonempty page")
        }
        do {
            let f = fixture(files: 2), worker = AtlasIndexedSearch(transport: f.windowTransport)
            let accepted = try run(worker, firstInput())
            let selected = accepted.capture!.target(accepted.hits[0])!
            let flag = AtlasSearchCancellation()
            do {
                _ = try run(worker, input(accepted.resultWindow!.next!), flag: flag) { report in
                    if !report.hits.isEmpty { flag.cancel() }
                }
                preconditionFailure("Canceled later window was delivered")
            } catch AtlasSearchError.canceled { checks += 1 }
            _ = try selected.openReader(100)
            let retried = try run(worker, input(accepted.resultWindow!.next!))
            check(retried.resultWindow?.offset == 1000 && f.count("create:") == 1
                && f.count("build-step:") == 2 && accepted.hits.first?.start == 0,
                "Canceled replacement keeps accepted witnesses and retries without recapturing")
        }
        do {
            let f = fixture(), worker = AtlasIndexedSearch(transport: f.windowTransport)
            let accepted = try run(worker, firstInput())
            let request = accepted.resultWindow!.next!
            f.configure { $0.digest = String(repeating: "b", count: 64); $0.hitCount = 1 }
            let stillCaptured = try run(worker, input(request))
            check(stillCaptured.hits.count == 1000 && stillCaptured.hits[0].captureSHA256 == accepted.hits[0].captureSHA256,
                "Changed live-fixture state cannot replace a retained window's capture")
            let fresh = AtlasIndexedSearch(transport: f.windowTransport)
            let calls = f.count("create:")
            do { _ = try run(fresh, input(request)); preconditionFailure("Window prepared an unrelated index") }
            catch AtlasSearchError.unavailable { checks += 1 }
            check(f.count("create:") == calls, "A missing immutable index fails before discovery or source reads")
            let changedGrant = NSObject()
            do { _ = try run(worker, input(request), grant: changedGrant); preconditionFailure("Window crossed a root grant") }
            catch AtlasSearchError.unavailable { checks += 1 }
            check(f.count("create:") == calls, "A changed native grant cannot trigger a silent recapture")
            for changed in ["query", "case", "count", "scope", "policy"] {
                do {
                    _ = try AtlasSearchInput(root: "/repo", query: changed == "query" ? "y" : "x",
                        pathScope: changed == "scope" ? AtlasSearchPathScope(kind: .file, path: "a.rs") : .workspace,
                        caseSensitive: changed != "case", countAllMatches: changed != "count",
                        discoveryPolicy: changed == "policy" ? .includeExcluded : .repositoryRules,
                        resultWindow: request)
                    preconditionFailure("Window accepted changed " + changed)
                } catch AtlasSearchError.invalidRequest { checks += 1 }
            }
        }
        do {
            let f = fixture(), service = AtlasWorkspaceSearch(scopedLive: { _, _, _, _ in
                preconditionFailure("Captured continuation reached live search")
            }, makeIndex: { AtlasIndexedSearch(transport: f.windowTransport) })
            let choice = service.selection(indexed: true)
            let first = try choice.scopedWork(firstInput(), AtlasSearchCancellation(), AtlasSearchAccessLease(nil), { _ in })
            let request = try input(first.resultWindow!.next!)
            service.refresh()
            check(!service.accepts(choice), "Refresh immediately revokes delivery of the earlier index choice")
            let fresh = service.selection(indexed: true), before = f.count("create:")
            do { _ = try fresh.scopedWork(request, AtlasSearchCancellation(), AtlasSearchAccessLease(nil), { _ in })
                preconditionFailure("Refreshed service recaptured a stale window") }
            catch AtlasSearchError.unavailable { checks += 1 }
            check(f.count("create:") == before, "A refreshed owner refuses before creating foreign work")
            let live = service.selection(indexed: false)
            do { _ = try live.scopedWork(request, AtlasSearchCancellation(), AtlasSearchAccessLease(nil), { _ in })
                preconditionFailure("Window fell back to live search") }
            catch AtlasSearchError.unavailable { checks += 1 }
        }
        do {
            let f = fixture(files: 2, count: 1500)
            f.configure { $0.scopedEligible = 1; $0.scopedCaptured = 1; $0.scopedHitCount = 1500
                $0.pathHex = "7372632f612e7273" }
            let worker = AtlasIndexedSearch(transport: f.windowTransport)
            let scope = try AtlasSearchFileScope(extensions: ["rs"])
            let directory = try AtlasSearchPathScope(kind: .directory, path: "src")
            let first = try run(worker, AtlasSearchInput(root: "/repo", query: "x",
                fileScope: scope, pathScope: directory, countAllMatches: true))
            let next = try run(worker, input(first.resultWindow!.next!))
            check(next.fileScope == scope && next.pathScope == directory && next.excludedFiles == 1
                && next.hits.count == 500 && next.complete,
                "Result windows retain their exact type and directory scope")
            check(f.count("capture-scope:") == 1 && f.count("query-path:directory:737263") == 2,
                "Changing result windows never broadens or rebuilds the capture scope")
        }
        for defect in ["offset", "skipped", "more", "next", "missing", "null", "drift", "source-io", "generation"] {
            let f = fixture(), worker = AtlasIndexedSearch(transport: f.windowTransport)
            let accepted = try run(worker, firstInput())
            f.patch { phase, original in
                var wire = original
                guard ["query-begin", "query-step", "page"].contains(phase) else { return wire }
                switch defect {
                case "offset": wire["result_offset"] = "0"
                case "skipped": wire["skipped_matches"] = "1001"
                case "more": wire["has_more_matches"] = true
                case "next": wire["next_match_offset"] = "2001"
                case "missing": wire.removeValue(forKey: "next_match_offset")
                case "null": wire["result_offset"] = NSNull()
                case "drift": if phase == "page" { wire["skipped_matches"] = "999" }
                case "source-io": wire["source_bytes_read"] = "1"
                default: wire["query_generation"] = "2"
                }
                return wire
            }
            do { _ = try run(worker, input(accepted.resultWindow!.next!))
                preconditionFailure("Invalid window accepted: " + defect) }
            catch AtlasSearchError.invalidResponse { checks += 1 }
            check(accepted.hits.first?.start == 0 && accepted.resultWindow?.offset == 0,
                "Rejected \(defect) reply cannot mutate the accepted result window")
            f.patch { _, wire in wire }
            _ = try accepted.capture!.target(accepted.hits[0])!.openReader(100)
        }
        do {
            let f = fixture()
            f.configure { $0.discovery = false }
            let partial = try run(AtlasIndexedSearch(transport: f.windowTransport), firstInput())
            check(!partial.complete && !partial.matchCountComplete && partial.resultWindow?.next != nil
                && partial.summary.contains("At least 3025") && partial.summary.contains("coverage is partial"),
                "Known following matches stay navigable without upgrading partial membership to complete")
            let legacy = try run(AtlasIndexedSearch(transport: IndexedFixture().countedTransport), firstInput())
            check(legacy.resultWindow == nil, "Older transports keep usable bounded results without unsupported continuation")
        }
        print("AtlasIndexedResultWindowTests: \(checks) checks passed")
    }
}
