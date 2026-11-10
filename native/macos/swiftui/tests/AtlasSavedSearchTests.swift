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
        try structureCoverageAndEvidenceContracts()
        try await structuralModesKeepExactReadersAndIndependentState()
        await structureRecoveryCancellationAndArchiveReplacement()
        await forgedStructuralReceiptsAndLateReadersRetire()
        try await largeStructuralTargetsRemainBounded()
        try await structuralQueriesRemainIndependentOfSavedIndex()
        try indexReceiptsRequireArchiveCoverageAndTrustedPin()
        try await indexAttachmentPreservesQueriesAndRetainsGrants()
        await unknownIndexStateRetainsBothGrantsUntilReconciled()
        await damagedIndexKeepsAcceptedResultsAndCanDetach()
        await canceledAndClosedIndexAttachmentsDrainBeforeRelease()
        print("Saved text and structural search, index attachment and coverage, UTF-8/UTF-16 captures, independent generations, grant lifetimes, bounded navigation and recovery regressions passed.")
    }

    private static func indexReceiptsRequireArchiveCoverageAndTrustedPin() throws {
        let f = SavedRepositoryFixture(searchVectors: true), pin = String(repeating: "a", count: 64)
        let identity = try AtlasSavedInfo.decode(f.encode(f.info(40)), handle: 40,
            expectedPath: Array("/saved/repository.fcbs".utf8), fresh: true).identity
        let index = try AtlasSavedIndex.decodeAttach(f.encode(f.indexAttachment(40, generation: UInt64.max, pin: pin)),
            identity: identity, generation: UInt64.max, pin: pin)
        precondition(index.indexed == 3 && index.uncovered == 1 && index.unavailable == 1)
        precondition(index.pageBytes == 0 && index.cacheCapacity == 65_536)
        for field in ["owner", "archive_digest", "index_generation", "index_pin", "last_index_attempt", "index_members",
                      "index_indexed_files", "index_uncovered_files", "index_unavailable_files", "index_pin_scope",
                      "index_kind", "index_body_verified_on_open", "index_file_bytes", "index_manifest_bytes_read",
                      "index_page_bytes_read", "index_cache_capacity_bytes"] {
            var bad = f.indexAttachment(40, generation: 1, pin: pin)
            switch field {
            case "owner": bad[field] = "41"
            case "archive_digest", "index_pin": bad[field] = String(repeating: "b", count: 64)
            case "index_generation", "last_index_attempt": bad[field] = "2"
            case "index_members": bad[field] = "6"
            case "index_indexed_files": bad[field] = "4"
            case "index_uncovered_files", "index_unavailable_files": bad[field] = "0"
            case "index_body_verified_on_open": bad[field] = true
            case "index_file_bytes": bad[field] = "25165825"
            case "index_manifest_bytes_read": bad[field] = "4194337"
            case "index_page_bytes_read": bad[field] = "8"
            default: bad.removeValue(forKey: field)
            }
            rejected("forged attached index accepted: \(field)") {
                _ = try AtlasSavedIndex.decodeAttach(f.encode(bad), identity: identity, generation: 1, pin: pin)
            }
        }
        var info = f.info(40, index: 1, pin: pin, indexAttempt: 2)
        let accepted = try AtlasSavedInfo.decode(f.encode(info), handle: 40,
            expectedPath: Array("/saved/repository.fcbs".utf8), identity: identity)
        precondition(accepted.index?.generation == 1 && accepted.lastIndexAttempt == 2)
        let failedPage = try AtlasSavedInfo.decode(f.encode(f.info(40, index: 1, pin: pin,
            indexAttempt: 2, indexPageBytes: 16)), handle: 40, expectedPath: Array("/saved/repository.fcbs".utf8))
        precondition(failedPage.index?.pageBytes == 16 && failedPage.index?.pageLoads == 0)
        info["accepted_index_generation"] = NSNull()
        rejected("detached info retained contradictory index metadata") {
            _ = try AtlasSavedInfo.decode(f.encode(info), handle: 40, expectedPath: Array("/saved/repository.fcbs".utf8))
        }
        info = f.info(40, index: 2, pin: pin, indexAttempt: 1)
        rejected("accepted generation exceeded the attempted generation") {
            _ = try AtlasSavedInfo.decode(f.encode(info), handle: 40, expectedPath: Array("/saved/repository.fcbs".utf8))
        }
        var detach = f.header(40, "index-detach")
        detach["accepted_index_generation"] = NSNull(); detach["last_index_attempt"] = "3"
        try AtlasSavedIndex.decodeDetach(f.encode(detach), identity: identity, generation: 3)
        rejected("detach receipt rebound to another attempt") {
            try AtlasSavedIndex.decodeDetach(f.encode(detach), identity: identity, generation: 4)
        }
        var direct = f.searchPage(40, generation: 1)
        direct["index_eliminated_files"] = "1"
        rejected("direct scan claimed an index elimination") {
            _ = try AtlasSavedSearchPage.decode(f.encode(direct), identity: identity, generation: 1,
                needle: "ana", start: 0, limit: 64)
        }
    }

    private final class FileGrant {}
    @MainActor private static func indexAttachmentPreservesQueriesAndRetainsGrants() async throws {
        let f = SavedRepositoryFixture(searchVectors: true), model = await prepared(f)
        let direct = model.searchPage!, pin = String(repeating: "a", count: 64)
        var grant: FileGrant? = FileGrant(); weak var held = grant
        model.attachIndex(path: "/saved/index.fcbd", pin: pin, accessLease: grant); grant = nil
        await wait("index did not attach") { !model.busy }
        precondition(model.index?.generation == 1 && model.indexPath == "/saved/index.fcbd" && held != nil)
        precondition(model.queryGeneration == direct.generation && model.relationshipGeneration == 1)
        model.loadSearchResults(start: 0, from: direct)
        await wait("pre-index results did not paginate") { !model.busy }
        precondition(model.searchPage?.indexGeneration == nil)
        model.search()
        await wait("indexed search did not finish") { !model.busy }
        let indexed = model.searchPage!
        precondition(indexed.indexGeneration == 1 && indexed.indexPin == pin && indexed.indexEliminatedFiles == 1)
        precondition(indexed.fallbackFiles == 2 && indexed.postingEntriesVisited == 3)
        f.corruptNext("index-query-pin"); model.loadSearchResults(start: 0, from: indexed)
        await wait("forged query page did not finish") { !model.busy }
        precondition(model.searchPage?.indexPin == pin, "pagination rebound a query to another index")
        var refused: FileGrant? = FileGrant(); weak var discarded = refused
        f.corruptNext("index-refused")
        model.attachIndex(path: "/saved/wrong.fcbd", pin: pin, accessLease: refused); refused = nil
        await wait("refused replacement did not finish") { !model.busy }
        precondition(model.index?.generation == 1 && model.info?.lastIndexAttempt == 2 && held != nil)
        await wait("refused replacement kept its unused grant") { discarded == nil }
        var replacement: FileGrant? = FileGrant(); weak var retained = replacement
        let nextPin = String(repeating: "b", count: 64)
        f.corruptNext("index-lost")
        model.attachIndex(path: "/saved/new.fcbd", pin: nextPin, accessLease: replacement); replacement = nil
        await wait("lost attach reply did not reconcile") { !model.busy }
        precondition(model.index?.generation == 3 && model.index?.pin == nextPin && retained != nil)
        await wait("replaced index grant was retained") { held == nil }
        precondition(f.calls.filter { $0.hasPrefix("index-attach:") }.count == 3, "attachment replayed a pathname")
        f.corruptNext("index-detach-lost"); model.detachIndex()
        await wait("lost detach reply did not reconcile") { !model.busy }
        precondition(model.index == nil && model.info?.lastIndexAttempt == 4 && model.indexStateKnown)
        await wait("detached index grant did not retire") { retained == nil }
        model.loadSearchResults(start: 0, from: indexed)
        await wait("detached-index query page did not survive") { !model.busy }
        precondition(model.searchPage?.indexGeneration == 1 && model.searchPage?.indexPin == pin)
        let capture = try await model.openHit(indexed.hits[0], from: model.searchPage!, readerTransport: f.readerTransport())
        precondition(capture.match?.start == 203, "detaching the index changed exact captured match navigation")
        model.close(); capture.reader.close()
    }

    @MainActor private static func unknownIndexStateRetainsBothGrantsUntilReconciled() async {
        let f = SavedRepositoryFixture(searchVectors: true), model = await prepared(f)
        let pin = String(repeating: "a", count: 64)
        var first: FileGrant? = FileGrant(); weak var previous = first
        model.attachIndex(path: "/saved/first.fcbd", pin: pin, accessLease: first); first = nil
        await wait("initial index did not attach") { !model.busy }
        var second: FileGrant? = FileGrant(); weak var candidate = second
        f.corruptNext("index-unknown")
        model.attachIndex(path: "/saved/second.fcbd", pin: String(repeating: "b", count: 64), accessLease: second); second = nil
        await wait("unknown attachment did not finish") { !model.busy }
        precondition(!model.indexStateKnown && !model.canChangeIndex && !model.canSearch)
        precondition(previous != nil && candidate != nil, "unconfirmed acceptance released an index grant")
        let attempts = f.calls.filter { $0.hasPrefix("index-attach:") }.count
        model.attachIndex(path: "/saved/third.fcbd", pin: pin, accessLease: nil); model.search()
        precondition(!model.busy && f.calls.filter { $0.hasPrefix("index-attach:") }.count == attempts)
        model.reloadIndex()
        await wait("index status did not recover") { !model.busy }
        precondition(model.indexStateKnown && model.index?.generation == 2 && candidate != nil)
        await wait("old grant survived accepted replacement") { previous == nil }
        f.corruptNext("index-unknown"); model.detachIndex()
        await wait("unknown detach did not finish") { !model.busy }
        precondition(!model.indexStateKnown && candidate != nil)
        model.reloadIndex()
        await wait("detached status did not recover") { !model.busy }
        precondition(model.indexStateKnown && model.index == nil)
        await wait("detached grant survived reconciliation") { candidate == nil }
        model.close()
    }

    @MainActor private static func canceledAndClosedIndexAttachmentsDrainBeforeRelease() async {
        for closing in [false, true] {
            let f = SavedRepositoryFixture(), model = AtlasSavedRepositoryModel(transport: f.savedTransport)
            var source: FileGrant? = FileGrant(); weak var archiveGrant = source
            model.open(path: "/saved/repository.fcbs", accessLease: source); source = nil
            await wait("archive grant fixture did not open") { !model.busy }
            var selected: FileGrant? = FileGrant(); weak var indexGrant = selected
            f.blockNextIndex()
            model.attachIndex(path: "/saved/pending.fcbd", pin: String(repeating: "a", count: 64), accessLease: selected)
            selected = nil
            await wait("index call did not enter worker") { f.entered.wait(timeout: .now()) == .success }
            if closing { model.close() } else { model.cancel() }
            precondition(archiveGrant != nil && indexGrant != nil && !f.calls.contains("saved-close:40"))
            f.resume.signal()
            await wait("entered index work did not drain") { !model.busy }
            if !closing {
                precondition(model.indexStateKnown && model.index?.generation == 1 && indexGrant != nil,
                    "cancel after acceptance dropped the accepted index grant")
                model.close()
            }
            await wait("saved resources did not retire together") {
                f.calls.contains("saved-close:40") && archiveGrant == nil && indexGrant == nil
            }
        }
    }

    @MainActor private static func damagedIndexKeepsAcceptedResultsAndCanDetach() async {
        let f = SavedRepositoryFixture(searchVectors: true), model = await prepared(f)
        model.attachIndex(path: "/saved/damaged.fcbd", pin: String(repeating: "a", count: 64), accessLease: nil)
        await wait("damage fixture index did not attach") { !model.busy }
        f.corruptNext("index-page-failed"); model.search()
        await wait("failed page verification did not reconcile") { !model.busy }
        precondition(model.indexStateKnown && model.queryGeneration == 1)
        precondition(model.index?.pageBytes == 16 && model.index?.pageLoads == 0 && model.canChangeIndex)
        model.detachIndex()
        await wait("damaged index could not be detached") { !model.busy }
        precondition(model.index == nil && model.queryGeneration == 1)
        model.close()
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

    private static func structureCoverageAndEvidenceContracts() throws {
        let f = SavedRepositoryFixture(searchVectors: true, headingVectors: true)
        let path = Array("/saved/repository.fcbs".utf8)
        let info = try AtlasSavedInfo.decode(f.encode(f.info(40)), handle: 40, expectedPath: path, fresh: true)
        precondition(info.structureStateKnown && info.structureGeneration == nil && info.lastStructureAttempt == 0)
        func decode(_ object: [String: Any], mode: AtlasSavedSearchMode = .symbols,
            name: AtlasSearchNameMode = .contains, needle: String = "banana") throws -> AtlasSavedStructurePage {
            try AtlasSavedStructurePage.decode(f.encode(object), identity: info.identity, generation: 1,
                mode: mode, nameMode: name, needle: needle, start: 0, limit: 64)
        }
        let page = try decode(f.structurePage(40, generation: 1))
        precondition(page.hits.count == 1 && page.hits[0].structure.name == "crate::banana")
        precondition(page.hits[0].hit.start == 202 && page.hits[0].hit.end == 208)
        precondition(page.hits[0].structure.evidenceEnd == 209 && page.unsupported == 3 && page.unavailable == 1)
        precondition(!page.complete && !page.truncated && page.seen == 1 && page.loaded == 1 && page.searched == 1)
        let heading = try decode(f.structurePage(40, generation: 1, mode: .headings, needle: "Café"),
            mode: .headings, needle: "Café")
        precondition(heading.hits[0].structure.headingSlug == "caf-details" && heading.hits[0].hit.start == 207)
        let occurrences = try decode(f.structurePage(40, generation: 1, mode: .occurrences,
            nameMode: .exact, needle: f.occurrenceNeedle), mode: .occurrences, name: .exact, needle: f.occurrenceNeedle)
        precondition(occurrences.hits.map(\.hit.start) == [0, 2] && occurrences.hits.map(\.hit.end) == [208, 416])

        var truncated = f.structurePage(40, generation: 1)
        truncated["state"] = "truncated"; truncated["truncated"] = true; truncated["matches_seen"] = "2"
        let capped = try decode(truncated)
        precondition(capped.truncated)
        var omitted = f.structurePage(40, generation: 1)
        omitted["metadata_omitted"] = "1"; omitted["matches_seen"] = "2"
        let boundedMetadata = try decode(omitted)
        precondition(boundedMetadata.metadataOmitted == 1)
        var zero = f.structurePage(40, generation: 1)
        zero["state"] = "work-limit"; zero["max_source_bytes"] = "0"
        zero["members_visited"] = "2"; zero["unsupported_files"] = "1"; zero["unavailable_files"] = "0"
        zero["members_loaded"] = "0"; zero["files_searched"] = "0"
        for field in ["loaded_bytes", "peak_source_bytes", "metadata_bytes", "member_bytes_read", "member_read_calls",
                      "retained_hits", "matches_seen"] { zero[field] = "0" }
        zero["hits"] = [[String: Any]]()
        let empty = try decode(zero)
        precondition(empty.hits.isEmpty && empty.sourceBytes == 0 && !empty.complete && empty.state == "work-limit")

        for field in ["selection_namespace", "structure_generation", "mode", "requested_mode", "realized_mode",
                      "evidence_level", "semantic_complete", "name_case", "name_mode", "scope", "completeness_basis",
                      "index_policy", "complete", "matches_seen_exact", "members_visited", "members_loaded",
                      "files_searched", "loaded_bytes", "metadata_bytes", "peak_source_bytes",
                      "retained_source_payload_bytes", "next_offset", "member_read_calls"] {
            var bad = f.structurePage(40, generation: 1)
            switch field {
            case "selection_namespace": bad[field] = "saved-expression"
            case "structure_generation": bad[field] = "2"
            case "mode": bad[field] = "markdown-heading"
            case "requested_mode": bad[field] = "literal"
            case "realized_mode", "evidence_level": bad[field] = "compiler-resolved"
            case "semantic_complete", "complete", "matches_seen_exact": bad[field] = true
            case "name_case": bad[field] = "insensitive"
            case "name_mode": bad[field] = "prefix"
            case "scope": bad[field] = "live-project"
            case "completeness_basis": bad[field] = "semantic-completeness"
            case "index_policy": bad[field] = "live-index"
            case "members_visited": bad[field] = "6"
            case "members_loaded": bad[field] = "5"
            case "files_searched": bad[field] = "2"
            case "loaded_bytes": bad[field] = "67108865"
            case "metadata_bytes": bad[field] = "8388609"
            case "peak_source_bytes": bad[field] = "65537"
            case "retained_source_payload_bytes": bad[field] = "1"
            case "member_read_calls": bad[field] = "01"
            default: bad.removeValue(forKey: field)
            }
            rejected("forged structural coverage accepted: \(field)") { _ = try decode(bad) }
        }
        for field in ["candidate_id", "heading_slug", "evidence_level", "evidence_range", "name"] {
            var bad = f.structurePage(40, generation: 1)
            var hits = bad["hits"] as! [[String: Any]], witness = hits[0]["structure"] as! [String: Any]
            switch field {
            case "candidate_id": witness[field] = "0"
            case "heading_slug": witness[field] = "foreign-heading"
            case "evidence_level": witness[field] = "whole-token-text-candidate"
            case "evidence_range": witness[field] = ["start": "203", "end": "208"]
            default: witness[field] = String(repeating: "x", count: 4097)
            }
            hits[0]["structure"] = witness; bad["hits"] = hits
            rejected("forged structural witness accepted: \(field)") { _ = try decode(bad) }
        }
        var wrongPolicy = f.structurePage(40, generation: 1, mode: .occurrences, nameMode: .exact, needle: f.occurrenceNeedle)
        wrongPolicy["compiler_resolved"] = true
        rejected("occurrences became compiler references") {
            _ = try decode(wrongPolicy, mode: .occurrences, name: .exact, needle: f.occurrenceNeedle)
        }
        var tooMany = f.structurePage(40, generation: 1)
        tooMany["hits"] = Array(repeating: f.structureHitObject(1, mode: .symbols, needle: "banana"), count: 129)
        rejected("oversized structural page accepted") { _ = try decode(tooMany) }
        var normalized = f.structurePage(40, generation: 1, mode: .headings, needle: "Café")
        normalized["needle"] = "Cafe\u{301}"
        rejected("Unicode equivalence rebound structural query bytes") { _ = try decode(normalized, mode: .headings, needle: "Café") }

        var clear = f.header(40, "structure-clear")
        clear["selection_namespace"] = "saved-structure"; clear["structure_generation"] = "2"
        clear["accepted_structure_generation"] = NSNull(); clear["retained_hits"] = "0"
        try AtlasSavedStructurePage.decodeClear(f.encode(clear), identity: info.identity, generation: 2)
        rejected("foreign structural clear attempt accepted") {
            try AtlasSavedStructurePage.decodeClear(f.encode(clear), identity: info.identity, generation: 3)
        }
        clear.removeValue(forKey: "accepted_structure_generation")
        rejected("omitted structural authority became an accepted clear") {
            try AtlasSavedStructurePage.decodeClear(f.encode(clear), identity: info.identity, generation: 2)
        }
        var legacy = f.info(40)
        legacy.removeValue(forKey: "accepted_structure_generation"); legacy.removeValue(forKey: "last_structure_attempt")
        let old = try AtlasSavedInfo.decode(f.encode(legacy), handle: 40, expectedPath: path)
        precondition(!old.structureStateKnown && old.structureGeneration == nil)
        legacy["accepted_structure_generation"] = NSNull()
        rejected("half-present structural recovery state was accepted") {
            _ = try AtlasSavedInfo.decode(f.encode(legacy), handle: 40, expectedPath: path)
        }
        rejected("accepted structural state exceeded its attempted generation") {
            _ = try AtlasSavedInfo.decode(f.encode(f.info(40, structure: 2, structureAttempt: 1)), handle: 40, expectedPath: path)
        }
    }

    @MainActor private static func preparedStructure(_ f: SavedRepositoryFixture,
        mode: AtlasSavedSearchMode = .headings, needle: String = "Café") async -> AtlasSavedRepositoryModel {
        let model = AtlasSavedRepositoryModel(transport: f.savedTransport)
        model.open(path: "/saved/repository.fcbs", accessLease: nil)
        await wait("structural archive did not open") { !model.busy }
        model.searchMode = mode; model.searchInput = needle; model.search()
        await wait("structural query did not finish") { !model.busy }
        precondition(model.structureGeneration == 1 && model.structurePage?.hits.isEmpty == false)
        return model
    }

    @MainActor private static func structuralModesKeepExactReadersAndIndependentState() async throws {
        let f = SavedRepositoryFixture(searchVectors: true, headingVectors: true)
        let model = await preparedExpression(f), literal = model.searchPage!, expression = model.expressionPage!
        model.searchMode = .symbols; model.searchNameMode = .exact; model.searchInput = "crate::banana"; model.search()
        await wait("symbol query did not finish") { !model.busy }
        let symbols = model.structurePage!
        precondition(symbols.generation == 1 && symbols.nameMode == .exact && model.structureIsCurrent)
        await refused("structural mode authorized a literal row with equal numeric identities") {
            _ = try await model.openHit(literal.hits[0], from: literal, readerTransport: f.readerTransport())
        }
        await refused("structural mode authorized an expression row with equal numeric identities") {
            _ = try await model.openExpressionHit(expression.hits[0], from: expression, readerTransport: f.readerTransport())
        }
        let symbolReader = try await model.openStructureHit(symbols.hits[0], from: symbols, readerTransport: f.readerTransport())
        precondition(symbolReader.match?.originalHex == "62616e616e61", "qualified names were incorrectly treated as literal source spelling")
        precondition(symbolReader.match?.start == 202 && symbolReader.match?.end == 208)
        model.searchNameMode = .prefix
        precondition(!model.structureIsCurrent)
        await refused("editing name matching left structural rows actionable") {
            _ = try await model.openStructureHit(symbols.hits[0], from: symbols, readerTransport: f.readerTransport())
        }
        model.searchMode = .headings; model.searchNameMode = .contains; model.searchInput = "Café"
        await refused("a heading draft authorized stale symbol results") {
            _ = try await model.openStructureHit(symbols.hits[0], from: symbols, readerTransport: f.readerTransport())
        }
        model.search(); await wait("heading query did not finish") { !model.busy }
        let headings = model.structurePage!
        precondition(headings.generation == 2 && headings.hits[0].structure.headingSlug == "caf-details")
        let headingReader = try await model.openStructureHit(headings.hits[0], from: headings, readerTransport: f.readerTransport())
        precondition(headingReader.match?.start == 207 && headingReader.match?.end == 223)
        precondition(headingReader.match?.originalHex == f.hex(f.headingBytes[207..<223]))
        precondition(headingReader.reader.page?.start == 75)

        model.searchMode = .occurrences; model.searchNameMode = .prefix; model.searchInput = f.occurrenceNeedle; model.search()
        await wait("identifier occurrences did not finish") { !model.busy }
        let occurrences = model.structurePage!
        precondition(occurrences.generation == 3 && occurrences.nameMode == .exact && model.structureIsCurrent)
        let utf16 = try await model.openStructureHit(occurrences.hits[1], from: occurrences, readerTransport: f.readerTransport())
        precondition(utf16.match?.start == 2 && utf16.match?.end == 416 && utf16.reader.page?.identity.encoding == "utf16le")
        precondition(utf16.match?.originalHex == f.hex(f.searchUTF16[2..<416]))
        let reads = f.calls.filter { $0.hasPrefix("structure:") }.count
        model.loadStructureResults(start: 1, from: occurrences)
        await wait("structural metadata pagination did not finish") { !model.busy }
        precondition(model.structurePage?.hits.map(\.id) == [2])
        precondition(f.calls.filter { $0.hasPrefix("structure:") }.count == reads)
        await refused("an older structural page remained actionable after pagination") {
            _ = try await model.openStructureHit(occurrences.hits[0], from: occurrences, readerTransport: f.readerTransport())
        }
        model.clearSearch(); await wait("structural clear did not finish") { !model.busy }
        precondition(model.structurePage == nil && model.structureGeneration == nil)
        precondition(model.queryGeneration == 1 && model.expressionGeneration == 1 && model.relationshipGeneration == 1)
        model.closeArchive()
        precondition(symbolReader.canUseMatch && headingReader.canUseMatch && utf16.canUseMatch)
        utf16.reader.offsetInput = "406"; utf16.reader.goToByte()
        await wait("structural reader did not survive archive close") { !utf16.reader.busy }
        precondition(utf16.reader.page?.originalHex == f.hex(f.searchUTF16[406...]) && !f.calls.contains("LIVE-OPEN"))
        symbolReader.reader.close(); headingReader.reader.close(); utf16.reader.close(); model.close()
    }

    @MainActor private static func structureRecoveryCancellationAndArchiveReplacement() async {
        let f = SavedRepositoryFixture(searchVectors: true, headingVectors: true)
        let model = await preparedStructure(f), accepted = model.structurePage!
        model.searchMode = .occurrences; model.searchInput = "invalid token"; f.corruptNext("structure"); model.search()
        await wait("structural refusal did not recover") { !model.busy }
        precondition(model.structureGeneration == 1 && model.structurePage?.mode == .headings && !model.structureIsCurrent)
        precondition(model.info?.lastStructureAttempt == 2 && model.notice.contains("REFERENCE_INVALID_NAME"))
        model.searchMode = .headings; model.searchInput = accepted.needle; f.corruptNext("structure-lost"); model.search()
        await wait("lost structural acceptance did not recover") { !model.busy }
        precondition(model.structureGeneration == 3 && model.structureIsCurrent)
        precondition(f.calls.filter { $0.hasPrefix("structure:40:3:") }.count == 1)
        precondition(f.calls.contains("structure-results:40:3:0"))
        f.corruptNext("structure"); f.blockNextSearch(); model.search()
        await wait("structural scan did not enter worker") { f.entered.wait(timeout: .now()) == .success }
        model.cancel(); precondition(model.busy && model.structureGeneration == 3)
        f.resume.signal(); await wait("canceled structural scan did not drain") { !model.busy }
        precondition(model.structureGeneration == 3 && model.info?.lastStructureAttempt == 4)
        f.corruptNext("structure-clear-lost"); model.clearSearch()
        await wait("lost structural clear did not reconcile") { !model.busy }
        precondition(model.structurePage == nil && model.structureGeneration == nil && model.info?.lastStructureAttempt == 5)
        precondition(f.calls.filter { $0 == "structure-clear:40:5" }.count == 1)
        model.searchInput = accepted.needle; model.search()
        await wait("structural query after clear did not finish") { !model.busy }
        precondition(model.structureGeneration == 6)
        let old = model.structurePage!
        model.open(path: "/saved/equal-archive.fcbs", accessLease: nil)
        await wait("replacement structural archive did not open") { !model.busy }
        precondition(model.structurePage == nil && model.structureGeneration == nil)
        model.searchInput = accepted.needle; model.search()
        await wait("new archive structural query did not finish") { !model.busy }
        precondition(model.structureGeneration == 1 && model.info?.identity.owner == 41)
        await refused("same-digest archive replacement rebound an old structural result") {
            _ = try await model.openStructureHit(old.hits[0], from: old, readerTransport: f.readerTransport())
        }
        precondition(!f.calls.contains(where: { $0.hasPrefix("reader-create:") }))
        model.close()
    }

    @MainActor private static func forgedStructuralReceiptsAndLateReadersRetire() async {
        for fault in ["structure-namespace", "structure-generation", "structure-mode", "structure-name-mode",
                      "structure-needle", "structure-witness", "structure-selection", "hit-id", "hit-domain", "hit-range",
                      "hit-hex", "digest", "live", "page-bytes", "lost"] {
            let f = SavedRepositoryFixture(headingVectors: true), model = await preparedStructure(f), page = model.structurePage!
            f.corruptNext(fault)
            await refused("forged structural receipt published a reader: \(fault)") {
                _ = try await model.openStructureHit(page.hits[0], from: page, readerTransport: f.readerTransport())
            }
            await wait("rejected structural destination did not retire: \(fault)") { f.calls.contains("reader-close:100") }
            precondition(model.structureGeneration == 1 && !f.calls.contains("LIVE-OPEN"))
            precondition(f.calls.filter { $0 == "structure-open:40:1:1" }.count == 1)
            model.close()
        }
        let f = SavedRepositoryFixture(headingVectors: true), model = await preparedStructure(f), page = model.structurePage!
        f.blockNextHandoff()
        let pending = Task { try await model.openStructureHit(page.hits[0], from: page, readerTransport: f.readerTransport()) }
        await wait("structural reader did not enter") { f.entered.wait(timeout: .now()) == .success }
        model.close(); precondition(!f.calls.contains("saved-close:40"))
        f.resume.signal()
        await refused("late structural reader appeared after archive close") { _ = try await pending.value }
        await wait("late structural reader and archive did not retire") {
            f.calls.contains("saved-close:40") && f.calls.contains("reader-close:100")
        }
        precondition(model.structurePage == nil && !f.calls.contains("LIVE-OPEN"))
    }

    @MainActor private static func largeStructuralTargetsRemainBounded() async throws {
        let f = SavedRepositoryFixture(headingVectors: true, longHeading: true)
        let model = await preparedStructure(f, needle: "A"), page = model.structurePage!
        precondition(page.hits[0].hit.start == 0 && page.hits[0].hit.end == 3003)
        let capture = try await model.openStructureHit(page.hits[0], from: page, readerTransport: f.readerTransport())
        precondition(capture.match == nil && capture.reader.page?.end == 3004)
        precondition(capture.reader.canPin)
        f.corruptNext("structure-large-hex")
        await refused("large structural target invented a full selection witness") {
            _ = try await model.openStructureHit(page.hits[0], from: page, readerTransport: f.readerTransport())
        }
        await wait("invalid large-target destination did not retire") { f.calls.contains("reader-close:101") }
        model.closeArchive(); precondition(capture.reader.canPin)
        capture.reader.offsetInput = "2048"; capture.reader.goToByte()
        await wait("large structural reader did not navigate after archive close") { !capture.reader.busy }
        precondition(capture.reader.page?.start == 2048 && !f.calls.contains("LIVE-OPEN"))
        capture.reader.close(); model.close()
    }

    @MainActor private static func structuralQueriesRemainIndependentOfSavedIndex() async throws {
        let f = SavedRepositoryFixture(searchVectors: true, headingVectors: true)
        let model = await preparedStructure(f), initial = model.structurePage!
        model.attachIndex(path: "/saved/first.fcbd", pin: String(repeating: "a", count: 64), accessLease: nil)
        await wait("index did not attach alongside structural query") { !model.busy }
        precondition(model.index?.generation == 1 && model.structureGeneration == initial.generation)
        model.searchMode = .occurrences; model.searchInput = f.occurrenceNeedle; model.search()
        await wait("structural extraction with attached index did not finish") { !model.busy }
        precondition(model.structureGeneration == 2 && model.structurePage?.mode == .occurrences && model.index?.generation == 1)

        f.corruptNext("index-unknown")
        model.attachIndex(path: "/saved/replacement.fcbd", pin: String(repeating: "b", count: 64), accessLease: nil)
        await wait("unconfirmed index replacement did not finish") { !model.busy }
        precondition(!model.indexStateKnown)
        model.searchMode = .literal
        precondition(!model.canSearch)
        model.searchMode = .headings; model.searchInput = "Café"
        precondition(model.canSearch, "independent structural extraction waited for a literal-only index")
        model.search(); await wait("structural query was blocked by uncertain index state") { !model.busy }
        precondition(model.structureGeneration == 3 && model.structureIsCurrent && !model.indexStateKnown)
        model.reloadIndex(); await wait("index status did not reconcile alongside structural results") { !model.busy }
        precondition(model.index?.generation == 2 && model.indexStateKnown && model.structureGeneration == 3)
        model.detachIndex(); await wait("index detach did not finish") { !model.busy }
        precondition(model.index == nil && model.structureGeneration == 3 && model.structureIsCurrent)
        let page = model.structurePage!
        let reader = try await model.openStructureHit(page.hits[0], from: page, readerTransport: f.readerTransport())
        precondition(reader.match?.start == 207 && reader.match?.end == 223)
        precondition(!f.calls.contains("LIVE-OPEN") && !f.calls.contains(where: { $0.hasPrefix("search:") }))
        model.close(); reader.reader.close()
    }
}
