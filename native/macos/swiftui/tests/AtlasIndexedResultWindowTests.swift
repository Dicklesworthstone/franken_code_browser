// Production worker, window decoder, source capabilities and mode/refresh
// lifecycle. Scripted engine envelopes do not qualify Rust or physical Macs.
import Foundation

@main private enum AtlasIndexedResultWindowTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ value: @autoclosure () -> Bool, _ label: String) {
        precondition(value(), label); checks += 1
    }
    private static func firstInput(countAll: Bool = true, mode: AtlasSearchMode = .text,
                                   caseSensitive: Bool = true,
                                   nameMode: AtlasSearchNameMode = .contains) throws -> AtlasSearchInput {
        try AtlasSearchInput(root: "/repo", query: "x", mode: mode,
            nameMode: nameMode, caseSensitive: caseSensitive, countAllMatches: countAll)
    }
    private static func input(_ request: AtlasSearchResultWindowRequest) throws -> AtlasSearchInput {
        let source = request.source
        return try AtlasSearchInput(root: source.root, query: source.query, fileScope: source.fileScope,
            pathScope: source.pathScope, mode: source.mode, nameMode: source.nameMode, caseSensitive: source.caseSensitive,
            countAllMatches: source.countAllMatches, discoveryPolicy: source.discoveryPolicy,
            catalogScope: source.catalogScope, resultWindow: request)
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
            let f = fixture(count: 2001)
            f.configure { $0.encoding = "utf16le"; $0.insensitiveText = "İ" }
            let worker = AtlasIndexedSearch(transport: f.matchingStructuralWindowTransport)
            let first = try run(worker, AtlasSearchInput(root: "/repo", query: "i\u{307}", mode: .occurrences,
                caseSensitive: false, countAllMatches: true))
            let next = try run(worker, input(first.resultWindow!.next!))
            let last = try run(worker, input(next.resultWindow!.next!))
            check(first.matchesSeen == 2001 && first.hits.count == 1000
                && next.hits.count == 1000 && last.hits.count == 1 && last.resultWindow?.next == nil,
                "Lowercase whole-token windows preserve exhaustive counts and bounded navigation rows")
            check(!last.caseSensitive && last.hits[0].structure?.name == "İ"
                && last.hits[0].start == 4000 && last.hits[0].end == 4002,
                "Later lowercase occurrence windows retain the original UTF-16 source coordinates")
            let target = last.capture!.target(last.hits[0])!
            check(target.needle == "İ" && target.occurrenceQuery?.utf8.elementsEqual("i\u{307}".utf8) == true,
                "Later-window activation separates matched spelling from the submitted token")
            _ = try target.openReader(100)
            let previous = try run(worker, input(last.resultWindow!.previous!))
            check(!previous.caseSensitive && previous.resultWindow?.offset == 1000
                && previous.hits[0].structure?.name == "İ" && f.count("build-begin:") == 1,
                "Previous lowercase windows reuse the same immutable capture and matching policy")
        }
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
        for mode in [AtlasSearchMode.symbols, .headings, .occurrences] {
            let f = fixture(), worker = AtlasIndexedSearch(transport: f.structuralWindowTransport)
            let first = try run(worker, firstInput(mode: mode, nameMode: .prefix))
            check(first.complete && first.matchCountComplete && first.truncated && first.matchesSeen == 3025
                && first.hits.count == 1000 && first.hits.first?.structure?.occurrence == 1,
                "Counted \(mode.rawValue) retains the first bounded evidence window and the complete total")
            let selected = first.capture!.target(first.hits[0])!
            let next = try run(worker, input(first.resultWindow!.next!))
            check(next.hits.first?.structure?.occurrence == 1001 && next.hits.last?.structure?.occurrence == 2000
                && next.resultWindow?.next?.offset == 2000 && next.resultWindow?.previous?.offset == 0,
                "Later \(mode.rawValue) windows preserve global occurrence identities and bounded row IDs")
            check(next.hits.first?.id == 0 && first.capture?.target(next.hits[0]) == nil
                && next.capture?.target(first.hits[0]) == nil && first.streamID != next.streamID,
                "Window-local structural row IDs cannot alias another window's source evidence")
            let back = try run(worker, input(next.resultWindow!.previous!))
            check(back.hits.first?.structure == first.hits.first?.structure
                && back.nameMode == mode.effectiveNameMode(.prefix),
                "Previous restores identical evidence with the frozen structural name policy")
            _ = try selected.openReader(100)
            check(f.count("create:") == 1 && f.count("build-step:") == 1 && f.count("source:") == 1,
                "Structural windows and previously selected readers use the original captures")
            let beyond = AtlasSearchResultWindowRequest(source: first.resultWindow!.source, offset: 4000)
            let empty = try run(worker, input(beyond))
            check(empty.complete && empty.hits.isEmpty && empty.matchesSeen == 3025
                && empty.resultWindow?.coveredMatches == 0 && empty.resultWindow?.next == nil,
                "A structural window past the total is explicitly empty")
            if mode.supportsNameMatching {
                do {
                    _ = try AtlasSearchInput(root: "/repo", query: "x", mode: mode, nameMode: .exact,
                        countAllMatches: true, resultWindow: first.resultWindow!.next!)
                    preconditionFailure("Structural continuation changed its name policy")
                } catch AtlasSearchError.invalidRequest { checks += 1 }
            }
        }
        do {
            let f = fixture(count: 9001)
            f.configure { $0.captureBytes = 32768; $0.encoding = "utf16le" }
            let worker = AtlasIndexedSearch(transport: f.structuralWindowTransport)
            let first = try run(worker, firstInput(mode: .occurrences))
            let later = AtlasSearchResultWindowRequest(source: first.resultWindow!.source, offset: 9000)
            let last = try run(worker, input(later))
            check(first.matchesSeen == 9001 && first.matchCountComplete && last.hits.count == 1
                && last.hits[0].structure?.occurrence == 9001 && last.hits[0].start == 18000
                && last.hits[0].end == 18002 && last.resultWindow?.next == nil,
                "Counted occurrences can cross the former per-file extraction ceiling with original UTF-16 ranges")
            let reader = try last.capture!.target(last.hits[0])!.openReader(100)
            check(reader.contains("utf16le"), "A later occurrence opens its immutable original encoding")
        }
        do {
            let f = fixture(count: 2001), worker = AtlasIndexedSearch(transport: f.structuralWindowTransport)
            let first = try run(worker, firstInput(countAll: false, mode: .headings))
            check(!first.complete && !first.matchCountComplete && first.matchesSeen == 1001
                && first.resultWindow?.next?.offset == 1000,
                "Noncounted structural windows stop after a verified lookahead")
            let middle = try run(worker, input(first.resultWindow!.next!))
            let last = try run(worker, input(middle.resultWindow!.next!))
            check(last.complete && last.hits.count == 1 && last.hits[0].structure?.occurrence == 2001
                && last.summary.contains("2001 headings") && last.summary.contains("2001–2001"),
                "The final bounded structural window can prove completion and report the full observed total")
        }
        for omitted in [7, 1000] {
            let f = fixture()
            f.configure { $0.structureMetadataOmitted = omitted }
            let worker = AtlasIndexedSearch(transport: f.structuralWindowTransport)
            let first = try run(worker, firstInput(mode: .symbols))
            check(!first.complete && !first.matchCountComplete && first.hits.count == 1000 - omitted
                && first.resultWindow?.coveredMatches == 1000 && first.resultWindow?.next?.offset == 1000
                && first.summary.contains("matching results omitted by metadata limits"),
                "Structural metadata omissions stay partial and advance by covered matches, including empty row windows")
            let second = try run(worker, input(first.resultWindow!.next!))
            check(second.resultWindow?.offset == 1000 && second.resultWindow?.next?.offset == 2000
                && (second.hits.first?.structure?.occurrence ?? 0) == (omitted == 1000 ? 0 : UInt64(1001 + omitted)),
                "Omitted metadata cannot repeat a prior structural window or trap Next on the same offset")
        }
        do {
            let f = fixture(files: 2), worker = AtlasIndexedSearch(transport: f.structuralWindowTransport)
            let accepted = try run(worker, firstInput(mode: .symbols))
            let selected = accepted.capture!.target(accepted.hits[0])!
            let flag = AtlasSearchCancellation()
            do {
                _ = try run(worker, input(accepted.resultWindow!.next!), flag: flag) { report in
                    if !report.hits.isEmpty { flag.cancel() }
                }
                preconditionFailure("Canceled structural window was delivered")
            } catch AtlasSearchError.canceled { checks += 1 }
            _ = try selected.openReader(100)
            let retry = try run(worker, input(accepted.resultWindow!.next!))
            check(retry.hits.first?.structure?.occurrence == 1001 && accepted.hits.first?.structure?.occurrence == 1
                && f.count("create:") == 1 && f.count("build-step:") == 2,
                "Canceling a structural continuation preserves accepted results and reader authority without recapture")
        }
        for defect in ["covered", "missing-covered", "omitted", "missing-occurrence", "zero-occurrence",
                       "past-window", "duplicate", "name-mode", "count-policy", "metadata-drift"] {
            let f = fixture(), worker = AtlasIndexedSearch(transport: f.structuralWindowTransport)
            let accepted = try run(worker, firstInput(mode: .symbols))
            f.patch { phase, original in
                var wire = original
                guard ["query-begin", "query-step", "page"].contains(phase) else { return wire }
                switch defect {
                case "covered": wire["window_matches"] = "1001"
                case "missing-covered": wire.removeValue(forKey: "window_matches")
                case "omitted": wire["structure_metadata_omitted"] = "1"
                case "name-mode": wire["name_mode"] = "exact"
                case "count-policy": wire["count_all_matches"] = false
                default:
                    guard var rows = wire["hits"] as? [[String: Any]], !rows.isEmpty else { return wire }
                    let at = defect == "duplicate" ? 1 : 0
                    var structure = rows[at]["structure"] as! [String: Any]
                    switch defect {
                    case "missing-occurrence": structure.removeValue(forKey: "occurrence_id")
                    case "zero-occurrence": structure["occurrence_id"] = "0"
                    case "past-window": structure["occurrence_id"] = "2001"
                    case "duplicate": structure["occurrence_id"] = (rows[0]["structure"] as! [String: Any])["occurrence_id"]
                    default: if phase == "page" { structure["occurrence_id"] = "1001" }
                    }
                    rows[at]["structure"] = structure; wire["hits"] = rows
                }
                return wire
            }
            do { _ = try run(worker, input(accepted.resultWindow!.next!))
                preconditionFailure("Invalid structural window accepted: " + defect) }
            catch AtlasSearchError.invalidResponse { checks += 1 }
            f.patch { _, wire in wire }
            _ = try accepted.capture!.target(accepted.hits[0])!.openReader(100)
            check(accepted.resultWindow?.offset == 0 && accepted.hits[0].structure?.occurrence == 1,
                "Rejected structural \(defect) reply preserves accepted evidence and reader ownership")
        }
        do {
            let f = IndexedFixture(), worker = AtlasIndexedSearch(transport: f.structuralTransport)
            do { _ = try run(worker, firstInput(mode: .symbols))
                preconditionFailure("Legacy structural transport discarded explicit counting") }
            catch AtlasSearchError.unsupportedCounting { checks += 1 }
            check(f.count("create:") == 0, "Unsupported structural counting fails before discovery or capture")
            let legacy = try run(worker, firstInput(countAll: false, mode: .symbols))
            check(legacy.complete && legacy.resultWindow == nil && legacy.hits.first?.structure?.occurrence == nil,
                "Legacy structural transports retain their existing bounded evidence contract")
        }
        print("AtlasIndexedResultWindowTests: \(checks) checks passed")
    }
}
