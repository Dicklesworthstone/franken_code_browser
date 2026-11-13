import Foundation

@main private enum AtlasProjectImportsTests {
    @MainActor private static func settled(_ model: AtlasProjectLinksModel) async {
        let deadline = ProcessInfo.processInfo.systemUptime + 10
        while model.busy && ProcessInfo.processInfo.systemUptime < deadline { await Task.yield() }
        precondition(!model.busy, "Relationship work did not drain")
    }
    private static func prepared(_ fixture: ProjectLinksFixture) async throws -> AtlasPreparedIndex {
        let worker = AtlasIndexedSearch(transport: fixture.index.transport)
        return try await Task.detached {
            try worker.prepare(root: "/project", cancellation: AtlasSearchCancellation(),
                access: AtlasSearchAccessLease(nil), progress: { _ in })
        }.value
    }
    private static func decode(_ value: [String: Any], index: AtlasPreparedIndex,
        direction: AtlasProjectLinkDirection = .all, path: [UInt8]? = nil) throws -> AtlasProjectLinksPage {
        try AtlasProjectLinksPage.decode(ProjectLinksFixture.json(value), index: index, generation: 1,
            command: "imports-page", direction: direction, path: path, projection: .imports)
    }
    private static func refuse(_ value: [String: Any], index: AtlasPreparedIndex) {
        do { _ = try decode(value, index: index); preconditionFailure("Malformed import authority admitted") }
        catch {}
    }
    private static func wire(_ index: AtlasPreparedIndex) throws {
        let valid = ProjectLinksFixture.importPage(owner: index.basis.owner, generation: 1, command: "imports-page")
        let page = try decode(valid, index: index)
        precondition(page.projection == .imports && page.complete && page.total == 70 && page.next == 64)
        let link = page.links[0]
        precondition(link.source.id == 3 && link.target?.id == 4 && link.block == nil
            && link.evidenceRange.start == 11 && link.evidenceRange.end == 21
            && link.evidenceLabel.contains("not compiler resolved") && link.canOpenTarget)
        precondition(page.summary.contains("does not establish compiler"), "Lexical completeness must stay qualified")
        let incoming = ProjectLinksFixture.importPage(owner: index.basis.owner, generation: 1,
            command: "imports-page", direction: .incoming, path: "src/child.rs")
        let incomingPage = try decode(incoming, index: index, direction: .incoming, path: Array("src/child.rs".utf8))
        precondition(incomingPage.total == 70 && incomingPage.filterFile == 4)

        for (key, value) in [
            ("schema", "fcb.atlas-links/1"), ("command", "links-page"), ("relationship_generation", "2"),
            ("projection", "compiler-imports"), ("relationship_kind", "parsed-markdown-link"),
            ("evidence_precision", "whole-captured-markdown-document"), ("extractor", "unknown"),
            ("resolver", "compiler"), ("extraction_policy", "all-languages"),
            ("source_bytes_read", "1"), ("read_calls", "1"), ("max_source_bytes", "262144"),
            ("pending_source_files", "1"), ("retained_links", "71")
        ] {
            var bad = valid; bad[key] = value; refuse(bad, index: index)
        }
        for key in ["compiler_resolved", "semantic_complete"] {
            var bad = valid; bad[key] = true; refuse(bad, index: index)
            bad = valid; bad.removeValue(forKey: key); refuse(bad, index: index)
        }
        for (key, value) in [
            ("language", "python"), ("lexical_import_kind", "javascript-import"), ("resolution_basis", "external-specifier"),
            ("source_bytes", "131073"), ("source_file_id", "5"), ("source_link_id", "0"), ("destination", "")
        ] {
            var bad = valid, rows = valid["links"] as! [[String: Any]]
            rows[0][key] = value; bad["links"] = rows; refuse(bad, index: index)
        }
        for range in [["start": "21", "end": "11"], ["start": "11", "end": "11"], ["start": "11", "end": "4097"]] {
            var bad = valid, rows = valid["links"] as! [[String: Any]]
            rows[0]["evidence_range"] = range; bad["links"] = rows; refuse(bad, index: index)
        }
        var markdown = valid, rows = valid["links"] as! [[String: Any]]
        rows[0]["block_index"] = "0"; markdown["links"] = rows; refuse(markdown, index: index)

        for (resolution, basis) in [
            ("unresolved", "unresolved-rust-use"), ("external", "external-specifier"),
            ("missing", "no-catalog-target"), ("ambiguous", "ambiguous-catalog-target")
        ] {
            var value = valid, rows = valid["links"] as! [[String: Any]]
            for i in rows.indices {
                rows[i]["resolution"] = resolution; rows[i]["resolution_basis"] = basis
                for key in ["target_file_id", "target_path", "target_revision", "target_sha256", "target_bytes"] { rows[i][key] = NSNull() }
            }
            value["links"] = rows
            let admitted = try decode(value, index: index).links[0]
            precondition(!admitted.canOpenTarget && !admitted.evidenceLabel.isEmpty,
                "Unresolved imports retain exact readable evidence without manufacturing a target")
        }

        for side in [AtlasProjectLinkSide.evidence, .target] {
            let value = ProjectLinksFixture.importReceipt(owner: index.basis.owner, reader: 77,
                generation: 1, link: 1, side: side)
            func admit(_ value: [String: Any]) throws -> AtlasProjectLinkReaderReceipt {
                try AtlasProjectLinkReaderReceipt.decode(ProjectLinksFixture.json(value), index: index,
                    generation: 1, link: link, side: side, destination: 77)
            }
            func reject(_ value: [String: Any]) {
                do { _ = try admit(value); preconditionFailure("Changed import source evidence admitted") } catch {}
            }
            let receipt = try admit(value)
            precondition(receipt.initialOriginalOffset == (side == .evidence ? 11 : 0) && receipt.fragment == nil)
            precondition(receipt.info.identity.pathBytes == (side == .evidence ? link.source.path.bytes : link.target!.path.bytes))
            for (key, changed) in [
                ("initial_original_offset", "12"), ("source_sha256", String(repeating: "f", count: 64)),
                ("source_revision", "2"), ("reader_owner", "78"), ("additional_source_bytes_read", "1"),
                ("side", side == .evidence ? "target" : "evidence")
            ] {
                var bad = value; bad[key] = changed; reject(bad)
            }
            var bad = value; bad["source_reopened"] = true; reject(bad)
            bad = value; bad["compiler_resolved"] = true; reject(bad)
            bad = value; bad["evidence_range"] = ["start": "12", "end": "21"]; reject(bad)
            bad = value; bad["fragment_status"] = "resolved"; reject(bad)
            for key in ["destination", "source_link_id", "source_sha256", "resolution_basis"] {
                bad = value; var row = value["relationship"] as! [String: Any]
                row[key] = key == "source_link_id" ? "2" : "changed"
                bad["relationship"] = row; reject(bad)
            }
        }

        // The other projection's envelope cannot be decoded by changing a command.
        var substituted = ProjectLinksFixture.page(owner: index.basis.owner, generation: 1, command: "imports-page")
        substituted["schema"] = "fcb.atlas-imports/1"; refuse(substituted, index: index)
    }


    /// Language support is accepted only with its declared evidence policy.
    /// Scripted envelopes exercise the native consumer; real-source extraction
    /// and original-byte navigation are covered by the Rust host/ABI fixtures.
    private static func pythonWire(_ index: AtlasPreparedIndex) throws {
        var value = ProjectLinksFixture.importPage(owner: index.basis.owner,
            generation: 1, command: "imports-page")
        var rows = value["links"] as! [[String: Any]]
        for i in rows.indices {
            rows[i]["language"] = "python"
            rows[i]["lexical_import_kind"] = "python-from"
            rows[i]["destination"] = ".worker"
            rows[i]["resolution_basis"] = "python-relative-module-candidate"
            rows[i]["source_path"] = ProjectLinksFixture.path("pkg/main.py")
            rows[i]["target_path"] = ProjectLinksFixture.path("pkg/worker.py")
        }
        value["links"] = rows
        let page = try decode(value, index: index), link = page.links[0]
        precondition(link.canOpenTarget && link.importEvidence?.language == "python")
        precondition(link.importEvidence?.kind == "python-from"
            && link.evidenceLabel.contains("Python from-import statement")
            && link.evidenceLabel.contains("not compiler resolved"))
        precondition(link.source.path.bytes == Array("pkg/main.py".utf8)
            && link.target?.path.bytes == Array("pkg/worker.py".utf8))

        for (key, wrong) in [
            ("language", "rust"), ("language", "swift"),
            ("lexical_import_kind", "python-import"),
            ("lexical_import_kind", "javascript-import"),
            ("resolution_basis", "rust-module-file-candidate"),
            ("resolution_basis", "unique-relative-module-candidate"),
            ("resolution_basis", "external-specifier")
        ] {
            var bad = value, changed = rows
            changed[0][key] = wrong; bad["links"] = changed
            refuse(bad, index: index)
        }
        for (key, oldPolicy) in [
            ("extractor", "franken-markdown-d6fbd1d5/import-statements-v1"),
            ("resolver", "fcb-closed-catalog-imports-v1"),
            ("extraction_policy", "rust-mod-use-and-js-static-import-export-v1")
        ] {
            var bad = value; bad[key] = oldPolicy
            refuse(bad, index: index)
        }

        // Absolute Python imports keep readable source evidence without
        // assigning a local module by guessing sys.path or package execution.
        var external = value, externalRows = rows
        for i in externalRows.indices {
            externalRows[i]["lexical_import_kind"] = "python-import"
            externalRows[i]["destination"] = "external.library"
            externalRows[i]["resolution"] = "external"
            externalRows[i]["resolution_basis"] = "external-specifier"
            for key in ["target_file_id", "target_path", "target_revision", "target_sha256", "target_bytes"] {
                externalRows[i][key] = NSNull()
            }
        }
        external["links"] = externalRows
        let absolute = try decode(external, index: index).links[0]
        precondition(!absolute.canOpenTarget && absolute.importEvidence?.kind == "python-import"
            && absolute.evidenceLabel.contains("Python import statement"))
        for (resolution, basis) in [("missing", "no-catalog-target"),
            ("ambiguous", "ambiguous-catalog-target"), ("unresolved", "unsupported-specifier")] {
            var unavailable = external, unavailableRows = externalRows
            for i in unavailableRows.indices {
                unavailableRows[i]["lexical_import_kind"] = "python-from"
                unavailableRows[i]["destination"] = ".worker"
                unavailableRows[i]["resolution"] = resolution
                unavailableRows[i]["resolution_basis"] = basis
            }
            unavailable["links"] = unavailableRows
            let unavailablePage = try decode(unavailable, index: index)
            precondition(unavailablePage.links.allSatisfy { !$0.canOpenTarget })
        }
    }

    @MainActor static func main() async throws {
        let fixture = ProjectLinksFixture(includesImports: true), index = try await prepared(fixture)
        try wire(index)
        try pythonWire(index)
        let live = ProjectLinksFixture(includesImports: true)
        let model = AtlasProjectLinksModel(root: "/project", transport: live.transport)
        precondition(model.availableProjections == [.documentation, .imports])
        model.prepare(); await settled(model)
        let documentation = model.page!
        let documentReader = try await model.open(documentation.links[0], from: documentation,
            side: .evidence, readerTransport: live.readerTransport)
        model.selectProjection(.imports); await settled(model)
        let imports = model.page!
        precondition(imports.projection == .imports && imports.generation == 1 && live.index.count("create:") == 1,
            "Imports must reuse the frozen capture without replacing the Markdown inventory")
        let evidence = try await model.open(imports.links[0], from: imports, side: .evidence, readerTransport: live.readerTransport)
        let target = try await model.open(imports.links[0], from: imports, side: .target, readerTransport: live.readerTransport)
        precondition(evidence.reader.page?.start == 11 && evidence.reader.page?.text.hasPrefix("mod child;") == true
            && evidence.evidenceLabel.contains("11..<21") && !evidence.reader.canRefresh)
        precondition(target.reader.page?.start == 0 && target.reader.page?.text.hasPrefix("pub struct Child;") == true
            && target.evidenceLabel.contains("candidate") && target.evidenceLabel.contains("Compiler resolution"))
        let creates = live.log.filter { $0 == "create-reader" }.count
        do {
            _ = try await model.open(documentation.links[0], from: documentation, side: .evidence, readerTransport: live.readerTransport)
            preconditionFailure("Equal generation in another projection authorized an old row")
        } catch {}
        precondition(live.log.filter { $0 == "create-reader" }.count == creates)
        model.selectPath(Array("src/child.rs".utf8), direction: .incoming); await settled(model)
        precondition(model.page?.total == 70 && model.page?.filterFile == 4)
        model.loadPage(start: 64); await settled(model)
        precondition(model.page?.links.count == 6 && model.page?.links.first?.id == 65)
        model.selectProjection(.documentation); await settled(model)
        precondition(model.page?.projection == .documentation && model.page?.total == 0
            && model.page?.generation == 1 && model.selectedPath == Array("src/child.rs".utf8)
            && live.log.filter { $0 == "begin" }.count == 1,
            "Projection switching preserves the exact file filter and previously built inventory")
        model.selectProjection(.imports); await settled(model)
        precondition(model.page?.total == 70 && model.page?.generation == 1
            && live.log.filter { $0 == "imports-begin" }.count == 1)
        model.prepare(); await settled(model)
        precondition(model.page?.generation == 2 && live.index.count("create:") == 1)
        model.selectProjection(.documentation); await settled(model)
        precondition(model.page?.generation == 1, "Rebuilding imports does not replace Markdown generation")

        model.selectProjection(.imports); await settled(model)
        let accepted = model.page!
        live.patch { phase, value in
            var value = value
            if phase == "imports-reader" { value["initial_original_offset"] = "12" }
            return value
        }
        let closedBefore = live.log.filter { $0 == "close-reader" }.count
        do {
            _ = try await model.open(accepted.links[0], from: accepted, side: .evidence, readerTransport: live.readerTransport)
            preconditionFailure("Changed evidence offset published a reader")
        } catch {}
        precondition(live.log.filter { $0 == "close-reader" }.count == closedBefore + 1 && evidence.reader.page?.start == 11)
        live.patch { _, value in value }
        model.prepare(refresh: true); await settled(model)
        precondition(model.page?.generation == 1 && live.index.count("create:") == 2)
        model.selectProjection(.documentation); await settled(model)
        precondition(model.page?.generation == 1 && live.log.filter { $0 == "begin" }.count == 2,
            "A new capture invalidates both projection inventories")
        model.close()
        precondition(documentReader.reader.page != nil && evidence.reader.page?.start == 11 && target.reader.page?.start == 0,
            "Opened captures survive closing the relationship owner")
        documentReader.reader.close(); evidence.reader.close(); target.reader.close()

        let canceled = ProjectLinksFixture(includesImports: true)
        let canceledModel = AtlasProjectLinksModel(root: "/project", transport: canceled.transport)
        canceledModel.prepare(); await settled(canceledModel)
        canceled.pause("imports-step"); canceledModel.selectProjection(.imports)
        let deadline = ProcessInfo.processInfo.systemUptime + 10
        while !canceled.isBlocked && ProcessInfo.processInfo.systemUptime < deadline { await Task.yield() }
        precondition(canceled.isBlocked)
        canceledModel.cancel(); canceled.gate.signal(); await settled(canceledModel)
        precondition(canceledModel.page == nil && canceledModel.hasPreparedSources)
        canceledModel.selectProjection(.documentation); await settled(canceledModel)
        precondition(canceledModel.page?.generation == 1 && canceled.log.filter { $0 == "begin" }.count == 1)
        canceledModel.selectProjection(.imports); await settled(canceledModel)
        precondition(canceledModel.page?.generation == 2, "Canceled import attempts consume only their own namespace")
        canceledModel.close()

        let retiring = ProjectLinksFixture(includesImports: true)
        let retiringModel = AtlasProjectLinksModel(root: "/project", projection: .imports, transport: retiring.transport)
        retiring.pause("imports-begin"); retiringModel.prepare()
        let closeDeadline = ProcessInfo.processInfo.systemUptime + 10
        while !retiring.isBlocked && ProcessInfo.processInfo.systemUptime < closeDeadline { await Task.yield() }
        precondition(retiring.isBlocked)
        retiringModel.close(); retiring.gate.signal(); await settled(retiringModel)
        precondition(retiringModel.page == nil && !retiringModel.hasPreparedSources)
        print("AtlasProjectImportsTests: projection reuse, exact evidence, target candidates and cancellation passed")
    }
}
