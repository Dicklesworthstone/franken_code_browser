import Foundation

@main private enum AtlasProjectLinksTests {
    @MainActor private static func settled(_ model: AtlasProjectLinksModel) async {
        let deadline = ProcessInfo.processInfo.systemUptime + 10
        while model.busy && ProcessInfo.processInfo.systemUptime < deadline { await Task.yield() }
        precondition(!model.busy, "Native documentation work did not drain")
    }
    @MainActor private static func settled(_ model: AtlasPagedReaderModel) async {
        let deadline = ProcessInfo.processInfo.systemUptime + 10
        while model.busy && ProcessInfo.processInfo.systemUptime < deadline { await Task.yield() }
        precondition(!model.busy)
    }
    private static func prepared(_ fixture: ProjectLinksFixture) async throws -> AtlasPreparedIndex {
        let worker = AtlasIndexedSearch(transport: fixture.index.transport)
        let flag = AtlasSearchCancellation(), lease = AtlasSearchAccessLease(nil)
        return try await Task.detached {
            let first = try worker.prepare(root: "/project", cancellation: flag, access: lease, progress: { _ in })
            let second = try worker.prepare(root: "/project", cancellation: flag, access: lease, progress: { _ in })
            precondition(first.owner === second.owner && first.basis == second.basis)
            return second
        }.value
    }
    private static func decode(_ value: [String: Any], index: AtlasPreparedIndex,
        direction: AtlasProjectLinkDirection = .all, path: [UInt8]? = nil) throws -> AtlasProjectLinksPage {
        try AtlasProjectLinksPage.decode(ProjectLinksFixture.json(value), index: index, generation: 1,
            command: "links-page", direction: direction, path: path)
    }
    private static func refuse(_ value: [String: Any], index: AtlasPreparedIndex,
        direction: AtlasProjectLinkDirection = .all, path: [UInt8]? = nil) {
        do { _ = try decode(value, index: index, direction: direction, path: path); preconditionFailure("Malformed link authority admitted") }
        catch {}
    }
    @MainActor static func main() async throws {
        let fixture = ProjectLinksFixture(), index = try await prepared(fixture)
        precondition(fixture.index.count("create:") == 1 && fixture.index.count("query-begin:") == 0,
            "Prepare-only leases must use the existing builder without a dummy query")
        let valid = ProjectLinksFixture.page(owner: index.basis.owner, generation: 1, command: "links-page")
        let page = try decode(valid, index: index)
        precondition(page.links.count == 64 && page.total == 70 && page.next == 64 && page.complete)
        precondition(page.basis.owner > 9_007_199_254_740_992 && page.links[0].source.id == 1
            && page.links[0].target?.id == 2 && page.links[0].label.contains("unqualified"))
        var short = valid
        short["links"] = Array((valid["links"] as! [[String: Any]]).prefix(7)); short["next_offset"] = "7"
        let shortened = try decode(short, index: index)
        precondition(shortened.links.count == 7 && shortened.next == 7,
            "A response-byte limit may shorten a page without losing next-page authority")
        var empty = short; empty["links"] = []; empty["next_offset"] = "0"; refuse(empty, index: index)
        for (key, value) in [
            ("relationship_generation", "01"), ("source_manifest", "2"), ("index_generation", "2"),
            ("capture_manifest", "3"), ("source_bytes_read", "1"), ("read_calls", "1"),
            ("retained_links", "71"), ("parsed_source_bytes", "4197"), ("hashed_source_bytes", "4197"),
            ("capture_unavailable_markdown_files", "1"), ("max_links", "4095"), ("next_offset", "63"), ("matching_links", "69")
        ] {
            var bad = valid; bad[key] = value; refuse(bad, index: index)
        }
        for (key, value) in [
            ("link_id", "0"), ("source_file_id", "3"), ("source_revision", "2"), ("source_link_id", "0"),
            ("target_file_id", "3"), ("target_revision", "2"), ("fragment_resolution", "resolved-heading"),
            ("destination", String(repeating: "x", count: 4097))
        ] {
            var bad = valid, rows = valid["links"] as! [[String: Any]]
            rows[0][key] = value; bad["links"] = rows; refuse(bad, index: index)
        }
        var exactSpan = valid, spans = valid["links"] as! [[String: Any]]
        spans[0]["evidence_range"] = ["start": "1", "end": "8"]
        exactSpan["links"] = spans; refuse(exactSpan, index: index)
        var partial = valid; partial["parser_diagnostics"] = "2"; partial["relationships_complete"] = false
        let partialPage = try decode(partial, index: index)
        precondition(!partialPage.complete)
        partial["relationships_complete"] = true; refuse(partial, index: index)

        let selected = Array("src.rs".utf8)
        let incoming = ProjectLinksFixture.page(owner: index.basis.owner, generation: 1,
            command: "links-page", direction: .incoming, path: "src.rs")
        let incomingPage = try decode(incoming, index: index, direction: .incoming, path: selected)
        precondition(incomingPage.total == 70)
        refuse(incoming, index: index, direction: .incoming, path: Array("docs.md".utf8))
        var wrongFilter = incoming; wrongFilter["filter_file_id"] = "3"
        refuse(wrongFilter, index: index, direction: .incoming, path: selected)
        // Raw non-UTF-8 filenames remain exact selectable identity, independent
        // of their escaped display and of any previous native catalog's IDs.
        let raw: [UInt8] = [255, 46, 114, 115]
        let rawPath = ["encoding": "unix-bytes", "hex": ProjectLinksFixture.hex(raw), "display": "\\xff.rs"]
        var rawPage = incoming, rawRows = incoming["links"] as! [[String: Any]]
        for i in rawRows.indices { rawRows[i]["target_path"] = rawPath }
        rawPage["filter_path"] = rawPath; rawPage["links"] = rawRows
        let rawAccepted = try decode(rawPage, index: index, direction: .incoming, path: raw)
        precondition(rawAccepted.path?.bytes == raw)
        for resolution in ["unavailable", "uncaptured", "outside-capture-scope", "missing", "external", "unresolved"] {
            var value = valid, rows = valid["links"] as! [[String: Any]]
            for i in rows.indices {
                rows[i]["resolution"] = resolution
                for key in ["target_revision", "target_sha256", "target_bytes"] { rows[i][key] = NSNull() }
                if ["missing", "external", "unresolved"].contains(resolution) {
                    rows[i]["target_file_id"] = NSNull(); rows[i]["target_path"] = NSNull()
                }
            }
            value["links"] = rows
            let result = try decode(value, index: index).links[0]
            precondition(!result.canOpenTarget && result.label != result.destination,
                "Uncaptured and external destinations remain distinct visible states without target authority")
        }

        let link = page.links[0], receiver: UInt64 = 77
        let receipt = ProjectLinksFixture.receipt(owner: index.basis.owner, reader: receiver, generation: 1, link: 1, side: .target)
        let info = try AtlasProjectLinkReaderReceipt.decode(ProjectLinksFixture.json(receipt), index: index,
            generation: 1, link: link, side: .target, destination: receiver)
        precondition(info.identity.pathBytes == selected && info.identity.capturedBytes == 100)
        for key in ["source_reopened", "additional_source_bytes_read", "source_sha256", "reader_owner", "destination_suffix"] {
            var bad = receipt
            switch key {
            case "source_reopened": bad[key] = true
            case "additional_source_bytes_read": bad[key] = "1"
            case "source_sha256": bad[key] = String(repeating: "c", count: 64)
            case "reader_owner": bad[key] = "78"
            default: bad[key] = "#invented-heading"
            }
            do {
                _ = try AtlasProjectLinkReaderReceipt.decode(ProjectLinksFixture.json(bad), index: index,
                    generation: 1, link: link, side: .target, destination: receiver)
                preconditionFailure("Changed captured-reader evidence admitted")
            } catch {}
        }

        let live = ProjectLinksFixture()
        let model = AtlasProjectLinksModel(root: "/project", initialPath: selected, direction: .incoming, transport: live.transport)
        model.prepare(); await settled(model)
        guard let accepted = model.page else { preconditionFailure(model.notice) }
        precondition(accepted.direction == .incoming && accepted.path?.bytes == selected && accepted.next == 64)
        precondition(live.index.count("query-begin:") == 0 && model.hasPreparedSources)
        let source = try await model.open(accepted.links[0], from: accepted, side: .evidence, readerTransport: live.readerTransport)
        let target = try await model.open(accepted.links[0], from: accepted, side: .target, readerTransport: live.readerTransport)
        precondition(source.reader.page?.text == ProjectLinksFixture.source(1)
            && target.reader.page?.text == ProjectLinksFixture.source(2))
        precondition(source.evidenceLabel.contains("whole captured document") && target.evidenceLabel.contains("unqualified"))
        let creates = live.log.filter { $0 == "create-reader" }.count
        model.loadPage(start: 64); await settled(model)
        precondition(model.page?.links.count == 6 && model.page?.links.first?.id == 65)
        do {
            _ = try await model.open(accepted.links[0], from: accepted, side: .target, readerTransport: live.readerTransport)
            preconditionFailure("A replaced native page UUID authorized an old row")
        } catch {}
        precondition(live.log.filter { $0 == "create-reader" }.count == creates)
        let currentID = model.page!.id
        model.selectPath(Array("missing.rs".utf8), direction: .incoming); await settled(model)
        precondition(model.page?.id == currentID && model.selectedPath == selected
            && model.notice.contains("outside this captured atlas catalog"), "Missing catalog membership is not an empty backlink set")
        model.selectPath(Array("docs.md".utf8), direction: .outgoing); await settled(model)
        precondition(model.page?.direction == .outgoing && model.page?.path?.bytes == Array("docs.md".utf8))
        model.prepare(); await settled(model)
        precondition(model.page?.generation == 2 && live.index.count("create:") == 1, "Rebuild reuses captured sources")
        model.prepare(refresh: true); await settled(model)
        precondition(live.index.count("create:") == 2 && model.page?.generation == 1, "Refresh owns a fresh index and graph namespace")
        model.close()
        source.reader.offsetInput = "0"; source.reader.goToByte(); await settled(source.reader)
        target.reader.offsetInput = "0"; target.reader.goToByte(); await settled(target.reader)
        precondition(source.reader.page?.text == ProjectLinksFixture.source(1) && target.reader.page?.text == ProjectLinksFixture.source(2),
            "Independent source and target readers survive graph refresh and close")
        source.reader.close(); target.reader.close()

        let canceled = ProjectLinksFixture(), cancelModel = AtlasProjectLinksModel(root: "/project", transport: canceled.transport)
        canceled.pause("step"); cancelModel.prepare()
        let deadline = ProcessInfo.processInfo.systemUptime + 10
        while !canceled.isBlocked && ProcessInfo.processInfo.systemUptime < deadline { await Task.yield() }
        precondition(canceled.isBlocked)
        cancelModel.cancel(); canceled.gate.signal(); await settled(cancelModel)
        precondition(cancelModel.page == nil && cancelModel.hasPreparedSources, "Canceled graph rows cannot become accepted authority")
        cancelModel.prepare(); await settled(cancelModel)
        precondition(cancelModel.page?.generation == 2 && canceled.index.count("create:") == 1,
            "Canceled attempts consume their generation while preserving a completed source index")
        cancelModel.close()

        let corrupted = ProjectLinksFixture(), badModel = AtlasProjectLinksModel(root: "/project", transport: corrupted.transport)
        corrupted.patch { phase, value in
            var value = value
            if phase == "step" { value["relationship_generation"] = "999" }
            return value
        }
        badModel.prepare(); await settled(badModel)
        precondition(badModel.page == nil && badModel.hasPreparedSources)
        corrupted.patch { _, value in value }; badModel.prepare(); await settled(badModel)
        precondition(badModel.page?.generation == 2); badModel.close()

        let retiring = ProjectLinksFixture(), retiringModel = AtlasProjectLinksModel(root: "/project", transport: retiring.transport)
        retiring.pause("begin"); retiringModel.prepare()
        let retireDeadline = ProcessInfo.processInfo.systemUptime + 10
        while !retiring.isBlocked && ProcessInfo.processInfo.systemUptime < retireDeadline { await Task.yield() }
        precondition(retiring.isBlocked)
        retiringModel.close(); retiring.gate.signal(); await settled(retiringModel)
        precondition(retiringModel.closed && retiringModel.page == nil && !retiringModel.hasPreparedSources,
            "Project/grant replacement revokes the surface before late foreign work can publish")
        print("AtlasProjectLinksTests: prepare-only, bounded pages, provenance, captured readers and cancellation passed")
    }
}
