// Full production worker, typed JSON decoder, captured-source handoff and
// original owner retirement. Scripted envelopes are NOT actual Rust execution.
import Foundation
import Dispatch

@main private enum AtlasIndexedSearchTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ value: @autoclosure () -> Bool, _ label: String) {
        precondition(value(), label); checks += 1
    }
    @MainActor private static func run(_ worker: AtlasIndexedSearch, query: String = "x", root: String = "/repo",
                                      access: AnyObject? = nil, flag: AtlasSearchCancellation = AtlasSearchCancellation(),
                                      reports: IndexReports = IndexReports(), caseSensitive: Bool = true,
                                      countAllMatches: Bool? = nil) throws -> AtlasSearchReport {
        try worker.runScoped(input: AtlasSearchInput(root: root, query: query, caseSensitive: caseSensitive,
            countAllMatches: countAllMatches),
            cancellation: flag, access: AtlasSearchAccessLease(access), progress: reports.offer)
    }
    @MainActor private static func scoped(_ worker: AtlasIndexedSearch, scope: AtlasSearchFileScope,
                                         pathScope: AtlasSearchPathScope = .workspace,
                                         flag: AtlasSearchCancellation = AtlasSearchCancellation()) throws -> AtlasSearchReport {
        try worker.runScoped(input: AtlasSearchInput(root: "/repo", query: "x", fileScope: scope, pathScope: pathScope),
            cancellation: flag, access: AtlasSearchAccessLease(nil), progress: { _ in })
    }
    @MainActor private static func wait(_ condition: () -> Bool) {
        let deadline = Date().addingTimeInterval(5)
        while !condition() && Date() < deadline { Thread.sleep(forTimeInterval: 0.001) }
        check(condition(), "Timed out awaiting deferred retirement")
    }
    @MainActor private static func reject(_ phase: String, _ key: String, value: String) throws {
        let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
        f.patch { p, object in var object = object; if p == phase { object[key] = value }; return object }
        do { _ = try run(w); preconditionFailure("Accepted bad \(phase).\(key)") }
        catch AtlasSearchError.invalidResponse { checks += 1 }
    }
    private static func withExcerpts(_ original: [String: Any], text: String = "x context") -> [String: Any] {
        var value = original
        if let rows = original["hits"] as? [[String: Any]] {
            value["hits"] = rows.map { originalRow in
                var row = originalRow
                let range = row["original_range"] as! [String: String]
                let start = UInt64(range["start"]!)!
                row["excerpt"] = ["text": text,
                    "original_range": ["start": String(start), "end": String(start + UInt64(text.utf8.count))],
                    "match_utf8_range": ["start": "0", "end": "1"],
                    "has_replacements": false, "clipped_start": start != 0, "clipped_end": true] as [String: Any]
                return row
            }
        }
        return value
    }
    @MainActor private static func structuralSearch() throws {
        func search(_ worker: AtlasIndexedSearch, _ mode: AtlasSearchMode,
                    nameMode: AtlasSearchNameMode = .contains,
                    scope: AtlasSearchFileScope = .all, pathScope: AtlasSearchPathScope = .workspace,
                    reports: IndexReports = IndexReports()) throws -> AtlasSearchReport {
            try worker.runScoped(input: AtlasSearchInput(root: "/repo", query: "x", fileScope: scope,
                pathScope: pathScope, mode: mode, nameMode: nameMode), cancellation: AtlasSearchCancellation(),
                access: AtlasSearchAccessLease(nil), progress: reports.offer)
        }
        do {
            let f = IndexedFixture(), worker = AtlasIndexedSearch(transport: f.structuralTransport)
            let text = try run(worker)
            let source = text.capture!.target(text.hits[0])!
            var selected: [AtlasSearchCapturedHit] = [source]
            for mode in [AtlasSearchMode.symbols, .headings, .occurrences] {
                let report = try search(worker, mode, nameMode: .prefix)
                check(report.complete && report.mode == mode && !report.countAllMatches,
                    "Each structural mode searches the same complete captured source universe")
                check(report.nameMode == mode.effectiveNameMode(.prefix)
                    && report.structureCoverage?.fileByteLimit == mode.structureFileByteLimit,
                    "Structural name policy and extraction bounds survive the report boundary")
                check(report.indexStatus?.reused == true && report.indexStatus?.verifiedFiles == 2
                    && report.indexStatus?.skippedFiles == 0 && report.indexStatus?.verificationBytes == 4196,
                    "Extraction visits captured sources without claiming trigram skips")
                check(report.hits.allSatisfy { $0.structure?.mode == mode && $0.actualText == nil },
                    "Structural rows preserve qualified evidence rather than text-match claims")
                check(report.resultLabel == mode.resultLabel && report.summary.contains("Captured index;"),
                    "Structural result labels and summaries retain the frozen-source distinction")
                let hit = report.hits[0]
                var forged = hit
                forged.structure = nil
                check(report.capture?.target(forged) == nil, "Removing structural evidence cannot forge activation")
                let target = report.capture!.target(hit)!
                check(target.mode == mode, "Captured activation retains structural mode")
                selected.append(target)
            }
            _ = try run(worker, query: "another query")
            for target in selected { _ = try target.openReader(100) }
            check(f.count("create:") == 1 && f.count("build-begin:") == 1 && f.count("build-step:") == 2
                && f.count("source:") == 4, "Mode edits reuse one index and all earlier source capabilities remain valid")
            check(f.count("query-structure:occurrences:exact") == 1,
                "Occurrence requests normalize the name policy to exact before the bridge")
        }
        do {
            let f = IndexedFixture(), worker = AtlasIndexedSearch(transport: f.transport)
            do { _ = try search(worker, .symbols); preconditionFailure("Missing structural capability accepted") }
            catch AtlasSearchError.unavailable { checks += 1 }
            check(f.count("create:") == 0, "Legacy transports refuse structural requests before any discovery or capture")
        }
        do {
            let f = IndexedFixture()
            f.configure { $0.hitCount = 200 }
            let worker = AtlasIndexedSearch(transport: f.structuralTransport)
            let reports = IndexReports()
            let report = try search(worker, .headings, nameMode: .exact, reports: reports)
            check(report.hits.count == 200 && f.count("page:") > 0
                && report.hits[199].structure?.candidate == 200,
                "Paged structural metadata and source witnesses survive bounded page delivery")
            check(reports.values.allSatisfy { $0.mode == .headings && $0.nameMode == .exact },
                "Index-build progress retains the requested mode and name policy")
        }
        do {
            let f = IndexedFixture()
            f.configure { $0.files = 3; $0.captureEligible = 1; $0.captureCaptured = 1
                $0.scopedEligible = 1; $0.scopedCaptured = 1; $0.scopedHitCount = 1 }
            let worker = AtlasIndexedSearch(transport: f.structuralTransport)
            _ = try search(worker, .symbols, pathScope: AtlasSearchPathScope(kind: .file, path: "a.rs"))
            let broader = try search(worker, .occurrences)
            check(!broader.complete && broader.indexStatus?.uncapturedFiles == 2 && broader.indexStatus?.reused == true,
                "A broader structural query reports uncaptured members instead of opening live paths")
            check(f.count("build-begin:") == 1, "Scope broadening leaves capture membership immutable")
        }
        do {
            let f = IndexedFixture()
            f.patch { phase, original in
                var value = original
                if phase == "query-step", value["step_count"] as? String == "2" {
                    value["scanned_files"] = "1"; value["verified_files"] = "1"
                    value["verification_source_bytes"] = "4096"
                    value["unsupported_structure_files"] = "1"; value["unavailable_files"] = "1"
                    value["search_complete"] = false; value["match_count_complete"] = false
                }
                return value
            }
            let report = try search(AtlasIndexedSearch(transport: f.structuralTransport), .symbols)
            check(!report.complete && report.unsupportedFiles == 1 && report.unavailableFiles == 1
                && report.hits.count == 2, "Unsupported captured members preserve useful rows with partial extraction coverage")
        }
        do {
            let f = IndexedFixture()
            f.configure { $0.files = 1; $0.hitCount = 0 }
            f.patch { phase, original in
                var value = original
                if phase == "query-step" {
                    value["scanned_files"] = "0"; value["verified_files"] = "0"
                    value["extraction_limited_files"] = "1"; value["unavailable_files"] = "1"
                    value["search_complete"] = false; value["match_count_complete"] = false
                }
                return value
            }
            let report = try search(AtlasIndexedSearch(transport: f.structuralTransport), .headings)
            check(!report.complete && report.structureCoverage?.extractionLimitedFiles == 1
                && report.indexStatus?.verifiedFiles == 0 && report.indexStatus?.verificationBytes == 4096,
                "Heading qualification failure preserves admitted work without claiming accepted extraction")
        }
        for defect in ["strategy", "io", "mode", "semantic", "name-policy", "range", "candidate", "missing-evidence", "repeated-evidence"] {
            let f = IndexedFixture()
            f.patch { phase, original in
                var value = original
                guard phase == "query-step" else { return value }
                switch defect {
                case "strategy": value["search_strategy"] = "retained-ephemeral-index"
                case "io": value["source_bytes_read"] = "1"
                case "mode": value["requested_mode"] = "headings"
                case "semantic": value["semantic_complete"] = true
                case "name-policy": value["name_mode"] = "exact"
                default:
                    var rows = value["hits"] as! [[String: Any]]
                    var structure = rows[0]["structure"] as! [String: Any]
                    if defect == "range" { structure["evidence_range"] = ["start": "1", "end": "1"] }
                    if defect == "candidate" { structure["candidate_id"] = "0" }
                    if defect == "repeated-evidence", value["step_count"] as? String == "2" { structure["name"] = "changed" }
                    rows[0]["structure"] = defect == "missing-evidence" ? NSNull() : structure
                    value["hits"] = rows
                }
                return value
            }
            do { _ = try search(AtlasIndexedSearch(transport: f.structuralTransport), .symbols)
                preconditionFailure("Invalid captured structural receipt accepted: " + defect) }
            catch AtlasSearchError.invalidResponse { checks += 1 }
        }
        do {
            let f = IndexedFixture()
            f.patch { phase, original in
                guard phase == "query-begin" else { return original }
                return ["schema": "fcb.atlas-search/1", "status": "error", "error": ["code": "REFERENCE_INVALID_NAME"]]
            }
            do { _ = try search(AtlasIndexedSearch(transport: f.structuralTransport), .occurrences)
                preconditionFailure("Invalid identifier query accepted") }
            catch AtlasSearchError.query(let code) { check(code == "REFERENCE_INVALID_NAME", "Shared token-policy query failures remain actionable") }
        }
    }

    @MainActor private static func countedSearch() throws {
        do {
            let f = IndexedFixture(), reports = IndexReports()
            f.configure { $0.hitCount = 1000; $0.countedMatches = 3000 }
            let w = AtlasIndexedSearch(transport: f.countedTransport)
            let first = try run(w, reports: reports)
            check(first.complete && first.truncated && first.countAllMatches && first.matchCountComplete,
                "An exhaustive count remains complete when retained results are capped")
            check(first.matchesSeen == 3000 && first.hits.count == 1000
                && first.hits.map(\.id) == Array(0..<1000), "Counted totals do not expand or renumber retained witnesses")
            check(first.summary.contains("3000 exact matches") && first.summary.contains("first 1000 retained"),
                "Completed indexed search presents the total separately from its retained rows")
            check(reports.values.contains { $0.isInProgress && $0.countAllMatches && !$0.matchCountComplete
                && $0.truncated && $0.matchesSeen == 3000 && $0.hits.count == 1000
                && $0.summary.contains("counted so far") }, "Progressive counts stay provisional past the row limit")
            let target = first.capture!.target(first.hits[999])!
            let next = try run(w)
            check(next.indexStatus?.reused == true && next.matchesSeen == 3000
                && next.streamID != first.streamID && f.count("build-step:") == 2 && f.count("open:") == 1,
                "Repeated full counts reuse the captured index without recapturing live files")
            _ = try target.openReader(100)
            check(f.count("source:") == 1 && f.count("query-counted:true") == 2,
                "Bounded retained witnesses still open their original source after another counted query")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.countedTransport)
            let insensitive = try run(w, caseSensitive: false)
            let expression = try w.runScoped(input: AtlasSearchInput(root: "/repo", query: "x", mode: .expression),
                cancellation: AtlasSearchCancellation(), access: AtlasSearchAccessLease(nil), progress: { _ in })
            check(!insensitive.countAllMatches && !expression.countAllMatches && f.count("query-counted:true") == 0,
                "Insensitive literals and expressions retain their existing bounded semantics")
            let legacy = try run(AtlasIndexedSearch(transport: IndexedFixture().transport))
            check(!legacy.countAllMatches && legacy.complete, "Legacy bounded transports remain supported")
        }
        do {
            let f = IndexedFixture()
            f.configure { $0.files = 3; $0.scopedEligible = 1; $0.scopedCaptured = 1
                $0.hitCount = 1000; $0.countedMatches = 3000 }
            var transport = f.countedTransport
            transport.scopedQueryBegin = nil; transport.pathScopedQueryBegin = nil
            let w = AtlasIndexedSearch(transport: transport)
            let path = try AtlasSearchPathScope(kind: .file, path: "a.rs")
            let scope = try AtlasSearchFileScope(extensions: ["rs"])
            let selected = try scoped(w, scope: scope, pathScope: path)
            check(selected.complete && selected.matchCountComplete && selected.matchesSeen == 3000
                && selected.fileScope == scope && selected.pathScope == path && selected.excludedFiles == 2,
                "The counted admission carries file-type and exact-path scopes without legacy scoped entry points")
            let broader = try run(w)
            check(!broader.complete && !broader.matchCountComplete && broader.countAllMatches
                && broader.matchesSeen == 3000 && broader.indexStatus?.uncapturedFiles == 2,
                "Widening a query beyond its original captures cannot claim a global total")
        }
        for reason in ["pending", "unavailable", "discovery", "verification"] {
            let f = IndexedFixture()
            f.configure { c in
                c.files = 3; c.hitCount = 1000; c.countedMatches = 3000
                if reason == "pending" { c.pending = 1 }
                if reason == "unavailable" { c.unavailable = 1 }
                if reason == "discovery" { c.discovery = false }
            }
            if reason == "verification" {
                f.patch { phase, original in
                    var value = original
                    if (phase == "query-step" || phase == "page"), value["step_count"] as? String == "1" {
                        value["search_in_progress"] = false; value["stop_reason"] = "verification-byte-limit"
                    }
                    return value
                }
            }
            let partial = try run(AtlasIndexedSearch(transport: f.countedTransport))
            check(partial.countAllMatches && !partial.complete && !partial.matchCountComplete
                && partial.truncated && partial.matchesSeen == 3000 && partial.hits.count == 1000
                && partial.summary.contains("total count is not complete"),
                "Incomplete \(reason) coverage cannot label its observed count as exhaustive")
        }
        do {
            let f = IndexedFixture(), flag = AtlasSearchCancellation(), reports = IndexReports()
            f.configure { $0.hitCount = 1000; $0.countedMatches = 3000 }
            let w = AtlasIndexedSearch(transport: f.countedTransport)
            do {
                _ = try w.run(root: "/repo", query: "x", cancellation: flag, access: AtlasSearchAccessLease(nil)) { report in
                    reports.offer(report)
                    if report.truncated { flag.cancel() }
                }
                preconditionFailure("Count continuation ignored cancellation")
            } catch AtlasSearchError.canceled { checks += 1 }
            check(f.count("query-step:") == 1 && reports.values.allSatisfy { !$0.matchCountComplete },
                "Canceling after useful retained rows suppresses any exhaustive-count claim or further work")
            let next = try run(w)
            check(next.matchCountComplete && next.indexStatus?.reused == true && f.count("create:") == 1
                && f.trace.contains { $0.hasPrefix("query-begin:") && $0.hasSuffix(":3") },
                "A canceled count consumes its generation while preserving the completed source index")
        }
        for defect in ["missing-policy", "missing-completion", "null-policy", "non-bool-policy", "wrong-policy",
                       "early-complete", "too-many-matches", "wrong-truncation", "wrong-stop", "match-limit",
                       "stale-generation", "page-count-drift", "page-missing-receipt", "decreasing-count", "false-total"] {
            let f = IndexedFixture()
            f.configure { $0.hitCount = 1000; $0.countedMatches = 3000 }
            f.patch { phase, original in
                var value = original
                if phase == "query-begin" {
                    switch defect {
                    case "missing-policy": value.removeValue(forKey: "count_all_matches")
                    case "missing-completion": value.removeValue(forKey: "match_count_complete")
                    case "null-policy": value["count_all_matches"] = NSNull()
                    case "non-bool-policy": value["count_all_matches"] = "true"
                    case "wrong-policy": value["count_all_matches"] = false
                    default: break
                    }
                }
                if phase == "query-step" {
                    switch defect {
                    case "early-complete": value["search_complete"] = true; value["match_count_complete"] = true
                    case "too-many-matches": value["matches_seen"] = "4197"
                    case "wrong-truncation": value["truncated"] = false
                    case "stale-generation": value["query_generation"] = "1"
                    default: break
                    }
                    if value["step_count"] as? String == "2" {
                        switch defect {
                        case "wrong-stop": value["stop_reason"] = "verification-byte-limit"
                        case "match-limit": value["stop_reason"] = "match-limit"
                        case "decreasing-count": value["matches_seen"] = "2999"
                        case "false-total": value["match_count_complete"] = false
                        default: break
                        }
                    }
                }
                if phase == "page" {
                    if defect == "page-count-drift" { value["matches_seen"] = "3001" }
                    if defect == "page-missing-receipt" {
                        value.removeValue(forKey: "count_all_matches"); value.removeValue(forKey: "match_count_complete")
                    }
                }
                return value
            }
            do { _ = try run(AtlasIndexedSearch(transport: f.countedTransport)); preconditionFailure("Accepted counted defect: " + defect) }
            catch AtlasSearchError.invalidResponse { checks += 1 }
        }
        do {
            let f = IndexedFixture()
            f.patch { phase, original in
                var value = original
                if phase == "query-begin" { value["count_all_matches"] = true; value["match_count_complete"] = false }
                return value
            }
            do { _ = try run(AtlasIndexedSearch(transport: f.transport)); preconditionFailure("Bounded request accepted counted receipt") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
        }
        #if canImport(CryptoKit)
        for falselyComplete in [false, true] {
            let f = IndexedFixture()
            f.configure { c in
                c.scopedEligible = 1; c.scopedCaptured = 1; c.captureVisibleCatalogued = 1
                c.captureVisibleMissing = 1; c.hitCount = 1000; c.countedMatches = 3000
            }
            if falselyComplete {
                f.patch { phase, original in
                    var value = original
                    if phase == "query-step" { value["search_complete"] = true; value["match_count_complete"] = true }
                    return value
                }
            }
            let region = try AtlasSearchPathScope(visiblePaths: [Array("a.rs".utf8), Array("missing.rs".utf8)])
            do {
                let partial = try scoped(AtlasIndexedSearch(transport: f.countedTransport), scope: .all, pathScope: region)
                check(!falselyComplete && !partial.matchCountComplete && partial.visibleMissingFiles == 1
                    && partial.countAllMatches && partial.matchesSeen == 3000,
                    "Missing visible membership prevents an exhaustive count even after all available captures were searched")
            } catch AtlasSearchError.invalidResponse {
                check(falselyComplete, "Only a false visible completeness receipt is rejected")
            }
        }
        #endif
    }
    @MainActor static func main() throws {
        try structuralSearch()
        try countedSearch()
        for caseSensitive in [true, false] {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.countedTransport)
            f.configure { $0.hitCount = 1000; $0.countedMatches = 2500 }
            let reports = IndexReports()
            let counted = try run(w, reports: reports, caseSensitive: caseSensitive, countAllMatches: true)
            check(counted.complete && counted.matchCountComplete && counted.countAllMatches && counted.truncated,
                "A complete count remains complete when retained navigation rows are capped")
            check(counted.matchesSeen == 2500 && counted.hits.count == 1000 && f.count("page:") > 0,
                "Totals exceed the row cap without retaining or fetching extra rows")
            check(counted.summary.contains("2500") && counted.summary.contains("first 1000 retained")
                && !counted.summary.contains("Partial search"),
                "The summary separates an exact total from the retained page limit")
            check(reports.values.filter { $0.indexStatus?.building == false }.allSatisfy {
                $0.countAllMatches && !$0.matchCountComplete
            }, "Running counts never claim a complete total")
            let target = counted.capture!.target(counted.hits[0])!
            let bounded = try run(w, caseSensitive: caseSensitive, countAllMatches: false)
            check(bounded.indexStatus?.reused == true && !bounded.countAllMatches
                && f.count("build-begin:") == 1 && f.count("query-counted:true") == 1
                && f.count("query-counted:false") == 1,
                "Changing the count policy reuses captures and admits a fresh bounded query")
            _ = try target.openReader(100)
            check(f.count("source:") == 1 && f.count("open:") == 1,
                "Earlier counted results retain capture-bound activation after a policy change")
        }
        for defect in ["missing-policy", "changed-policy", "missing-completeness", "false-completeness",
                       "truncation", "count-overflow", "match-stop"] {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.countedTransport)
            f.configure { $0.hitCount = 1000; $0.countedMatches = 2500 }
            f.patch { phase, original in
                var value = original
                guard phase == "query-step" else { return value }
                switch defect {
                case "missing-policy": value.removeValue(forKey: "count_all_matches")
                case "changed-policy": value["count_all_matches"] = false
                case "missing-completeness": value.removeValue(forKey: "match_count_complete")
                case "false-completeness": value["match_count_complete"] = true
                case "truncation": value["truncated"] = false
                case "count-overflow": value["matches_seen"] = "18446744073709551615"
                default: value["stop_reason"] = "match-limit"; value["search_in_progress"] = false
                }
                return value
            }
            do { _ = try run(w, countAllMatches: true); preconditionFailure("Accepted invalid count proof: " + defect) }
            catch AtlasSearchError.invalidResponse { checks += 1 }
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.countedTransport)
            f.configure { $0.unavailable = 1; $0.hitCount = 1000; $0.countedMatches = 2500 }
            let partial = try run(w, countAllMatches: true)
            check(!partial.complete && !partial.matchCountComplete && partial.matchesSeen == 2500
                && partial.summary.contains("Partial search") && partial.summary.contains("2500"),
                "Unavailable captures keep counted totals explicitly partial")
        }
        do {
            let f = IndexedFixture(); var transport = f.countedTransport
            transport.countedQueryBegin = nil
            do { _ = try run(AtlasIndexedSearch(transport: transport), countAllMatches: true)
                preconditionFailure("A counted request silently used a legacy row-limited query")
            } catch AtlasSearchError.unsupportedCounting { checks += 1 }
            check(f.count("create:") == 0, "Unsupported count policy is refused before discovery or capture")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.configure { $0.hitCount = 70 }
            f.patch { _, original in withExcerpts(original) }
            let report = try run(w)
            check(report.hits.count == 70 && report.hits.allSatisfy { $0.excerpt?.matched == "x" },
                "Captured excerpts survive progressive delivery and the extra result page")
            check(report.hits[1].excerpt?.originalStart == 2 && report.hits[1].start == 2,
                "Repeated text in one file retains distinct occurrence coordinates")
            var withoutPreview = report.hits[0]; withoutPreview.excerpt = nil
            check(report.capture?.target(withoutPreview) != nil,
                "Optional presentation context does not grant or revoke exact hit activation")
        }
        for defect in ["changed-context", "wrong-highlight"] {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.patch { phase, original in
                let changed = phase == "query-step" && original["step_count"] as? String == "2"
                return withExcerpts(original, text: changed ? (defect == "changed-context" ? "x altered" : "y context") : "x context")
            }
            do { _ = try run(w); preconditionFailure("Inconsistent indexed excerpt accepted: " + defect) }
            catch AtlasSearchError.invalidResponse { checks += 1 }
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.patch { _, original in
                original["step_count"] as? String == "2" ? original : withExcerpts(original)
            }
            let report = try run(w)
            check(report.complete && report.hits.allSatisfy { $0.excerpt != nil },
                "Output-limited repeated pages may omit context without invalidating accepted results")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            let spelling = String(repeating: "K", count: 1024)
            f.configure { $0.hitCount = 1000; $0.insensitiveText = spelling
                $0.insensitiveRangeBytes = 2048; $0.encoding = "utf16le" }
            f.patch { _, original in
                var value = original
                if let rows = original["hits"] as? [[String: Any]] {
                    value["hits"] = rows.map { originalRow in
                        var row = originalRow
                        let range = row["original_range"] as! [String: String]
                        let start = UInt64(range["start"]!)!, end = UInt64(range["end"]!)!
                        row["excerpt"] = ["text": spelling + " context",
                            "original_range": ["start": String(start), "end": String(end + 16)],
                            "match_utf8_range": ["start": "0", "end": "3072"],
                            "has_replacements": false, "clipped_start": start != 0,
                            "clipped_end": true] as [String: Any]
                        return row
                    }
                }
                return value
            }
            let report = try run(w, query: String(repeating: "k", count: 1024), caseSensitive: false)
            check(report.complete && report.hits.count == 1000 && report.hits.first?.excerpt != nil
                && report.hits.last?.excerpt == nil,
                "Optional excerpt retention cannot starve a full page set of valid long-spelling witnesses")
            check(report.capture?.target(report.hits.last!)?.needle.utf8.elementsEqual(spelling.utf8) == true,
                "A result without retained preview still activates its exact original spelling")
            check(report.hits.compactMap(\.excerpt).reduce(0) { $0 + $1.retainedByteCount + 256 }
                <= AtlasSearchRetention.excerptBytes, "Indexed previews stay within their separate fixed allowance")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport), reports = IndexReports()
            f.configure { $0.hitCount = 70 }
            let first = try run(w, reports: reports)
            check(first.complete && first.hits.count == 70, "First query returns all paginated hits")
            check(first.hits.map(\.id) == Array(0..<70), "Occurrence identity survives extra pages")
            check(first.indexStatus?.reused == false && first.indexStatus?.capturedFiles == 2, "First query reports a new captured index")
            check(first.indexStatus?.skippedFiles == 1 && first.indexStatus?.verifiedFiles == 1, "Actual verification/skip counters are exposed")
            check(first.summary.contains("Refresh to include file edits"), "Snapshot freshness cannot be mistaken for a live scan")
            check(reports.values.contains { $0.indexStatus?.building == true && $0.hits.isEmpty && !$0.complete }, "Index preparation is not an empty complete query")
            check(reports.values.contains { $0.indexStatus?.building == false && $0.isInProgress && $0.hits.count == 70 }, "Indexed hits publish progressively")
            check(reports.values.filter { $0.indexStatus?.building == false }.allSatisfy { $0.streamID == first.streamID }, "One query keeps one row identity")
            let oldTarget = first.capture!.target(first.hits[0])!
            let second = try run(w, query: "new exact query")
            check(second.indexStatus?.reused == true && second.streamID != first.streamID, "Second query reuses source, not result identities")
            check(f.count("create:") == 1 && f.count("open:") == 1 && f.count("build-begin:") == 1 && f.count("build-step:") == 2,
                  "Repeated queries do not rediscover, recapture or rebuild")
            check(f.count("query-begin:") == 2 && f.count("query-step:") == 4 && f.count("page:") == 2, "Only new query verification and needed pages repeat")
            check(f.trace.contains { $0.hasPrefix("query-begin:") && $0.hasSuffix(":3") }, "Monotone full-width query identities")
            let info = try oldTarget.openReader(100)
            check(info.contains("host-supplied") && f.count("source:") == 1, "An earlier query's source still imports after result replacement")
            check(f.count("open:") == 1 && f.count("build-step:") == 2, "Source activation does not read the live file")
            let forged = SearchHit(id: 0, path: "a.rs", sourcePath: "a.rs", start: 2, end: 3,
                captureSHA256: first.hits[0].captureSHA256, captureByteLength: 4096)
            check(first.capture?.target(forged) == nil, "Reused row numbers do not borrow capture witnesses")
        }
        do {
            let f = IndexedFixture()
            var w: AtlasIndexedSearch? = AtlasIndexedSearch(transport: f.transport)
            var first: AtlasSearchReport? = try run(w!)
            var target = first!.capture!.target(first!.hits[0])
            let firstID = first!.streamID
            first = nil; w = nil
            check(f.count("close:") == 0, "Selected target pins old index through refresh")
            f.configure { $0.digest = String(repeating: "b", count: 64) }
            let newer = AtlasIndexedSearch(transport: f.transport)
            let second = try run(newer)
            check(second.streamID != firstID && second.hits[0].captureSHA256 == String(repeating: "b", count: 64), "Explicit refresh establishes a new source owner")
            _ = try target!.openReader(100)
            check(f.count("create:") == 2 && f.count("source:") == 1, "Old source remains importable from old owner after refresh")
            target = nil; wait { f.count("close:") == 1 }
            check(f.count("retirement-failed") == 0, "Old index retires without affecting replacement")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.configure { $0.fallback = true }
            let r = try run(w)
            check(r.complete && r.hits.count == 2, "Uncovered gram segments retain fallback matches")
            check(r.indexStatus?.verifiedFiles == 1, "Fallback verification is recorded")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport), progress = IndexReports()
            f.configure { $0.insensitiveText = "İ"; $0.insensitiveRangeBytes = 2; $0.encoding = "utf16le" }
            let first = try run(w, query: "i\u{307}", reports: progress, caseSensitive: false)
            check(!first.caseSensitive && first.complete && first.hits[0].actualText == "İ"
                && first.hits[0].end == 2, "Insensitive indexed result retains source spelling and original UTF-16 range")
            check(progress.values.contains { $0.indexStatus?.building == true }
                && progress.values.allSatisfy { !$0.caseSensitive },
                "Index preparation and query progress carry the same frozen case semantics")
            check(first.indexStatus?.skippedFiles == 0 && first.indexStatus?.verifiedFiles == 2,
                "Insensitive fallback verification counters do not imply case-sensitive index exclusions")
            let target = first.capture!.target(first.hits[0])!
            check(target.needle.utf8.elementsEqual("İ".utf8) && !target.caseSensitive,
                "Captured-index reader receives the actual spelling as an exact query")
            _ = try run(w, query: "i\u{307}")
            let again = try run(w, query: "i\u{307}", caseSensitive: false)
            check(again.indexStatus?.reused == true && again.streamID != first.streamID,
                "Toggling Match Case keeps sources and replaces only query identity")
            check(f.count("create:") == 1 && f.count("build-begin:") == 1 && f.count("build-step:") == 2
                && f.count("query-case:false") == 2 && f.count("query-case:true") == 1,
                "Changing case never recaptures or rebuilds the index")
            _ = try target.openReader(100)
            check(f.count("source:") == 1 && f.count("open:") == 1,
                "Original insensitive target remains importable after replacement queries without live I/O")
            var forged = first.hits[0]; forged.actualText = "i\u{307}"
            check(first.capture?.target(forged) == nil, "A substituted matched spelling cannot borrow the indexed range")
        }
        do {
            let f = IndexedFixture(); var transport = f.transport; transport.matchingQueryBegin = nil
            let w = AtlasIndexedSearch(transport: transport)
            do { _ = try run(w, caseSensitive: false); preconditionFailure("Legacy transport ignored Match Case") }
            catch AtlasSearchError.unsupportedMatching { checks += 1 }
            check(f.count("create:") == 0, "Unsupported matching is refused before index allocation or source reads")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            let types = try AtlasSearchFileScope(extensions: ["rs"])
            let location = try AtlasSearchPathScope(kind: .directory, path: "src")
            f.configure { $0.pathHex = "7372632f612e7273"; $0.scopedEligible = 1; $0.scopedCaptured = 1 }
            let result = try w.runScoped(input: AtlasSearchInput(root: "/repo", query: "x", fileScope: types,
                pathScope: location, caseSensitive: false), cancellation: AtlasSearchCancellation(),
                access: AtlasSearchAccessLease(nil), progress: { _ in })
            check(!result.caseSensitive && result.fileScope == types && result.pathScope == location
                && result.excludedFiles == 1 && result.hits[0].sourcePath == "src/a.rs",
                "Insensitive indexed queries preserve combined type and path predicates")
        }
        for defect in ["case", "missing-case", "mode", "missing-text", "empty-text", "nul-text", "oversized-text"] {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.patch { phase, original in
                var value = original
                guard phase == "query-step" else { return value }
                if defect == "case" { value["case_sensitive"] = true }
                else if defect == "missing-case" { value.removeValue(forKey: "case_sensitive") }
                else if defect == "mode" { value["mode"] = "exact-decoded-literal" }
                else {
                    var rows = value["hits"] as! [[String: Any]]
                    if defect == "missing-text" { rows[0].removeValue(forKey: "matched_text") }
                    if defect == "empty-text" { rows[0]["matched_text"] = "" }
                    if defect == "nul-text" { rows[0]["matched_text"] = "x\0y" }
                    if defect == "oversized-text" { rows[0]["matched_text"] = String(repeating: "X", count: 4097) }
                    value["hits"] = rows
                }
                return value
            }
            do { _ = try run(w, caseSensitive: false); preconditionFailure("Invalid indexed matching receipt: " + defect) }
            catch AtlasSearchError.invalidResponse { checks += 1 }
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.configure { $0.insensitiveText = "é"; $0.insensitiveRangeBytes = 2 }
            f.patch { phase, original in
                var value = original
                if phase == "query-step", value["step_count"] as? String == "2" {
                    var rows = value["hits"] as! [[String: Any]]
                    rows[0]["matched_text"] = "e\u{301}"; value["hits"] = rows
                }
                return value
            }
            do { _ = try run(w, query: "é", caseSensitive: false); preconditionFailure("Repeated indexed witness changed spelling") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
        }
        for mode in 0..<3 {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.configure { c in
                if mode == 0 { c.unavailable = 1 }
                if mode == 1 { c.pending = 1 }
                if mode == 2 { c.discovery = false }
            }
            let r = try run(w)
            check(!r.complete && !r.isInProgress && r.hits.count == 2, "Partial catalog/captures remain useful but never exhaustive")
            let rerun = try run(w)
            check(rerun.indexStatus?.reused == true, "Partial index is explicit and reusable without silently excluding coverage")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.configure { $0.files = 0 }
            let r = try run(w)
            check(r.complete && r.hits.isEmpty && r.indexStatus?.capturedFiles == 0, "Empty closed catalog finalizes without a phantom source")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.configure { $0.pathHex = "ff2e7273" }
            let r = try run(w)
            check(r.hits[0].sourcePath == nil && r.capture?.target(r.hits[0]) == nil, "Non-UTF-8 path stays visible without fabricated native opener")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            let canceled = AtlasSearchCancellation(); canceled.cancel()
            do { _ = try run(w, flag: canceled); preconditionFailure("Pre-cancel admitted") }
            catch AtlasSearchError.canceled { checks += 1 }
            check(f.count("create:") == 0, "Canceled query does not create an index")
            do { _ = try run(w, query: ""); preconditionFailure("Invalid input admitted") }
            catch AtlasSearchError.invalidRequest { checks += 1 }
            check(f.count("create:") == 0, "Invalid input is inert")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport), flag = AtlasSearchCancellation()
            f.block { p in if p == "build-step" { flag.cancel() } }
            do { _ = try run(w, flag: flag); preconditionFailure("Build cancellation ignored") }
            catch AtlasSearchError.canceled { checks += 1 }
            wait { f.count("close:") == 1 }
            f.block(nil)
            let retry = try run(w)
            check(retry.indexStatus?.reused == false && f.count("create:") == 2, "Canceled candidate is never cached as ready")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            _ = try run(w)
            let flag = AtlasSearchCancellation()
            f.block { p in if p == "query-step" { flag.cancel() } }
            do { _ = try run(w, query: "canceled", flag: flag); preconditionFailure("Query cancellation ignored") }
            catch AtlasSearchError.canceled { checks += 1 }
            f.block(nil)
            let next = try run(w, query: "after canceled")
            check(next.indexStatus?.reused == true && f.count("create:") == 1, "Canceled query preserves accepted index")
            check(f.trace.contains { $0.hasPrefix("query-begin:") && $0.hasSuffix(":4") }, "Canceled attempt cannot reuse generation")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport), flags = IndexReports()
            let flag = AtlasSearchCancellation()
            do {
                _ = try w.run(root: "/repo", query: "x", cancellation: flag, access: AtlasSearchAccessLease(nil)) { r in
                    flags.offer(r)
                    if !r.hits.isEmpty { flag.cancel() }
                }
                preconditionFailure("Progress cancellation ignored")
            } catch AtlasSearchError.canceled { checks += 1 }
            check(f.count("query-step:") == 1, "Cancel after first hits prevents another step")
            let rerun = try run(w)
            check(rerun.indexStatus?.reused == true, "Index survives stopping after useful early matches")
        }
        do {
            final class Grant {}
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport), a = Grant(), b = Grant()
            _ = try run(w, access: a); _ = try run(w, access: a)
            check(f.count("create:") == 1, "Same raw root and grant reuse index")
            _ = try run(w, access: b)
            check(f.count("create:") == 2, "New grant does not borrow old native access")
            _ = try run(w, root: "/re\u{301}po", access: b)
            _ = try run(w, root: "/répo", access: b)
            check(f.count("create:") == 4, "Canonically equivalent root strings remain distinct byte identities")
        }
        for (phase, key, value) in [
            ("open", "owner", "1"), ("open", "layout_revision", "0"), ("open", "catalogued_files", "20001"),
            ("build-begin", "index_generation", "2"), ("build-step", "source_manifest", "2"),
            ("build-step", "captured_files", "03"), ("build-step", "indexed_source_bytes", "33554433"),
            ("query-begin", "query_generation", "1"), ("query-step", "query_generation", "99"),
            ("query-step", "capture_manifest", "3"), ("query-step", "index_generation", "2"),
            ("query-step", "needle", "not the query"), ("query-step", "read_calls", "1"),
            ("query-step", "source_bytes_read", "1"), ("query-step", "search_strategy", "live-capture-scan"),
            ("query-step", "step_count", "99"), ("query-step", "verified_files", "2"),
            ("query-step", "pending_files", "0"), ("query-step", "retained_hits", "1001"),
            ("query-step", "verification_source_bytes", "999999"), ("query-step", "stop_reason", "unrecognized")
        ] { try reject(phase, key, value: value) }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.patch { p, object in var o = object; if p == "query-step" { o["search_complete"] = true }; return o }
            do { _ = try run(w); preconditionFailure("Early exhaustive flag accepted") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.configure { $0.hitCount = 70 }
            f.patch { p, object in var o = object; if p == "page" { o["matches_seen"] = "71" }; return o }
            do { _ = try run(w); preconditionFailure("Page from different coverage accepted") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
        }
        for field in ["query_generation", "hit_id", "original_range", "source_reopened", "capture_sha256", "capture_manifest", "reader_owner"] {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            let r = try run(w), t = r.capture!.target(r.hits[0])!
            f.patch { p, object in
                var o = object
                if p == "source" {
                    if field == "source_reopened" { o[field] = true }
                    else { o[field] = "wrong" }
                }
                return o
            }
            do { _ = try t.openReader(100); preconditionFailure("Corrupt source handoff accepted: \(field)") }
            catch { checks += 1 }
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.patch { phase, object in
                var o = object
                if phase == "query-step", o["step_count"] as? String == "2" {
                    var rows = o["hits"] as! [[String: Any]]
                    rows[0]["capture_sha256"] = String(repeating: "b", count: 64); o["hits"] = rows
                }
                return o
            }
            do { _ = try run(w); preconditionFailure("Changed repeated source witness accepted") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
        }
        let rustScope = try AtlasFileScope.rust.searchScope()
        do {
            let f = IndexedFixture()
            f.configure { c in c.scopedEligible = 1; c.scopedCaptured = 1; c.scopedUnavailable = 0 }
            let w = AtlasIndexedSearch(transport: f.transport)
            let first = try scoped(w, scope: rustScope)
            check(first.complete && first.fileScope == rustScope && first.hits.count == 2, "Indexed query admits its frozen file scope")
            check(first.progress?.cataloguedFiles == 1 && first.excludedFiles == 1, "Indexed progress uses eligible membership")
            let onlyRS = try AtlasSearchFileScope(extensions: ["rs"])
            let second = try scoped(w, scope: onlyRS)
            check(second.fileScope == onlyRS && second.streamID != first.streamID && second.indexStatus?.reused == true,
                "Scope change creates a query generation without changing captured bytes")
            check(f.count("create:") == 1 && f.count("build-step:") == 1 && f.count("query-step:") == 2,
                "Scope changes do not rebuild the index or silently fall back to live discovery")
            let all = try run(w)
            check(all.fileScope.isAll && all.progress?.cataloguedFiles == 2 && all.indexStatus?.reused == true,
                "Widening a scope recomputes candidates against the same index")
            let selected = first.capture!.target(first.hits[0])!
            _ = try selected.openReader(100)
            check(f.count("source:") == 1, "Scope replacement leaves selected old capture usable")
        }
        do {
            let f = IndexedFixture()
            f.configure { c in
                c.files = 20_000; c.pending = 19_998
                c.scopedEligible = 1; c.scopedCaptured = 1; c.scopedUnavailable = 0
            }
            let w = AtlasIndexedSearch(transport: f.transport)
            let broad = try run(w)
            check(!broad.complete && broad.progress?.cataloguedFiles == 20_000
                && broad.progress?.examinedFiles == 2 && broad.indexStatus?.capturedFiles == 2,
                "Wide metadata stays separate from bounded captured sources and reports incomplete coverage")
            let selected = try scoped(w, scope: .all,
                pathScope: AtlasSearchPathScope(kind: .file, path: "a.rs"))
            check(selected.complete && selected.progress?.cataloguedFiles == 1
                && selected.excludedFiles == 19_999 && selected.indexStatus?.reused == true,
                "Narrowing the wide metadata universe searches the same retained captures")
            check(f.count("create:") == 1 && f.count("build-step:") == 2,
                "Scope changes never recapture a wide catalog merely to change query membership")
        }
        for mode in 0..<3 {
            let f = IndexedFixture()
            f.configure { c in
                c.files = 3; c.scopedCaptured = 1
                if mode == 0 { c.unavailable = 1; c.scopedEligible = 1; c.scopedUnavailable = 0 }
                if mode == 1 { c.unavailable = 1; c.scopedEligible = 2; c.scopedUnavailable = 1 }
                if mode == 2 { c.pending = 1; c.scopedEligible = 2; c.scopedUnavailable = 0 }
            }
            let w = AtlasIndexedSearch(transport: f.transport), r = try scoped(w, scope: rustScope)
            if mode == 0 {
                check(r.complete && r.unavailableFiles == 0 && r.excludedFiles == 2, "Failures outside requested scope do not falsify selected completeness")
            } else {
                check(!r.complete && r.progress?.cataloguedFiles == 2, "Uncaptured or unavailable selected files remain explicit partial coverage")
            }
        }
        do {
            let f = IndexedFixture()
            f.configure { c in c.scopedEligible = 0; c.scopedCaptured = 0; c.scopedUnavailable = 0; c.scopedHitCount = 0 }
            let w = AtlasIndexedSearch(transport: f.transport)
            let r = try scoped(w, scope: AtlasSearchFileScope(extensions: []))
            check(r.complete && r.hits.isEmpty && r.excludedFiles == 2 && r.progress?.cataloguedFiles == 0
                && r.indexStatus?.verificationBytes == 0, "Empty included scope never verifies or exposes every indexed file")
        }
        do {
            let f = IndexedFixture()
            var transport = f.transport; transport.scopedQueryBegin = nil
            let w = AtlasIndexedSearch(transport: transport)
            do { _ = try scoped(w, scope: rustScope); preconditionFailure("Legacy transport widened scope") }
            catch AtlasSearchError.invalidScope { checks += 1 }
            check(f.count("create:") == 0, "Unsupported scope refuses before any source capture")
            let permitted = try run(w)
            check(permitted.complete, "Legacy all-files route still succeeds")
        }
        for defect in ["search_scope", "scope_extensions", "eligible_files", "excluded_files", "eligible_captured_files", "scoped_capture_unavailable_files", "excluded-hit"] {
            let f = IndexedFixture()
            f.configure { c in c.scopedEligible = 1; c.scopedCaptured = 1; c.scopedUnavailable = 0 }
            f.patch { phase, object in
                var o = object
                if phase == "query-step" {
                    if defect == "search_scope" { o[defect] = "all" }
                    else if defect == "scope_extensions" { o[defect] = "py" }
                    else if defect == "excluded-hit" {
                        var rows = o["hits"] as! [[String: Any]]
                        rows[0]["path"] = ["encoding": "unix-bytes", "hex": "612e747874", "display": "a.rs"]
                        o["hits"] = rows
                    } else { o[defect] = "3" }
                }
                return o
            }
            let w = AtlasIndexedSearch(transport: f.transport)
            do { _ = try scoped(w, scope: rustScope); preconditionFailure("Malformed scope accepted: \(defect)") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport), flag = AtlasSearchCancellation()
            flag.cancel()
            do { _ = try scoped(w, scope: rustScope, flag: flag); preconditionFailure("Canceled scope admitted") }
            catch AtlasSearchError.canceled { checks += 1 }
            check(f.count("create:") == 0, "Scoped pre-cancellation performs no capture")
        }
        let directory = try AtlasSearchPathScope(kind: .directory, path: "src")
        let file = try AtlasSearchPathScope(kind: .file, path: "src/a.rs")
        do {
            let f = IndexedFixture()
            f.configure { c in c.pathHex = "7372632f612e7273"; c.scopedEligible = 1; c.scopedCaptured = 1; c.scopedUnavailable = 0 }
            let w = AtlasIndexedSearch(transport: f.transport)
            let folder = try scoped(w, scope: .all, pathScope: directory)
            check(folder.complete && folder.pathScope == directory && folder.hits.count == 2
                && folder.progress?.cataloguedFiles == 1 && folder.excludedFiles == 1,
                "Path-only index query verifies its selected capture population")
            let selected = folder.capture!.target(folder.hits[0])!
            let exact = try scoped(w, scope: rustScope, pathScope: file)
            check(exact.complete && exact.pathScope == file && exact.fileScope == rustScope
                && exact.streamID != folder.streamID && exact.indexStatus?.reused == true,
                "Location changes freeze a fresh query while reusing the accepted index")
            let broad = try run(w)
            check(broad.pathScope.isWorkspace && broad.progress?.cataloguedFiles == 2 && broad.indexStatus?.reused == true,
                "Returning to workspace widens candidates without rebuilding captures")
            check(f.count("create:") == 1 && f.count("open:") == 1 && f.count("build-step:") == 1,
                "Folder/file/extension query changes do not reread the repository")
            check(f.trace.contains("query-path:directory:737263") && f.trace.contains("query-path:file:7372632f612e7273"),
                "The transport receives exact frozen location bytes")
            _ = try selected.openReader(100)
            check(f.count("source:") == 1, "A selected old hit keeps its capture after path-scope replacement")
            let flag = AtlasSearchCancellation()
            f.block { phase in if phase == "query-step" { flag.cancel() } }
            do { _ = try scoped(w, scope: .all, pathScope: file, flag: flag); preconditionFailure("Path-scoped query cancellation ignored") }
            catch AtlasSearchError.canceled { checks += 1 }
            f.block(nil)
            let retried = try scoped(w, scope: .all, pathScope: directory)
            check(retried.indexStatus?.reused == true && f.count("create:") == 1,
                "Canceling a file query preserves captured sources for the next folder query")
        }
        for mode in 0..<3 {
            let f = IndexedFixture()
            f.configure { c in
                c.pathHex = "7372632f612e7273"; c.files = 3; c.scopedCaptured = 1
                if mode == 0 { c.unavailable = 1; c.scopedEligible = 1; c.scopedUnavailable = 0 }
                if mode == 1 { c.unavailable = 1; c.scopedEligible = 2; c.scopedUnavailable = 1 }
                if mode == 2 { c.pending = 1; c.scopedEligible = 2; c.scopedUnavailable = 0 }
            }
            let w = AtlasIndexedSearch(transport: f.transport), result = try scoped(w, scope: .all, pathScope: directory)
            check(result.pathScope == directory && result.complete == (mode == 0),
                "Only excluded failures leave selected-directory coverage complete")
            check(result.progress?.cataloguedFiles == (mode == 0 ? 1 : 2),
                "Selected missing/unreadable files remain in the path-scope denominator")
        }
        do {
            let f = IndexedFixture()
            f.configure { c in c.scopedEligible = 0; c.scopedCaptured = 0; c.scopedUnavailable = 0; c.scopedHitCount = 0 }
            let w = AtlasIndexedSearch(transport: f.transport)
            let empty = try scoped(w, scope: .all, pathScope: file)
            check(empty.complete && empty.hits.isEmpty && empty.progress?.cataloguedFiles == 0
                && empty.excludedFiles == 2 && empty.indexStatus?.verificationBytes == 0,
                "An exact path absent from the index remains a complete empty selection")
        }
        do {
            let f = IndexedFixture()
            f.configure { $0.pathHex = "7372632fff2e7273" }
            let w = AtlasIndexedSearch(transport: f.transport)
            let raw = try scoped(w, scope: .all, pathScope: directory)
            check(raw.hits.count == 2 && raw.hits[0].sourcePath == nil && raw.capture?.target(raw.hits[0]) == nil,
                "A non-UTF8 directory descendant retains searchable bytes without a fabricated opener")
        }
        do {
            let f = IndexedFixture()
            var transport = f.transport; transport.pathScopedQueryBegin = nil
            let w = AtlasIndexedSearch(transport: transport)
            do { _ = try scoped(w, scope: .all, pathScope: directory); preconditionFailure("Legacy index widened path scope") }
            catch AtlasSearchError.invalidPathScope { checks += 1 }
            check(f.count("create:") == 0, "Unsupported location rejects before source capture")
            let permitted = try run(w)
            check(permitted.complete, "Legacy workspace query remains usable")
        }
        for phase in ["query-begin", "query-step", "page"] {
            let defects = ["missing", "kind", "path", "hex"] + (phase == "query-begin" ? [] : ["sibling-prefix"])
            for defect in defects {
                let f = IndexedFixture()
                f.configure { c in c.pathHex = "7372632f612e7273"; c.hitCount = 70; c.scopedEligible = 1; c.scopedCaptured = 1; c.scopedUnavailable = 0 }
                f.patch { received, object in
                    var value = object
                    if received == phase {
                        if defect == "missing" {
                            for key in ["scope_path_kind", "scope_path", "scope_path_hex"] { value.removeValue(forKey: key) }
                        } else if defect == "kind" { value["scope_path_kind"] = "file" }
                        else if defect == "path" { value["scope_path"] = "src2" }
                        else if defect == "hex" { value["scope_path_hex"] = "00" }
                        else {
                            var rows = value["hits"] as! [[String: Any]]
                            rows[0]["path"] = ["encoding": "unix-bytes", "hex": "737263322f612e7273", "display": "src/a.rs"]
                            value["hits"] = rows
                        }
                    }
                    return value
                }
                let w = AtlasIndexedSearch(transport: f.transport)
                do { _ = try scoped(w, scope: .all, pathScope: directory); preconditionFailure("Corrupt path \(phase)/\(defect) admitted") }
                catch AtlasSearchError.invalidResponse { checks += 1 }
            }
        }
        let composed = try AtlasSearchPathScope(kind: .directory, path: "café")
        let decomposed = try AtlasSearchPathScope(kind: .directory, path: "cafe\u{301}")
        for defect in ["echo", "row"] {
            let f = IndexedFixture()
            f.configure { $0.pathHex = composed.pathHex + "2f612e7273" }
            f.patch { phase, object in
                var value = object
                if phase == "query-step" {
                    if defect == "echo" { value["scope_path"] = decomposed.path }
                    else {
                        var rows = value["hits"] as! [[String: Any]]
                        rows[0]["path"] = ["encoding": "unix-bytes", "hex": decomposed.pathHex + "2f612e7273", "display": "café/a.rs"]
                        value["hits"] = rows
                    }
                }
                return value
            }
            let w = AtlasIndexedSearch(transport: f.transport)
            do { _ = try scoped(w, scope: .all, pathScope: composed); preconditionFailure("Normalized index path accepted") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            let late = try AtlasSearchPathScope(kind: .file, path: "zz-last.rs")
            f.configure { c in
                c.files = 20_000; c.fileID = 19_999; c.pathHex = "7a7a2d6c6173742e7273"
                c.scopedEligible = 1; c.scopedCaptured = 1; c.scopedUnavailable = 0
            }
            let selected = try scoped(w, scope: .all, pathScope: late)
            check(selected.complete && selected.progress?.cataloguedFiles == 1 && selected.excludedFiles == 19_999,
                "An initial late-file scope captures its own population before the global file quota")
            check(selected.indexStatus?.capturedFiles == 1 && selected.indexStatus?.captureEligibleFiles == 1
                && selected.indexStatus?.capturePathScope == late && selected.indexStatus?.uncapturedFiles == 0,
                "Capture provenance records the exact scope that admitted source work")
            check(f.count("build-step:") == 1 && f.trace.contains("capture-path:file:7a7a2d6c6173742e7273:"),
                "Selected capture admission reaches the scoped transport once")
            let target = selected.capture!.target(selected.hits[0])!
            let broad = try run(w, query: "another query")
            check(!broad.complete && broad.progress?.cataloguedFiles == 20_000
                && broad.indexStatus?.reused == true && broad.indexStatus?.uncapturedFiles == 19_999,
                "Widening queries keeps the retained scope and reports the uncaptured population")
            check(broad.summary.contains("Refresh to capture the current scope")
                && f.count("create:") == 1 && f.count("build-step:") == 1,
                "Broadening explains explicit Refresh without a hidden recapture or live fallback")
            f.configure { $0.digest = String(repeating: "b", count: 64) }
            let refreshed = AtlasIndexedSearch(transport: f.transport)
            let fresh = try scoped(refreshed, scope: .all, pathScope: late)
            check(fresh.complete && fresh.indexStatus?.reused == false
                && fresh.hits[0].captureSHA256 != selected.hits[0].captureSHA256 && f.count("create:") == 2,
                "Explicit owner replacement captures the current selected scope again")
            _ = try target.openReader(100)
            check(f.count("source:") == 1 && f.count("open:") == 2,
                "The earlier late-file capture still imports after refreshed bytes replace the query")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            let location = try AtlasSearchPathScope(kind: .directory, path: "src")
            let flag = AtlasSearchCancellation()
            f.configure { c in
                c.files = 5; c.pathHex = "7372632f612e7273"
                c.scopedEligible = 2; c.scopedCaptured = 2; c.scopedUnavailable = 0
            }
            f.patch { phase, value in
                if phase == "build-step", value["build_steps"] as? String == "1" { flag.cancel() }
                return value
            }
            do { _ = try scoped(w, scope: .all, pathScope: location, flag: flag); preconditionFailure("Paused build cancellation ignored") }
            catch AtlasSearchError.canceled { checks += 1 }
            f.patch { _, value in value }
            let reports = IndexReports(), broad = try run(w, query: "replacement", reports: reports)
            check(f.count("create:") == 1 && f.count("build-begin:") == 1 && f.count("build-step:") == 2,
                "A validated preparation step survives cancellation and resumes its original scope")
            check(!broad.complete && broad.pathScope.isWorkspace
                && broad.indexStatus?.capturePathScope == location && broad.indexStatus?.uncapturedFiles == 3,
                "Changed query membership does not relabel the paused capture")
            check(reports.values.contains { $0.indexStatus?.building == true && $0.pathScope.isWorkspace
                && $0.progress?.cataloguedFiles == 2 && $0.indexStatus?.capturePathScope == location
                && $0.summary.contains("selected folder") },
                "Resumed progress keeps current query authority and separately describes original capture scope")
        }
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            f.configure { c in
                c.pathHex = "7372632f612e7273"; c.scopedEligible = 1; c.scopedCaptured = 1; c.scopedUnavailable = 0
            }
            _ = try scoped(w, scope: .all, pathScope: AtlasSearchPathScope(kind: .directory, path: "src"))
            f.patch { phase, original in
                var value = original
                if phase == "query-step" {
                    var rows = value["hits"] as! [[String: Any]]
                    rows[0]["path"] = ["encoding": "unix-bytes", "hex": "6f746865722f612e7273", "display": "other/a.rs"]
                    value["hits"] = rows
                }
                return value
            }
            do { _ = try run(w); preconditionFailure("Broader query borrowed an uncaptured source witness") }
            catch AtlasSearchError.invalidResponse { checks += 1 }
        }
        do {
            let f = IndexedFixture(); var transport = f.transport; transport.scopedIndexBegin = nil
            let worker = AtlasIndexedSearch(transport: transport)
            do {
                _ = try scoped(worker, scope: .all, pathScope: AtlasSearchPathScope(kind: .file, path: "a.rs"))
                preconditionFailure("Legacy builder silently captured a global prefix")
            } catch AtlasSearchError.invalidPathScope { checks += 1 }
            do { _ = try scoped(worker, scope: rustScope); preconditionFailure("Legacy builder widened initial type scope") }
            catch AtlasSearchError.invalidScope { checks += 1 }
            check(f.count("create:") == 0, "Unsupported scoped preparation is refused before discovery or source work")
            let permitted = try run(worker)
            check(permitted.complete && permitted.indexStatus?.capturePathScope.isWorkspace == true,
                "Legacy all-workspace preparation still serves its declared contract")
        }
        do {
            let f = IndexedFixture(); var transport = f.transport; transport.scopedIndexBegin = nil
            f.patch { _, original in
                original.filter { entry in
                    let key = entry.key
                    return !(key == "capture_search_scope" || key.hasPrefix("capture_scope_")
                        || key == "capture_case_sensitive" || key == "capture_eligible_files" || key == "capture_excluded_files")
                }
            }
            let old = try run(AtlasIndexedSearch(transport: transport))
            check(old.complete && old.indexStatus?.captureEligibleFiles == 2,
                "Truly legacy all-workspace receipts remain supported without invented scoped authority")
        }
        for phase in ["build-begin", "build-step", "query-begin", "query-step", "page", "source"] {
            for defect in ["missing", "filter", "path", "case", "eligible", "excluded", "catalog"] {
                let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
                f.configure { $0.hitCount = 70 }
                f.patch { received, original in
                    var value = original
                    guard received == phase else { return value }
                    switch defect {
                    case "missing": value.removeValue(forKey: "capture_scope_path_hex")
                    case "filter": value["capture_scope_extensions"] = "rs"
                    case "path": value["capture_scope_path_kind"] = "directory"
                    case "case": value["capture_case_sensitive"] = false
                    case "eligible": value["capture_eligible_files"] = "1"
                    case "excluded": value["capture_excluded_files"] = "1"
                    default: value["catalogued_files"] = "1"
                    }
                    return value
                }
                do {
                    let report = try run(w)
                    if phase == "source" { _ = try report.capture!.target(report.hits[0])!.openReader(100) }
                    preconditionFailure("Capture provenance corruption accepted: \(phase)/\(defect)")
                } catch { checks += 1 }
            }
        }
        #if canImport(CryptoKit)
        do {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            let visible = try AtlasSearchPathScope(visiblePaths: [Array("zz-last.rs".utf8), [255, 46, 114, 115]])
            f.configure { c in
                c.files = 20_000; c.pathHex = "7a7a2d6c6173742e7273"; c.fileID = 19_999
                c.scopedEligible = 2; c.scopedCaptured = 2; c.scopedUnavailable = 0
            }
            let report = try scoped(w, scope: .all, pathScope: visible)
            check(report.complete && report.indexStatus?.capturePathScope == visible
                && report.indexStatus?.captureEligibleFiles == 2 && f.count("build-step:") == 2,
                "Frozen raw-byte visible membership, including non-UTF8 paths, selects capture work before quotas")
            let emptyFixture = IndexedFixture()
            emptyFixture.configure { c in c.scopedEligible = 0; c.scopedCaptured = 0; c.scopedUnavailable = 0; c.scopedHitCount = 0 }
            let empty = try scoped(AtlasIndexedSearch(transport: emptyFixture.transport), scope: .all,
                pathScope: AtlasSearchPathScope(visiblePaths: []))
            check(empty.complete && empty.hits.isEmpty && empty.indexStatus?.capturedBytes == 0
                && empty.indexStatus?.captureEligibleFiles == 0,
                "An empty visible capture stays empty and never widens to the workspace")
        }
        for defect in ["digest", "missing", "coverage", "complete"] {
            let f = IndexedFixture(), w = AtlasIndexedSearch(transport: f.transport)
            let visible = try AtlasSearchPathScope(visiblePaths: [Array("a.rs".utf8), Array("missing.rs".utf8)])
            f.configure { c in
                c.scopedEligible = 1; c.scopedCaptured = 1; c.scopedUnavailable = 0
                c.captureVisibleCatalogued = 1; c.captureVisibleMissing = 1
            }
            f.patch { phase, original in
                var value = original
                guard phase == "build-step" else { return value }
                switch defect {
                case "digest": value["capture_scope_region_sha256"] = String(repeating: "0", count: 64)
                case "missing": value.removeValue(forKey: "capture_scope_region_files")
                case "coverage": value["capture_scope_region_missing_files"] = "0"
                default: value["capture_complete"] = true
                }
                return value
            }
            do { _ = try scoped(w, scope: .all, pathScope: visible); preconditionFailure("Corrupt visible capture accepted: " + defect) }
            catch AtlasSearchError.invalidResponse { checks += 1 }
        }
        do {
            let f = IndexedFixture()
            let visible = try AtlasSearchPathScope(visiblePaths: [Array("a.rs".utf8), Array("missing.rs".utf8)])
            f.configure { c in
                c.scopedEligible = 1; c.scopedCaptured = 1; c.scopedUnavailable = 0
                c.captureVisibleCatalogued = 1; c.captureVisibleMissing = 1
            }
            let partial = try scoped(AtlasIndexedSearch(transport: f.transport), scope: .all, pathScope: visible)
            check(!partial.complete && partial.hits.count == 2 && partial.visibleMissingFiles == 1
                && partial.indexStatus?.captureMissingVisibleFiles == 1,
                "Missing visible paths preserve useful captured results with explicit incomplete membership")
        }
        #endif
        print("AtlasIndexedSearchTests: \(checks) checks passed")
    }
}
