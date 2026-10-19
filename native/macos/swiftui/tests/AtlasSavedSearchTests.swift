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
        print("Saved decoded search, UTF-8/UTF-16 original matches, frozen identity, recovery and cancellation regressions passed.")
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
}
