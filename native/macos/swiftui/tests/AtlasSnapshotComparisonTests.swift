import Foundation

@main private enum AtlasSnapshotComparisonTests {
    private static let owner: UInt64 = 9_007_199_254_740_997
    @MainActor private static func waitFor(_ condition: () -> Bool) async {
        let deadline = ProcessInfo.processInfo.systemUptime + 10
        while !condition() && ProcessInfo.processInfo.systemUptime < deadline { await Task.yield() }
        precondition(condition(), "Saved comparison work did not reach its expected boundary")
    }
    private static func info() throws -> AtlasSnapshotComparisonInfo {
        try AtlasSnapshotComparisonInfo.decode(SnapshotComparisonFixture.json(SnapshotComparisonFixture.info(owner: owner)),
            owner: owner, before: Array("/before.fcbs".utf8), after: Array("/after.fcbs".utf8), fresh: true)
    }
    private static func decode(_ value: [String: Any], identity: AtlasSnapshotComparisonIdentity,
        generation: UInt64 = 1, filter: AtlasSnapshotComparisonFilter = .differences,
        start: UInt64 = 0, limit: UInt64 = 64) throws -> AtlasSnapshotComparisonPage {
        try AtlasSnapshotComparisonPage.decode(SnapshotComparisonFixture.json(value), identity: identity,
            generation: generation, command: "page", filter: filter, start: start, limit: limit)
    }
    private static func refuse(_ value: [String: Any], identity: AtlasSnapshotComparisonIdentity,
        filter: AtlasSnapshotComparisonFilter = .differences) {
        do { _ = try decode(value, identity: identity, filter: filter); preconditionFailure("Malformed comparison authority accepted") }
        catch {}
    }

    @MainActor static func main() async throws {
        let identity = try info().identity
        let valid = SnapshotComparisonFixture.page(owner: owner)
        let page = try decode(valid, identity: identity)
        precondition(page.rows.count == 64 && page.rows.first?.id == 2 && page.next == 64 && page.total == 301)
        precondition(page.progress.compared == 302 && page.progress.changed == 298
            && page.progress.added == 1 && page.progress.removed == 1 && page.progress.uncertain == 1)
        precondition(!page.complete && page.progress.scanComplete && page.identity.membershipComparable,
            "Completed scanning cannot erase an unavailable member")
        let hugeGeneration = UInt64.max - 2
        let huge = try decode(SnapshotComparisonFixture.page(owner: owner, generation: hugeGeneration),
            identity: identity, generation: hugeGeneration)
        precondition(huge.generation == hugeGeneration && huge.identity.owner == owner)

        var shortened = valid
        shortened["entries"] = Array((valid["entries"] as! [[String: Any]]).prefix(3))
        shortened["next_offset"] = "3"; shortened["page_limited"] = true
        let short = try decode(shortened, identity: identity)
        precondition(short.rows.count == 3 && short.next == 3)
        var empty = shortened; empty["entries"] = []; empty["next_offset"] = "0"; refuse(empty, identity: identity)
        var falseLimited = shortened; falseLimited["page_limited"] = false; refuse(falseLimited, identity: identity)
        let end = try decode(SnapshotComparisonFixture.page(owner: owner, start: 301), identity: identity, start: 301)
        precondition(end.rows.isEmpty && end.next == nil && !end.pageLimited)

        for (key, value) in [
            ("owner", "9007199254740992"), ("comparison_generation", "01"), ("max_path_bytes", "4096"),
            ("max_source_file_bytes", "4194304"), ("max_response_bytes", "4194305"),
            ("compared_paths", "301"), ("retained_entries", "301"), ("known_differences", "298"),
            ("uncertain_paths", "0"), ("matching_entries", "300"), ("next_offset", "63"),
            ("paths_upper_bound", "603"), ("max_step_entries", "0"), ("last_step_entries", "1025"),
            ("live_source_bytes_read", "1")
        ] {
            var bad = valid; bad[key] = value; refuse(bad, identity: identity)
        }
        for key in ["comparison_complete", "history_inferred", "renames_inferred", "native_presented", "live_source_reopened"] {
            var bad = valid; bad[key] = true; refuse(bad, identity: identity)
        }
        var rounded = valid; rounded["owner"] = NSNumber(value: owner); refuse(rounded, identity: identity)
        var differentArchive = valid, archive = valid["after_archive"] as! [String: Any]
        archive["digest"] = String(repeating: "f", count: 64); differentArchive["after_archive"] = archive
        refuse(differentArchive, identity: identity)
        var wrongPolicy = valid, beforeArchive = valid["before_archive"] as! [String: Any]
        beforeArchive["policy"] = "different-v1"; wrongPolicy["before_archive"] = beforeArchive
        refuse(wrongPolicy, identity: identity)

        for (key, value) in [("member", "301"), ("file_id", "2"), ("source_revision", "2"),
                             ("captured_bytes", "1048577"), ("source_digest", String(repeating: "Z", count: 64))] {
            var bad = valid, rows = valid["entries"] as! [[String: Any]], member = rows[0]["after"] as! [String: Any]
            member[key] = value; rows[0]["after"] = member; bad["entries"] = rows
            refuse(bad, identity: identity)
        }
        var mismatchedLength = valid, rows = valid["entries"] as! [[String: Any]]
        var member = rows[0]["before"] as! [String: Any]
        member["observed_bytes"] = "3"; rows[0]["before"] = member; mismatchedLength["entries"] = rows
        refuse(mismatchedLength, identity: identity)
        var unsorted = valid; rows = valid["entries"] as! [[String: Any]]
        rows.swapAt(0, 1); unsorted["entries"] = rows; refuse(unsorted, identity: identity)
        var repeated = valid; rows = valid["entries"] as! [[String: Any]]
        rows[1] = rows[0]; repeated["entries"] = rows; refuse(repeated, identity: identity)
        var wrongKind = valid; rows = valid["entries"] as! [[String: Any]]
        rows[0]["kind"] = "unchanged"; wrongKind["entries"] = rows; refuse(wrongKind, identity: identity)
        let unavailable = try decode(SnapshotComparisonFixture.page(owner: owner, filter: .unavailable),
            identity: identity, filter: .unavailable)
        precondition(unavailable.rows.count == 1 && unavailable.rows[0].before?.observedBytes == UInt64.max
            && !unavailable.rows[0].canOpen(.before) && unavailable.rows[0].canOpen(.after))

        // Incomplete opposite membership downgrades an addition to only-after.
        // It must never be displayed as a proven removal/addition.
        var partialInfo = SnapshotComparisonFixture.info(owner: owner)
        beforeArchive = partialInfo["before_archive"] as! [String: Any]
        beforeArchive["discovery_complete"] = false; partialInfo["before_archive"] = beforeArchive
        let partialIdentity = try AtlasSnapshotComparisonInfo.decode(SnapshotComparisonFixture.json(partialInfo),
            owner: owner, before: Array("/before.fcbs".utf8), after: Array("/after.fcbs".utf8), fresh: true).identity
        var partial = SnapshotComparisonFixture.page(owner: owner, filter: .added)
        var partialBefore = partial["before_archive"] as! [String: Any]
        partialBefore["discovery_complete"] = false; partial["before_archive"] = partialBefore
        partial["membership_comparable"] = false; partial["added"] = "0"; partial["only_after"] = "1"
        partial["known_differences"] = "299"; partial["uncertain_paths"] = "2"; partial["filter"] = "only-after"
        rows = partial["entries"] as! [[String: Any]]; rows[0]["kind"] = "only-after"; partial["entries"] = rows
        let uncertain = try decode(partial, identity: partialIdentity, filter: .onlyAfter)
        precondition(uncertain.rows[0].kind == .onlyAfter && !uncertain.complete)
        rows[0]["kind"] = "added"; partial["entries"] = rows
        refuse(partial, identity: partialIdentity, filter: .onlyAfter)

        // Raw native bytes, including invalid UTF-8 and decomposed Unicode,
        // remain the authority; labels and Swift canonical equality do not.
        var raw = shortened; rows = Array((raw["entries"] as! [[String: Any]]).prefix(1))
        rows[0]["path"] = ["encoding": "unix-bytes", "hex": "ff2e7273", "display": "\\xff.rs"]
        raw["entries"] = rows; raw["next_offset"] = "1"
        let rawPage = try decode(raw, identity: identity)
        precondition(rawPage.rows[0].path.bytes == [255, 46, 114, 115])
        var traversal = raw; rows[0]["path"] = SnapshotComparisonFixture.path("../source.rs"); traversal["entries"] = rows
        refuse(traversal, identity: identity)

        let fixture = SnapshotComparisonFixture(), model = AtlasSnapshotComparisonModel(transport: fixture.transport)
        model.open(before: "/before.fcbs", after: "/after.fcbs", beforeAccess: nil, afterAccess: nil)
        await waitFor { !model.busy }
        guard let first = model.page else { preconditionFailure(model.notice) }
        precondition(fixture.log.filter { $0 == "step" }.count == 2 && first.progress.compared == 302)
        precondition(fixture.log.filter { $0 == "open-reader" }.isEmpty, "Metadata comparison cannot read saved source payloads")
        model.setFilter(.changed); await waitFor { !model.busy }
        let selectedPage = model.page!, selected = selectedPage.rows[0]
        let before = try await model.open(selected, from: selectedPage, side: .before, readerTransport: fixture.readerTransport)
        let after = try await model.open(selected, from: selectedPage, side: .after, readerTransport: fixture.readerTransport)
        precondition(before.reader.page?.text == "old\n" && after.reader.page?.text == "new\n")
        precondition(!before.reader.canRefresh && !after.reader.canRefresh
            && before.reader.page?.identity.owner != after.reader.page?.identity.owner)
        precondition(model.info?.io.beforeBytes == 4 && model.info?.io.afterBytes == 4)
        let opens = fixture.log.filter { $0 == "open-reader" }.count
        model.loadPage(start: selectedPage.next!); await waitFor { !model.busy }
        do {
            _ = try await model.open(selected, from: selectedPage, side: .before, readerTransport: fixture.readerTransport)
            preconditionFailure("A previous page UUID retained row authority")
        } catch AtlasSnapshotComparisonError.invalidInput {}
        precondition(fixture.log.filter { $0 == "open-reader" }.count == opens)

        // A bad transfer receipt advances archive I/O but cannot publish a
        // reader; the next explicit activation observes this same owner again.
        let current = model.page!, target = current.rows[0]
        fixture.patch { phase, value in
            var value = value
            if phase == "open-reader" { value["archive_digest"] = String(repeating: "f", count: 64) }
            return value
        }
        do {
            _ = try await model.open(target, from: current, side: .before, readerTransport: fixture.readerTransport)
            preconditionFailure("Wrong archive receipt was accepted")
        } catch AtlasSnapshotComparisonError.invalidResponse {}
        fixture.patch { _, value in value }
        let recovered = try await model.open(target, from: current, side: .before, readerTransport: fixture.readerTransport)
        precondition(recovered.reader.page?.text == "old\n" && model.info?.io.beforeBytes == 12)

        let oldPageID = model.page!.id, oldOwner = model.page!.identity.owner
        fixture.pause("step")
        model.open(before: "/replacement.fcbs", after: "/next.fcbs", beforeAccess: nil, afterAccess: nil)
        await waitFor { fixture.isBlocked }
        precondition(model.page?.id == oldPageID && model.busy)
        model.cancel(); fixture.gate.signal(); await waitFor { !model.busy }
        precondition(model.page?.id == oldPageID && model.page?.identity.owner == oldOwner)
        fixture.patch { phase, value in
            var value = value
            if phase == "step" { value["compared_paths"] = "999999" }
            return value
        }
        model.open(before: "/replacement.fcbs", after: "/next.fcbs", beforeAccess: nil, afterAccess: nil)
        await waitFor { !model.busy }
        precondition(model.page?.id == oldPageID, "Failed replacement must preserve the accepted archive pair")
        fixture.patch { _, value in value }
        model.open(before: "/replacement.fcbs", after: "/next.fcbs", beforeAccess: nil, afterAccess: nil)
        await waitFor { !model.busy }
        precondition(model.page?.identity.owner != oldOwner
            && model.page?.identity.before.path.bytes == Array("/replacement.fcbs".utf8))
        precondition(before.reader.page?.text == "old\n" && after.reader.page?.text == "new\n",
            "Independent reader captures survive replacement of both archive descriptors")
        before.reader.close(); after.reader.close(); recovered.reader.close(); model.close()

        final class Grant {}
        var beforeGrant: Grant? = Grant(), afterGrant: Grant? = Grant()
        weak var oldBeforeGrant = beforeGrant, oldAfterGrant = afterGrant
        let closingFixture = SnapshotComparisonFixture(), closing = AtlasSnapshotComparisonModel(transport: closingFixture.transport)
        closingFixture.pause("open")
        closing.open(before: "/before.fcbs", after: "/after.fcbs", beforeAccess: beforeGrant, afterAccess: afterGrant)
        beforeGrant = nil; afterGrant = nil
        await waitFor { closingFixture.isBlocked }
        closing.close()
        precondition(oldBeforeGrant != nil && oldAfterGrant != nil && closing.busy,
            "Cancel/close cannot release either archive grant during an entered call")
        closingFixture.gate.signal(); await waitFor { !closing.busy }
        await waitFor { oldBeforeGrant == nil && oldAfterGrant == nil }
        precondition(closing.page == nil && closing.closed)
        print("AtlasSnapshotComparisonTests: strict scope classifications, canonical provenance, paging, independent readers and cancellation passed")
    }
}
