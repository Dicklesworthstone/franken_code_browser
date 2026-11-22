import Foundation

@main private enum AtlasWorkspaceStructureTests {
    private static let owner: UInt64 = 9_007_199_254_740_997
    private static let root = Array("/private/tmp/project".utf8)
    private static func request(_ needle: String = "needle", mode: AtlasWorkspaceStructureMode = .symbols,
        nameMode: AtlasSearchNameMode = .contains, startAfter: UInt64 = 0) throws -> AtlasWorkspaceStructureRequest {
        try .init(needle: needle, mode: mode, nameMode: nameMode, startAfter: startAfter)
    }
    private static func decode(_ value: [String: Any], request: AtlasWorkspaceStructureRequest,
        root: [UInt8] = AtlasWorkspaceStructureTests.root, command: String = "page", offset: UInt64 = 0,
        canonical: Bool = false) throws -> AtlasWorkspaceStructurePage {
        try .decode(WorkspaceStructureFixture.json(value), command: command, owner: owner, generation: 1,
            root: root, request: request, offset: offset, acceptingCanonicalRoot: canonical)
    }
    private static func refuse(_ value: [String: Any], request: AtlasWorkspaceStructureRequest,
        root: [UInt8] = AtlasWorkspaceStructureTests.root, command: String = "page", canonical: Bool = false) {
        do {
            _ = try decode(value, request: request, root: root, command: command, canonical: canonical)
            preconditionFailure("Malformed structural search authority was accepted")
        } catch {}
    }
    @MainActor private static func waitFor(_ condition: () -> Bool) async {
        let deadline = ProcessInfo.processInfo.systemUptime + 10
        while !condition() && ProcessInfo.processInfo.systemUptime < deadline { await Task.yield() }
        precondition(condition(), "Structural search did not reach its expected boundary")
    }
    @MainActor private static func model(_ fixture: WorkspaceStructureFixture) -> AtlasWorkspaceStructureModel {
        .init(transport: fixture.transport, query: "needle", registryDomain: fixture.domain,
            makeReader: { AtlasPagedReaderModel(transport: fixture.readerTransport) })
    }
    @MainActor private static func source(access: AnyObject? = nil) -> AtlasWorkspaceStreamSource {
        .init(root: "/tmp/project", project: UUID(), browserOwner: 77, access: access)
    }

    @MainActor static func main() async throws {
        let request = try Self.request()
        let valid = WorkspaceStructureFixture.page(owner: owner, request: request)
        let page = try decode(valid, request: request)
        precondition(page.rows.count == 32 && page.nextOffset == 32 && page.summary.retainedHits == 40)
        precondition(page.summary.matchesSeen == 100 && page.summary.complete
            && page.summary.counts["catalogued_files"] == 5001
            && page.summary.counts["retained_catalog_files"] == 256)
        precondition(page.summary.hasMoreMatches && page.summary.counts["structure_metadata_omitted"] == 60,
            "Retention omissions must remain distinct from completed extraction counts")
        let second = try decode(WorkspaceStructureFixture.page(owner: owner, request: request, offset: 32),
            request: request, offset: 32)
        precondition(second.rows.count == 8 && second.rows.last?.occurrence == 80 && second.nextOffset == nil)
        let oldTarget = page.target(page.rows[0])!
        precondition(page.admits(oldTarget) && !second.admits(oldTarget))
        let repeated = try decode(valid, request: request)
        precondition(!repeated.admits(oldTarget), "Identical numeric rows cannot reuse a previous page capability")

        for (key, value) in [
            ("owner", "9007199254740992"), ("query_generation", "2"), ("revision", "00"),
            ("needle", "Needle"), ("requested_mode", "headings"), ("realized_mode", "compiler"),
            ("name_mode", "exact"), ("name_case", "insensitive"),
            ("discovery_policy", "all-regular-files/no-rule-files-v1"),
            ("scope_path", "src"), ("scope_path_hex", "737263"), ("scope_extensions", "rs"),
            ("retained_hits", "257"), ("matches_seen", "39"), ("files_examined", "5002"),
            ("files_unexamined", "1"), ("retained_catalog_pages", "5"), ("retained_catalog_files", "1025"),
            ("page_count", "31"), ("next_offset", "31"), ("payload_bytes_read", "268435457"),
            ("read_calls", "1048577"), ("structure_metadata_bytes", "2097153")
        ] {
            var bad = valid; bad[key] = value; refuse(bad, request: request)
        }
        for key in ["semantic_complete", "compiler_resolved"] {
            var bad = valid; bad[key] = true; refuse(bad, request: request)
        }
        for key in ["count_all_matches", "match_count_complete", "selection_available", "truncated"] {
            var bad = valid; bad[key] = false; refuse(bad, request: request)
        }
        var rounded = valid; rounded["owner"] = NSNumber(value: owner); refuse(rounded, request: request)
        var incomplete = valid; incomplete["incomplete_files"] = "1"; refuse(incomplete, request: request)
        incomplete["search_complete"] = false; incomplete["match_count_complete"] = false; incomplete["state"] = "partial"
        let partialPage = try decode(incomplete, request: request)
        precondition(!partialPage.summary.complete)
        var mixedRows = valid, rows = valid["hits"] as! [[String: Any]]
        rows.swapAt(0, 1); mixedRows["hits"] = rows; refuse(mixedRows, request: request)
        for (key, value) in [
            ("index", "1"), ("occurrence_id", "101"), ("candidate_id", "0"), ("file_id", "0"),
            ("source_revision", "0"), ("source_length", "65537"), ("name", "Needle"),
            ("capture_sha256", String(repeating: "A", count: 64)), ("encoding", "utf32")
        ] {
            var bad = valid; rows = valid["hits"] as! [[String: Any]]; rows[0][key] = value
            bad["hits"] = rows; refuse(bad, request: request)
        }
        var traversal = valid; rows = valid["hits"] as! [[String: Any]]
        rows[0]["path"] = WorkspaceStructureFixture.path(Array("../src/lib.rs".utf8)); traversal["hits"] = rows
        refuse(traversal, request: request)
        var raw = valid; rows[0]["path"] = WorkspaceStructureFixture.path([115, 114, 99, 47, 255, 46, 114, 115])
        raw["hits"] = rows
        let rawPage = try decode(raw, request: request)
        precondition(rawPage.rows[0].path == [115, 114, 99, 47, 255, 46, 114, 115])

        // Only a fresh owned open establishes canonical root bytes. Later
        // replies cannot switch roots, even to a canonically equivalent String.
        let alias = Array("/tmp/project".utf8)
        let opened = WorkspaceStructureFixture.page(owner: owner, revision: 1, request: request, command: "info", round: 0)
        refuse(opened, request: request, root: alias, command: "info")
        let accepted = try decode(opened, request: request, root: alias, command: "info", canonical: true)
        precondition(accepted.summary.root == root)
        refuse(valid, request: request, root: alias)
        refuse(valid, request: request, root: alias, canonical: true)
        for badRoot in [Array("relative".utf8), Array("/a/../b".utf8), Array("/a//b".utf8), [47, 0]] {
            var bad = opened; bad["root_path"] = WorkspaceStructureFixture.path(badRoot)
            refuse(bad, request: request, root: alias, command: "info", canonical: true)
        }
        let nativeRoot: [UInt8] = [47, 255, 47, 112]
        let nativeOpen = WorkspaceStructureFixture.page(owner: owner, revision: 1, root: nativeRoot,
            request: request, command: "info", round: 0)
        let nativePage = try decode(nativeOpen, request: request, root: alias, command: "info", canonical: true)
        precondition(nativePage.summary.root == nativeRoot)
        let advanced = WorkspaceStructureFixture.page(owner: owner, revision: 2, request: request, command: "advance", round: 1)
        let progress = try decode(advanced, request: request, command: "advance")
        precondition(progress.summary.follows(accepted.summary) && page.summary.follows(progress.summary))
        var changedRoot = advanced; changedRoot["root_path"] = WorkspaceStructureFixture.path(alias)
        refuse(changedRoot, request: request, command: "advance")

        for mode in AtlasSearchNameMode.allCases {
            let exactBytes = try self.request("é", nameMode: mode)
            precondition(exactBytes.admitsName("é") && !exactBytes.admitsName("e\u{301}"))
            precondition(!exactBytes.admitsName("É"))
        }
        for needle in ["", "x\0y", String(repeating: "x", count: 1025)] {
            do { _ = try self.request(needle); preconditionFailure("Invalid query was admitted") }
            catch AtlasWorkspaceStructureError.invalidInput {}
        }
        let headings = try self.request(mode: .headings)
        let headingPage = try decode(WorkspaceStructureFixture.page(owner: owner, request: headings), request: headings)
        precondition(headingPage.rows.first?.headingSlug == "fixture-heading")
        #if canImport(CryptoKit)
        let visible = try AtlasSearchPathScope(visiblePaths: [Array("src/lib.rs".utf8)])
        for policy in [AtlasDiscoveryPolicy.repositoryRules, .includeExcluded] {
            let scoped = try AtlasWorkspaceStructureRequest(needle: "needle", mode: .symbols, nameMode: .contains,
                pathScope: visible, discoveryPolicy: policy)
            let admitted = try decode(WorkspaceStructureFixture.page(owner: owner, request: scoped), request: scoped)
            precondition(admitted.summary.missingScopeFiles == 0 && admitted.summary.request.discoveryPolicy == policy)
            var bad = WorkspaceStructureFixture.page(owner: owner, request: scoped)
            bad["scope_region_sha256"] = String(repeating: "f", count: 64); refuse(bad, request: scoped)
        }
        #endif

        let readerOwner = owner + 100
        let receipt = WorkspaceStructureFixture.receipt(owner: owner, reader: readerOwner, request: request)
        let copied = try AtlasWorkspaceStructureReaderReceipt.decode(WorkspaceStructureFixture.json(receipt),
            target: oldTarget, summary: page.summary, root: root, destination: readerOwner)
        precondition(copied.info.identity.owner == readerOwner && copied.info.identity.file == 1
            && copied.initialOffset == oldTarget.hit.range.start && oldTarget.hit.file == 7)
        func refuseReceipt(_ value: [String: Any]) {
            do {
                _ = try AtlasWorkspaceStructureReaderReceipt.decode(WorkspaceStructureFixture.json(value),
                    target: oldTarget, summary: page.summary, root: root, destination: readerOwner)
                preconditionFailure("Foreign retained reader receipt accepted")
            } catch {}
        }
        for (key, value) in [("revision", "4"), ("reader_owner", String(readerOwner + 1)),
            ("activation_source", "live-source"), ("additional_source_bytes_read", "1"), ("additional_read_calls", "1")] {
            var bad = receipt; bad[key] = value; refuseReceipt(bad)
        }
        for (key, value) in [("capture_sha256", String(repeating: "b", count: 64)), ("captured_bytes", "12"),
            ("encoding", "utf16le"), ("initial_source_bytes_read", "1"), ("capture_origin", "live-path")] {
            var bad = receipt, info = receipt["reader"] as! [String: Any]
            info[key] = value; bad["reader"] = info; refuseReceipt(bad)
        }
        var wrongPath = receipt, info = receipt["reader"] as! [String: Any]
        info["path"] = WorkspaceStructureFixture.path(Array("src/other.rs".utf8)); wrongPath["reader"] = info; refuseReceipt(wrongPath)
        var wrongHit = receipt, hit = receipt["hit"] as! [String: Any]
        hit["source_revision"] = "10"; wrongHit["hit"] = hit; refuseReceipt(wrongHit)

        // A heading's selected original span may extend beyond the first 16 KiB
        // window; the independent reader still owns the complete source.
        let long = try self.request(String(repeating: "x", count: 1024), mode: .headings)
        var longPage = WorkspaceStructureFixture.page(owner: owner, request: long)
        var longRows = longPage["hits"] as! [[String: Any]]
        longRows[0]["original_range"] = ["start": "2", "end": "50000"]
        longRows[0]["evidence_range"] = ["start": "0", "end": "60000"]; longPage["hits"] = longRows
        let selectedLong = try decode(longPage, request: long), longTarget = selectedLong.target(selectedLong.rows[0])!
        var longReceipt = WorkspaceStructureFixture.receipt(owner: owner, reader: readerOwner, request: long)
        longReceipt["hit"] = longRows[0]
        _ = try AtlasWorkspaceStructureReaderReceipt.decode(WorkspaceStructureFixture.json(longReceipt),
            target: longTarget, summary: selectedLong.summary, root: root, destination: readerOwner)

        let fixture = WorkspaceStructureFixture(), model = Self.model(fixture)
        model.open(source())
        await waitFor { !model.busy }
        guard let first = model.page else { preconditionFailure(model.notice) }
        precondition(first.summary.root == root && first.summary.complete && fixture.log.filter { $0 == "advance" }.count == 2)
        let target = first.target(first.rows[0])!
        model.nextPage(); await waitFor { !model.busy }
        let opens = fixture.log.filter { $0 == "open-reader" }.count
        model.open(target)
        precondition(!model.busy && fixture.log.filter { $0 == "open-reader" }.count == opens)
        model.previousPage(); await waitFor { !model.busy }
        let current = model.page!, selected = current.target(current.rows[0])!
        model.open(selected); await waitFor { !model.busy }
        guard let retained = model.currentReader else { preconditionFailure(model.notice) }
        precondition(!retained.reader.canRefresh && retained.reader.page?.start == selected.hit.range.start)
        let retainedIdentity = retained.reader.page!.identity

        model.queryText = "other"
        precondition(!model.authorized && !model.canSelect && model.currentReader === retained)
        model.search(); await waitFor { !model.busy }
        fixture.patch { phase, value in
            guard phase == "open-reader" else { return value }
            var changed = value, info = value["reader"] as! [String: Any]
            info["capture_sha256"] = String(repeating: "b", count: 64); changed["reader"] = info
            return changed
        }
        let changed = model.page!, changedTarget = changed.target(changed.rows[0])!
        model.open(changedTarget); await waitFor { !model.busy }
        precondition(model.currentReader === retained && retained.reader.page?.identity == retainedIdentity,
            "A refused replacement cannot discard previously opened source")
        fixture.patch { _, value in value }
        model.laterWindow(); await waitFor { !model.busy }
        precondition(model.page?.summary.request.startAfter == 80 && fixture.log.contains("start-after:80"),
            "Next window must use the actual last retained occurrence, including gaps")
        precondition(model.page?.rows.count == 20 && model.canEarlierWindow && !model.canLaterWindow)
        model.earlierWindow(); await waitFor { !model.busy }
        precondition(model.page?.summary.request.startAfter == 0 && !model.canEarlierWindow)

        // Omitted labels before the final retained match cannot cause an empty
        // and potentially expensive extra observation.
        fixture.patch { phase, value in
            guard phase == "page", value["page_offset"] as? String == "39" else { return value }
            var changed = value
            let request = try! AtlasWorkspaceStructureRequest(needle: "other", mode: .symbols, nameMode: .contains)
            changed["hits"] = [WorkspaceStructureFixture.hit(request, index: 39, occurrence: 100)]
            return changed
        }
        let creates = fixture.log.filter { $0 == "create" }.count
        model.laterWindow(); await waitFor { !model.busy }
        precondition(fixture.log.filter { $0 == "create" }.count == creates && model.notice.contains("No later observed matches"))
        fixture.patch { _, value in value }

        fixture.pause("open-reader")
        let closingPage = model.page!, closingTarget = closingPage.target(closingPage.rows[1])!
        model.open(closingTarget); await waitFor { fixture.isBlocked }
        model.closeReader(); fixture.gate.signal()
        await waitFor { !model.busy }
        precondition(model.currentReader == nil && model.canSelect,
            "Closing a reader during replacement must finish the same operation without wedging busy state")
        model.close(); await waitFor { fixture.log.filter { $0 == "close" }.count == fixture.log.filter { $0 == "create" }.count }

        let pausedFixture = WorkspaceStructureFixture(), paused = self.model(pausedFixture)
        pausedFixture.pause("advance"); paused.open(source())
        await waitFor { pausedFixture.isBlocked }
        paused.pause(); pausedFixture.gate.signal()
        await waitFor { !paused.busy }
        precondition(paused.canContinue && paused.page?.summary.revision == 2 && pausedFixture.log.filter { $0 == "advance" }.count == 1)
        let pausedOwner = paused.page!.summary.owner, pausedGeneration = paused.page!.summary.generation
        paused.resume(); await waitFor { !paused.busy }
        precondition(paused.page?.summary.complete == true && paused.page?.summary.owner == pausedOwner
            && paused.page?.summary.generation == pausedGeneration)
        paused.close(); await waitFor { pausedFixture.log.contains("close") }

        let stoppedFixture = WorkspaceStructureFixture(), stopped = self.model(stoppedFixture)
        stoppedFixture.pause("advance"); stopped.open(source()); await waitFor { stoppedFixture.isBlocked }
        stopped.pause(); stoppedFixture.gate.signal(); await waitFor { !stopped.busy }
        stopped.stopAndKeep(); await waitFor { !stopped.busy }
        precondition(stopped.page?.summary.stopReason == "user-stopped" && stopped.canSelect
            && !stopped.canContinue && stopped.page?.summary.complete == false)
        stopped.close(); await waitFor { stoppedFixture.log.contains("close") }

        let lateFixture = WorkspaceStructureFixture(), late = self.model(lateFixture)
        var grant: NSObject? = NSObject(); weak var weakGrant = grant
        lateFixture.pause("open"); late.open(source(access: grant)); grant = nil
        await waitFor { lateFixture.isBlocked }
        late.close()
        precondition(late.closed && late.page == nil && weakGrant != nil,
            "Entered work must retain the original grant through native return")
        lateFixture.gate.signal()
        await waitFor { lateFixture.log.contains("close") && weakGrant == nil }
        precondition(late.page == nil && late.currentReader == nil && !late.busy,
            "A root/sheet close must reject late receipts")

        print("AtlasWorkspaceStructureTests: schema, canonical roots, retained activation and lifecycle contracts passed")
    }
}

