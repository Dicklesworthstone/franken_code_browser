import Foundation
import Dispatch

/// Uses the shared foreign boundary fixture and the production saved-search
/// decoders, observable model, receipt validator and retained-reader worker.
/// Original ranges and digests are independent fixed UTF-8/UTF-16 vectors.
@main private enum AtlasSavedSearchTests {
    @MainActor private static func wait(_ message: String, _ ready: () -> Bool) async {
        for _ in 0..<2000 {
            if ready() { return }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        preconditionFailure(message)
    }
    @MainActor private static func prepared(_ fixture: SavedRepositoryFixture) async -> AtlasSavedRepositoryModel {
        let model = AtlasSavedRepositoryModel(transport: fixture.savedTransport)
        model.open(path: "/saved/repository.fcbs", accessLease: nil)
        await wait("archive did not open") { !model.busy }
        precondition(model.info?.identity.owner == 40)
        model.prepareRelationships()
        await wait("documentation links did not prepare") { !model.busy }
        precondition(model.relationshipGeneration == 1)
        model.searchInput = "ana"; model.search()
        await wait("saved decoded search did not finish") { !model.busy }
        precondition(model.queryGeneration == 1 && model.searchPage?.hits.count == 4)
        return model
    }
    @MainActor private static func preparedExpression(_ fixture: SavedRepositoryFixture) async -> AtlasSavedRepositoryModel {
        let model = await prepared(fixture)
        model.searchMode = .expression; model.searchInput = "ana +banana -unsafe"; model.search()
        await wait("saved expression did not finish") { !model.busy }
        precondition(model.expressionGeneration == 1 && model.expressionPage?.hits.count == 4)
        precondition(model.queryGeneration == 1 && model.relationshipGeneration == 1)
        return model
    }
    private static func rejected(_ message: String, _ operation: () throws -> Void) {
        do { try operation(); preconditionFailure(message) } catch { }
    }
    @MainActor private static func refused(_ message: String, _ operation: @MainActor () async throws -> Void) async {
        do { try await operation(); preconditionFailure(message) } catch { }
    }
    @MainActor static func main() async throws {
        try exactPageAndPartialCoverageContracts()
        try await exactUTF8AndUTF16HandoffsKeepOriginalMatches()
        await frozenArchiveQueryPageAndEditorRejectStaleHits()
        await failedAndLostRepliesReconcileWithoutRepeatingSearch()
        await canceledSearchKeepsAcceptedQueriesAndRelationships()
        await invalidMetadataAndHitReceiptsNeverPublishReaders()
        await closeDuringHitHandoffDrainsAndRetires()
        try expressionCoverageAndNamespaceContracts()
        try await scopedExpressionsKeepExactReadersAndIndependentModes()
        await expressionRecoveryCancellationAndArchiveReplacement()
        await forgedExpressionReceiptsAndLateReadersRetire()
        print("Saved literal and expression search, scoped coverage, UTF-8/UTF-16 matches, independent generations, recovery and cancellation regressions passed.")
    }

    private static func exactPageAndPartialCoverageContracts() throws {
        let f = SavedRepositoryFixture(searchVectors: true)
        let info = try AtlasSavedInfo.decode(f.encode(f.info(40)), handle: 40,
            expectedPath: Array("/saved/repository.fcbs".utf8), fresh: true)
        let page = try AtlasSavedSearchPage.decode(f.encode(f.searchPage(40, generation: 1, searching: true)),
            identity: info.identity, generation: 1, needle: "ana", start: 0, limit: 64, searching: true)
        precondition(!page.complete && !page.truncated && page.unavailable == 1 && page.unsupported == 0)
        precondition(page.retained == 4 && page.seen == 4 && page.indexGeneration == nil && page.indexPin == nil)
        precondition(page.hits.map(\.start) == [203, 205, 406, 410])
        precondition(page.hits.map(\.end) == [206, 208, 412, 416])
        precondition(page.hits[0].path.bytes == f.rawPath)
        precondition(page.hits[0].end - page.hits[0].start == 3)
        precondition(page.hits[2].end - page.hits[2].start == 6, "UTF-16 width was equated with the UTF-8 needle")
        let second = try AtlasSavedSearchPage.decode(f.encode(f.searchPage(40, generation: 1, start: 2, limit: 1)),
            identity: info.identity, generation: 1, needle: "ana", start: 2, limit: 1)
        precondition(second.hits.map(\.id) == [3] && second.next == 3)
        let empty = try AtlasSavedSearchPage.decode(f.encode(f.searchPage(40, generation: 1, start: 4)),
            identity: info.identity, generation: 1, needle: "ana", start: 4, limit: 64)
        precondition(empty.hits.isEmpty && empty.next == nil && !empty.complete)

        var truncated = f.searchPage(40, generation: 1)
        truncated["hits"] = Array((truncated["hits"] as! [[String: Any]]).prefix(2))
        truncated["retained_hits"] = "2"; truncated["matches_seen"] = "3"; truncated["truncated"] = true
        let partial = try AtlasSavedSearchPage.decode(f.encode(truncated), identity: info.identity,
            generation: 1, needle: "ana", start: 0, limit: 64)
        precondition(partial.truncated && !partial.complete && partial.seen == 3 && partial.hits.count == 2)

        for field in ["owner", "query_generation", "mode", "search_complete", "matches_seen",
                      "unavailable_files", "scanned_bytes", "retained_source_payload_bytes",
                      "index_generation", "index_pin", "next_offset"] {
            var bad = f.searchPage(40, generation: 1)
            switch field {
            case "owner": bad[field] = "41"
            case "query_generation": bad[field] = "2"
            case "mode": bad[field] = "raw-byte-substring"
            case "search_complete": bad[field] = true
            case "matches_seen": bad[field] = "5"
            case "unavailable_files": bad[field] = "0"
            case "scanned_bytes": bad[field] = "67108865"
            case "retained_source_payload_bytes": bad[field] = "1"
            default: bad.removeValue(forKey: field)
            }
            rejected("forged query page accepted: \(field)") {
                _ = try AtlasSavedSearchPage.decode(f.encode(bad), identity: info.identity,
                    generation: 1, needle: "ana", start: 0, limit: 64)
            }
        }
        var equivalent = f.searchPage(40, generation: 1)
        equivalent["needle"] = "e\u{301}"
        rejected("canonically equivalent query bytes rebound an accepted search") {
            _ = try AtlasSavedSearchPage.decode(f.encode(equivalent), identity: info.identity,
                generation: 1, needle: "é", start: 0, limit: 64)
        }
        var wrongOrder = f.searchPage(40, generation: 1)
        var rows = wrongOrder["hits"] as! [[String: Any]]
        rows[1]["original_range"] = ["start": "203", "end": "206"]
        wrongOrder["hits"] = rows
        rejected("a duplicate source occurrence was accepted under another result ID") {
            _ = try AtlasSavedSearchPage.decode(f.encode(wrongOrder), identity: info.identity,
                generation: 1, needle: "ana", start: 0, limit: 64)
        }
        for field in ["file_id", "source_revision", "original_range", "hit_id"] {
            var bad = f.hitObject(1)
            switch field {
            case "original_range": bad[field] = ["start": "9", "end": "8"]
            case "hit_id": bad[field] = 1
            default: bad[field] = "9"
            }
            rejected("forged hit identity or original range accepted: \(field)") {
                _ = try JSONDecoder().decode(AtlasSavedHit.self, from: Data(f.encode(bad).utf8))
            }
        }
        precondition(AtlasSavedSearchWire.validNeedle(" \n") && AtlasSavedSearchWire.validNeedle("\u{feff}"))
        for needle in ["", "a\0b", String(repeating: "é", count: 513)] {
            precondition(!AtlasSavedSearchWire.validNeedle(needle))
        }
        var clear = f.header(40, "clear-results")
        clear["accepted_query_generation"] = NSNull(); clear["retained_hits"] = "0"
        try AtlasSavedSearchPage.decodeClear(f.encode(clear), identity: info.identity)
        clear.removeValue(forKey: "accepted_query_generation")
        rejected("missing accepted-generation field became a successful clear") {
            try AtlasSavedSearchPage.decodeClear(f.encode(clear), identity: info.identity)
        }
    }

    @MainActor private static func exactUTF8AndUTF16HandoffsKeepOriginalMatches() async throws {
        let f = SavedRepositoryFixture(searchVectors: true), model = await prepared(f)
        let page = model.searchPage!, links = model.relationships!
        let utf8 = try await model.openHit(page.hits[0], from: page, readerTransport: f.readerTransport())
        guard let first = utf8.match else { preconditionFailure("UTF-8 hit lost its exact match receipt") }
        precondition(first.start == 203 && first.end == 206 && first.originalHex == "616e61")
        precondition(utf8.reader.page?.start == 71 && utf8.reader.page?.identity.pathBytes == f.rawPath)
        precondition(utf8.reader.page?.selection == nil && utf8.canUseMatch && !utf8.reader.canRefresh)
        let utf16 = try await model.openHit(page.hits[3], from: page, readerTransport: f.readerTransport())
        guard let second = utf16.match else { preconditionFailure("UTF-16 hit lost its exact original-byte match") }
        precondition(second.start == 410 && second.end == 416 && second.originalHex == "61006e006100")
        precondition(utf16.reader.page?.start == 278 && utf16.reader.page?.identity.encoding == "utf16le")
        precondition(utf16.reader.page?.originalHex == f.hex(f.searchUTF16[278...]))
        precondition(model.relationshipGeneration == links.generation && model.queryGeneration == page.generation)
        let borrowed = try await utf16.withMatchReader { $0 }
        precondition(borrowed == second.identity.owner)

        // This selected FEFF is inside UTF-16BE content. Treating its two
        // witness bytes as a separately BOM-prefixed document would erase it.
        model.searchInput = "\u{feff}"; model.search()
        await wait("in-content BOM search did not finish") { !model.busy }
        let bomPage = model.searchPage!
        let bom = try await model.openHit(bomPage.hits[0], from: bomPage, readerTransport: f.readerTransport())
        precondition(bom.match?.start == 4 && bom.match?.end == 6 && bom.match?.originalHex == "feff")
        precondition(bom.reader.page?.identity.encoding == "utf16be" && bom.canUseMatch)
        precondition(bom.reader.page?.start == 2 && bom.reader.page?.boundariesAdjusted == true)
        precondition(bom.reader.page?.originalHex == "0061feff0062")
        precondition(model.relationshipGeneration == 1 && model.queryGeneration == 2)
        model.closeArchive()
        precondition(model.info == nil && utf16.canUseMatch && utf8.canUseMatch)
        let independent = try await utf16.withMatchReader { $0 }
        precondition(independent == second.identity.owner)
        utf16.reader.offsetInput = "406"; utf16.reader.goToByte()
        await wait("retained UTF-16 reader did not navigate after archive close") { !utf16.reader.busy }
        precondition(utf16.reader.page?.originalHex == f.hex(f.searchUTF16[406...]))
        precondition(utf16.canUseMatch, "viewport movement replaced the original source identity")
        precondition(!f.calls.contains("LIVE-OPEN"))
        utf8.reader.close(); utf16.reader.close(); bom.reader.close()
        precondition(!utf16.canUseMatch)
        await refused("a closed reader still lent a saved-match capability") { _ = try await utf16.withMatchReader { $0 } }
        model.close()
    }

    @MainActor private static func frozenArchiveQueryPageAndEditorRejectStaleHits() async {
        let f = SavedRepositoryFixture(searchVectors: true), model = await prepared(f)
        let old = model.searchPage!
        model.searchInput = "another query"
        await refused("edited query text left an old hit actionable") {
            _ = try await model.openHit(old.hits[0], from: old, readerTransport: f.readerTransport())
        }
        model.searchInput = "ana"; model.search()
        await wait("replacement query did not finish") { !model.busy }
        await refused("old query generation rebound through equal occurrences") {
            _ = try await model.openHit(old.hits[0], from: old, readerTransport: f.readerTransport())
        }
        let current = model.searchPage!
        model.loadSearchResults(start: 2, from: current)
        await wait("result pagination did not finish") { !model.busy }
        precondition(model.searchPage?.start == 2 && model.searchPage?.hits.map(\.id) == [3, 4])
        await refused("an old page row bypassed current result-page admission") {
            _ = try await model.openHit(current.hits[0], from: current, readerTransport: f.readerTransport())
        }
        let last = model.searchPage!
        await refused("foreign reader registry accepted a saved hit") {
            _ = try await model.openHit(last.hits[0], from: last, readerTransport: f.readerTransport(domain: UUID()))
        }
        precondition(!f.calls.contains(where: { $0.hasPrefix("reader-create:") || $0.hasPrefix("handoff:") }))
        model.open(path: "/saved/equal-archive.fcbs", accessLease: nil)
        await wait("archive replacement did not finish") { !model.busy }
        model.searchInput = "ana"; model.search()
        await wait("replacement archive query did not finish") { !model.busy }
        precondition(model.queryGeneration == 1 && model.info?.identity.owner == 41)
        precondition(model.info?.identity.digest == old.identity.digest)
        await refused("old archive row rebound into equal bytes, query, member and range") {
            _ = try await model.openHit(old.hits[0], from: old, readerTransport: f.readerTransport())
        }
        precondition(!f.calls.contains(where: { $0.hasPrefix("reader-create:") || $0.hasPrefix("handoff:") }))
        model.close()
    }

    @MainActor private static func failedAndLostRepliesReconcileWithoutRepeatingSearch() async {
        let f = SavedRepositoryFixture(searchVectors: true), model = await prepared(f)
        let old = model.searchPage!, links = model.relationships!
        f.corruptNext("search"); model.search()
        await wait("refused query did not reconcile") { !model.busy }
        precondition(model.queryGeneration == old.generation && model.searchPage?.hits.map(\.id) == old.hits.map(\.id))
        precondition(model.relationshipGeneration == links.generation)
        f.corruptNext("search-lost"); model.search()
        await wait("lost accepted search reply did not recover") { !model.busy }
        precondition(model.queryGeneration == 3 && model.searchPage?.generation == 3)
        precondition(f.calls.filter { $0 == "search:40:3:ana" }.count == 1)
        precondition(f.calls.contains("results:40:3:0"))
        let accepted = model.searchPage!
        f.corruptNext("results"); model.loadSearchResults(start: 2, from: accepted)
        await wait("failed result page did not finish") { !model.busy }
        precondition(model.searchPage?.start == 0 && model.queryGeneration == 3)
        model.prepareRelationships()
        await wait("independent relationship replacement did not finish") { !model.busy }
        precondition(model.relationshipGeneration == 2 && model.queryGeneration == 3)
        f.corruptNext("clear-lost"); model.clearSearch()
        await wait("unconfirmed clear did not reconcile") { !model.busy }
        precondition(model.queryGeneration == nil && model.searchPage == nil && model.relationshipGeneration == 2)
        precondition(f.calls.filter { $0 == "clear-search:40:4" }.count == 1)
        precondition(model.relationships?.links.map(\.id) == links.links.map(\.id))
        model.close()
    }

    @MainActor private static func canceledSearchKeepsAcceptedQueriesAndRelationships() async {
        let f = SavedRepositoryFixture(searchVectors: true), model = await prepared(f)
        f.corruptNext("search"); f.blockNextSearch(); model.search()
        await wait("query did not enter the worker") { f.entered.wait(timeout: .now()) == .success }
        model.cancel()
        precondition(model.busy && model.queryGeneration == 1 && model.relationshipGeneration == 1)
        f.resume.signal()
        await wait("canceled query did not drain and reconcile") { !model.busy }
        precondition(model.queryGeneration == 1 && model.searchPage?.hits.count == 4 && model.relationshipGeneration == 1)
        precondition(f.calls.filter { $0 == "search:40:2:ana" }.count == 1)
        precondition(!f.calls.contains(where: { $0.hasPrefix("reader-create:") }))
        model.close()
    }

    @MainActor private static func invalidMetadataAndHitReceiptsNeverPublishReaders() async {
        let f = SavedRepositoryFixture(searchVectors: true), model = await prepared(f), page = model.searchPage!
        f.corruptNext("metadata")
        await refused("changed hit member metadata reached reader admission") {
            _ = try await model.openHit(page.hits[0], from: page, readerTransport: f.readerTransport())
        }
        precondition(!f.calls.contains(where: { $0.hasPrefix("reader-create:") }))
        model.close()
        for fault in ["lost", "hit-generation", "hit-id", "hit-domain", "hit-range", "hit-hex", "hit-context",
                      "digest", "nested-owner", "nested-path", "inherited-query", "page-bytes"] {
            let f = SavedRepositoryFixture(searchVectors: true), model = await prepared(f), page = model.searchPage!
            f.corruptNext(fault)
            await refused("corrupt saved match was published: \(fault)") {
                _ = try await model.openHit(page.hits[0], from: page, readerTransport: f.readerTransport())
            }
            precondition(model.queryGeneration == 1 && model.relationshipGeneration == 1 && !model.busy)
            await wait("rejected match destination did not retire: \(fault)") { f.calls.contains("reader-close:100") }
            precondition(f.calls.filter { $0 == "handoff:100" }.count == 1 && !f.calls.contains("LIVE-OPEN"))
            model.close()
        }
    }

    @MainActor private static func closeDuringHitHandoffDrainsAndRetires() async {
        let f = SavedRepositoryFixture(searchVectors: true), model = await prepared(f), page = model.searchPage!
        f.blockNextHandoff()
        let pending = Task {
            try await model.openHit(page.hits[2], from: page, readerTransport: f.readerTransport())
        }
        await wait("hit handoff did not enter") { f.entered.wait(timeout: .now()) == .success }
        model.close()
        precondition(!f.calls.contains("saved-close:40"))
        f.resume.signal()
        await refused("late hit appeared after archive closure") { _ = try await pending.value }
        await wait("closed archive or late hit reader did not retire") {
            f.calls.contains("reader-close:100") && f.calls.contains("saved-close:40")
        }
        precondition(model.closed && model.info == nil && model.queryGeneration == nil && !f.calls.contains("LIVE-OPEN"))
    }

    private static func expressionCoverageAndNamespaceContracts() throws {
        let f = SavedRepositoryFixture(searchVectors: true), archivePath = Array("/saved/repository.fcbs".utf8)
        let info = try AtlasSavedInfo.decode(f.encode(f.info(40)), handle: 40, expectedPath: archivePath, fresh: true)
        func decode(_ object: [String: Any], query: String = "ana +banana -unsafe") throws -> AtlasSavedExpressionPage {
            try AtlasSavedExpressionPage.decode(f.encode(object), identity: info.identity,
                generation: 1, expression: query, start: 0, limit: 64)
        }
        let full = try decode(f.expressionPage(40, generation: 1))
        precondition(full.state == "complete" && !full.complete && full.unavailable == 1)
        precondition(full.required == 1 && full.excluded == 1 && full.primaryNeedle == "ana")
        precondition(full.scannedBytes > full.sourceBytes, "predicate work was confused with unique payload I/O")
        let scoped = try decode(f.expressionPage(40, generation: 1, query: "ana path:raw"), query: "ana path:raw")
        precondition(scoped.complete && scoped.scopeFiles == 1 && scoped.filteredFiles == 4 && scoped.hits.count == 2)

        var limited = f.expressionPage(40, generation: 1)
        limited["state"] = "work-limit"; limited["max_scan_bytes"] = limited["scanned_bytes"]
        limited["retained_hits"] = "2"; limited["matches_seen"] = "2"
        limited["hits"] = Array((limited["hits"] as! [[String: Any]]).prefix(2))
        let earlier = try decode(limited)
        precondition(!earlier.complete && !earlier.truncated && earlier.hits.count == 2 && earlier.seen == 2)
        var zero = limited
        zero["max_scan_bytes"] = "0"; zero["scanned_bytes"] = "0"; zero["hits"] = [[String: Any]]()
        zero["retained_hits"] = "0"; zero["matches_seen"] = "0"; zero["members_loaded"] = "0"
        zero["member_bytes_read"] = "0"; zero["loaded_bytes"] = "0"
        zero["peak_source_bytes"] = "0"; zero["member_read_calls"] = "0"
        zero["predicate_scans"] = "0"; zero["predicate_rejections"] = "0"; zero["primary_scans"] = "0"
        let empty = try decode(zero)
        precondition(empty.state == "work-limit" && !empty.complete && empty.hits.isEmpty && empty.sourceBytes == 0)
        var truncated = limited
        truncated["state"] = "truncated"; truncated["truncated"] = true; truncated["matches_seen"] = "3"
        let capped = try decode(truncated)
        precondition(capped.truncated && capped.seen == 3 && !capped.complete)

        for field in ["selection_namespace", "expression_generation", "predicate_scope", "index_used",
                      "complete", "matches_seen_exact", "scanned_bytes", "metadata_excluded", "required_terms",
                      "retained_source_payload_bytes", "primary_needle", "next_offset", "member_read_calls"] {
            var bad = f.expressionPage(40, generation: 1)
            switch field {
            case "selection_namespace": bad[field] = "saved-literal"
            case "expression_generation": bad[field] = "2"
            case "predicate_scope": bad[field] = "visible-window"
            case "index_used", "complete", "matches_seen_exact": bad[field] = true
            case "scanned_bytes": bad[field] = "67108865"
            case "metadata_excluded", "retained_source_payload_bytes": bad[field] = "1"
            case "required_terms": bad[field] = "64"
            case "primary_needle": bad[field] = ""
            case "member_read_calls": bad[field] = "04"
            default: bad.removeValue(forKey: field)
            }
            rejected("forged expression coverage accepted: \(field)") { _ = try decode(bad) }
        }
        var differentBytes = f.expressionPage(40, generation: 1)
        differentBytes["expression"] = "e\u{301}"
        rejected("canonical string equality rebound an expression") { _ = try decode(differentBytes, query: "é") }
        var tooMany = f.expressionPage(40, generation: 1)
        tooMany["hits"] = Array(repeating: f.hitObject(1), count: 129)
        rejected("expression decoder admitted an oversized page") { _ = try decode(tooMany) }
        var clear = f.header(40, "expression-clear")
        clear["selection_namespace"] = "saved-expression"; clear["expression_generation"] = "2"
        clear["accepted_expression_generation"] = NSNull(); clear["retained_hits"] = "0"
        try AtlasSavedExpressionPage.decodeClear(f.encode(clear), identity: info.identity, generation: 2)
        rejected("a clear receipt from a different attempt was accepted") {
            try AtlasSavedExpressionPage.decodeClear(f.encode(clear), identity: info.identity, generation: 3)
        }
        clear.removeValue(forKey: "accepted_expression_generation")
        rejected("omitted expression authority became a successful clear") {
            try AtlasSavedExpressionPage.decodeClear(f.encode(clear), identity: info.identity, generation: 2)
        }
        var legacy = f.info(40)
        legacy.removeValue(forKey: "accepted_expression_generation"); legacy.removeValue(forKey: "last_expression_attempt")
        let old = try AtlasSavedInfo.decode(f.encode(legacy), handle: 40, expectedPath: archivePath, fresh: true)
        precondition(!old.expressionStateKnown && old.expressionGeneration == nil)
        legacy["accepted_expression_generation"] = NSNull()
        rejected("half-present expression recovery metadata was accepted") {
            _ = try AtlasSavedInfo.decode(f.encode(legacy), handle: 40, expectedPath: archivePath)
        }
        rejected("an accepted expression beyond its attempted generation was accepted") {
            _ = try AtlasSavedInfo.decode(f.encode(f.info(40, expression: 2, expressionAttempt: 1)),
                handle: 40, expectedPath: archivePath)
        }
    }

    @MainActor private static func scopedExpressionsKeepExactReadersAndIndependentModes() async throws {
        let f = SavedRepositoryFixture(searchVectors: true), model = await preparedExpression(f)
        let expression = model.expressionPage!, literal = model.searchPage!
        precondition(expression.generation == literal.generation && expression.hits[0].matches(literal.hits[0]))
        model.searchMode = .literal; model.searchInput = "ana"
        precondition(model.searchIsCurrent && !model.expressionIsCurrent)
        await refused("literal mode authorized an expression occurrence with equal numeric IDs") {
            _ = try await model.openExpressionHit(expression.hits[0], from: expression, readerTransport: f.readerTransport())
        }
        model.searchMode = .expression; model.searchInput = expression.expression
        await refused("expression mode authorized a literal occurrence with equal numeric IDs") {
            _ = try await model.openHit(literal.hits[0], from: literal, readerTransport: f.readerTransport())
        }
        let utf8 = try await model.openExpressionHit(expression.hits[0], from: expression, readerTransport: f.readerTransport())
        let utf16 = try await model.openExpressionHit(expression.hits[3], from: expression, readerTransport: f.readerTransport())
        precondition(utf8.match?.start == 203 && utf8.match?.end == 206 && utf8.match?.originalHex == "616e61")
        precondition(utf16.match?.start == 410 && utf16.match?.end == 416 && utf16.match?.originalHex == "61006e006100")
        precondition(utf16.reader.page?.start == 278 && utf16.reader.page?.identity.encoding == "utf16le")
        guard case .expressionHit(let generation, let query, let primary, _) = utf16.origin else {
            preconditionFailure("expression reader lost its accepted predicate provenance")
        }
        precondition(generation == 1 && query == expression.expression && primary == "ana")
        model.searchInput = "ana path:raw"
        await refused("an edited expression left old occurrences actionable") {
            _ = try await model.openExpressionHit(expression.hits[0], from: expression, readerTransport: f.readerTransport())
        }
        model.search(); await wait("scoped expression did not finish") { !model.busy }
        precondition(model.expressionGeneration == 2 && model.expressionPage?.complete == true)
        await refused("an old expression generation rebound to equal scoped occurrences") {
            _ = try await model.openExpressionHit(expression.hits[0], from: expression, readerTransport: f.readerTransport())
        }
        let scoped = model.expressionPage!
        f.corruptNext("expression-results-primary"); model.loadExpressionResults(start: 1, from: scoped)
        await wait("changed primary page did not finish") { !model.busy }
        precondition(model.expressionPage?.start == 0 && model.expressionPage?.primaryNeedle == "ana")
        model.loadExpressionResults(start: 1, from: scoped)
        await wait("scoped pagination did not finish") { !model.busy }
        precondition(model.expressionPage?.hits.map(\.id) == [2])
        await refused("a row outside the current expression page reached reader admission") {
            _ = try await model.openExpressionHit(scoped.hits[0], from: scoped, readerTransport: f.readerTransport())
        }
        model.clearSearch(); await wait("expression clear did not finish") { !model.busy }
        precondition(model.expressionPage == nil && model.expressionGeneration == nil)
        precondition(model.queryGeneration == 1 && model.searchPage?.hits.count == 4 && model.relationshipGeneration == 1)
        model.closeArchive()
        precondition(utf8.canUseMatch && utf16.canUseMatch)
        let reader = try await utf16.withMatchReader { $0 }
        precondition(reader == utf16.match?.identity.owner)
        utf16.reader.offsetInput = "406"; utf16.reader.goToByte()
        await wait("expression reader did not navigate after archive close") { !utf16.reader.busy }
        precondition(utf16.reader.page?.originalHex == f.hex(f.searchUTF16[406...]) && utf16.canUseMatch)
        precondition(!f.calls.contains("LIVE-OPEN"))
        utf8.reader.close(); utf16.reader.close(); model.close()
    }

    @MainActor private static func expressionRecoveryCancellationAndArchiveReplacement() async {
        let f = SavedRepositoryFixture(searchVectors: true), model = await preparedExpression(f)
        let accepted = model.expressionPage!
        model.searchInput = "\"unterminated"; f.corruptNext("expression"); model.search()
        await wait("syntax refusal did not recover accepted expression") { !model.busy }
        precondition(model.expressionGeneration == 1 && model.expressionPage?.expression == accepted.expression)
        precondition(model.info?.lastExpressionAttempt == 2 && model.notice.contains("QUERY_SYNTAX_ERROR"))
        precondition(!model.expressionIsCurrent && model.queryGeneration == 1 && model.relationshipGeneration == 1)
        model.searchInput = accepted.expression; f.corruptNext("expression-lost"); model.search()
        await wait("lost accepted expression reply did not recover") { !model.busy }
        precondition(model.expressionGeneration == 3 && model.expressionPage?.generation == 3)
        precondition(f.calls.filter { $0 == "expression:40:3:\(accepted.expression)" }.count == 1)
        precondition(f.calls.contains("expression-results:40:3:0"))
        f.corruptNext("expression"); f.blockNextSearch(); model.search()
        await wait("expression did not enter cancelable worker") { f.entered.wait(timeout: .now()) == .success }
        model.cancel(); precondition(model.busy && model.expressionGeneration == 3)
        f.resume.signal(); await wait("canceled expression did not drain") { !model.busy }
        precondition(model.expressionGeneration == 3 && model.info?.lastExpressionAttempt == 4)
        f.corruptNext("expression-clear-lost"); model.clearSearch()
        await wait("unconfirmed expression clear did not reconcile") { !model.busy }
        precondition(model.expressionGeneration == nil && model.expressionPage == nil && model.info?.lastExpressionAttempt == 5)
        precondition(model.queryGeneration == 1 && model.relationshipGeneration == 1)
        precondition(f.calls.filter { $0 == "expression-clear:40:5" }.count == 1)
        model.searchInput = accepted.expression; model.search()
        await wait("expression after clear did not finish") { !model.busy }
        precondition(model.expressionGeneration == 6)
        let oldArchive = model.expressionPage!
        model.open(path: "/saved/equal-archive.fcbs", accessLease: nil)
        await wait("replacement expression archive did not open") { !model.busy }
        precondition(model.expressionPage == nil && model.expressionGeneration == nil && model.queryGeneration == nil)
        model.searchInput = accepted.expression; model.search()
        await wait("expression in new archive did not finish") { !model.busy }
        precondition(model.expressionGeneration == 1 && model.info?.identity.owner == 41)
        await refused("same-digest archive replacement rebound an old expression occurrence") {
            _ = try await model.openExpressionHit(oldArchive.hits[0], from: oldArchive, readerTransport: f.readerTransport())
        }
        precondition(!f.calls.contains(where: { $0.hasPrefix("reader-create:") }))
        model.close()
    }

    @MainActor private static func forgedExpressionReceiptsAndLateReadersRetire() async {
        for fault in ["expression-namespace", "expression-generation", "expression-primary", "expression-selection",
                      "hit-id", "hit-range", "hit-hex", "digest", "lost"] {
            let f = SavedRepositoryFixture(searchVectors: true), model = await preparedExpression(f)
            let page = model.expressionPage!
            f.corruptNext(fault)
            await refused("forged expression receipt published a reader: \(fault)") {
                _ = try await model.openExpressionHit(page.hits[0], from: page, readerTransport: f.readerTransport())
            }
            await wait("rejected expression reader did not retire: \(fault)") { f.calls.contains("reader-close:100") }
            precondition(model.expressionGeneration == 1 && model.queryGeneration == 1 && model.relationshipGeneration == 1)
            precondition(f.calls.filter { $0 == "expression-open:40:1:1" }.count == 1 && !f.calls.contains("LIVE-OPEN"))
            model.close()
        }
        let f = SavedRepositoryFixture(searchVectors: true), model = await preparedExpression(f), page = model.expressionPage!
        f.blockNextHandoff()
        let pending = Task { try await model.openExpressionHit(page.hits[2], from: page, readerTransport: f.readerTransport()) }
        await wait("expression reader handoff did not enter") { f.entered.wait(timeout: .now()) == .success }
        model.close(); precondition(!f.calls.contains("saved-close:40"))
        f.resume.signal()
        await refused("late expression reader appeared after closing its archive") { _ = try await pending.value }
        await wait("closed expression archive and late reader did not retire") {
            f.calls.contains("saved-close:40") && f.calls.contains("reader-close:100")
        }
        precondition(model.expressionGeneration == nil && model.expressionPage == nil && !f.calls.contains("LIVE-OPEN"))
    }
}
