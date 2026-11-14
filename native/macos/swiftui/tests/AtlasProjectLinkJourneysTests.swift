import Foundation

/// Exercises production preview/model/history admission with injected foreign
/// receipts. Rust owns real Markdown extraction and retained source transfer;
/// this fixture does not claim physical-Mac or real-engine qualification.
@main private enum AtlasProjectLinkJourneysTests {
    @MainActor private static func until(_ done: () -> Bool) async {
        let deadline = ProcessInfo.processInfo.systemUptime + 10
        while !done() && ProcessInfo.processInfo.systemUptime < deadline { await Task.yield() }
        precondition(done(), "Captured relationship journey did not drain")
    }

    @MainActor static func main() async throws {
        let fixture = ProjectLinksFixture(includesImports: true)
        let model = AtlasProjectLinksModel(root: "/project", initialPath: Array("guide.md".utf8),
            direction: .incoming, transport: fixture.transport)
        model.prepare(); await until { !model.busy }
        guard let visible = model.page else { preconditionFailure(model.notice) }
        let source = try await model.open(visible.links[0], from: visible, side: .evidence,
            readerTransport: fixture.readerTransport, document: fixture.documentTransport)
        source.reader.documentPreview.prepare()
        await until { !source.reader.documentPreview.busy }
        source.reader.documentPreview.prepareLinks()
        await until { !source.reader.documentPreview.busy }
        source.reader.documentPreview.nextLinks()
        await until { !source.reader.documentPreview.busy }
        guard let target = source.reader.documentPreview.links?.target(id: 65) else {
            preconditionFailure(source.reader.documentPreview.notice)
        }
        let history = AtlasDocumentLinkHistory<AtlasProjectLinkReaderCapture>(
            retire: { $0.reader.close() },
            failureMessage: { ($0 as? AtlasProjectLinksError)?.message ?? "Refused" })
        history.bind(root: "/project", accessIdentity: nil)
        history.follow { source }; await until { !history.busy }
        let metadataBefore = fixture.log.filter { $0 == "page" }.count
        history.follow {
            try await model.openDocumentLink(target, from: source,
                readerTransport: fixture.readerTransport, document: fixture.documentTransport)
        }
        await until { !history.busy }
        guard let linked = history.current, linked !== source else { preconditionFailure(history.notice) }
        precondition(linked.link.sourceLink == 65 && linked.reader.page?.start == 34
            && linked.fragment?.status == "resolved", "A late occurrence opens its exact retained heading")
        precondition(fixture.log.filter { $0 == "page" }.count == metadataBefore + 2,
            "The resolver must page beyond the first 64 metadata rows")
        precondition(model.page?.id == visible.id && model.direction == .incoming
            && model.selectedPath == Array("guide.md".utf8), "Following preserves the Inspector's place")
        let reads = fixture.log.filter { $0 == "window" }.count
        history.back(); precondition(history.current === source && history.canForward)
        history.forward(); precondition(history.current === linked)
        history.back()
        precondition(fixture.log.filter { $0 == "window" }.count == reads,
            "Back/Forward reuse independent readers without live I/O or receipt replay")

        // A source occurrence is qualified by exact destination bytes, block,
        // digest, revision and whole-document evidence, not a convenient path.
        for key in ["destination", "block_index", "source_sha256", "source_link_id"] {
            fixture.patch { phase, value in
                guard phase == "page", value["direction"] as? String == "outgoing",
                      value["offset"] as? String == "64" else { return value }
                var value = value, rows = value["links"] as! [[String: Any]]
                rows[0][key] = key == "destination" ? "guide.md#other"
                    : (key == "source_sha256" ? String(repeating: "c", count: 64) : "66")
                value["links"] = rows
                return value
            }
            history.follow {
                try await model.openDocumentLink(target, from: source,
                    readerTransport: fixture.readerTransport, document: fixture.documentTransport)
            }
            await until { !history.busy }
            precondition(history.current === source && history.canForward && history.captures.count == 2,
                "Changed occurrence authority must preserve both directions")
        }
        fixture.patch { phase, value in
            guard phase == "page", value["direction"] as? String == "outgoing" else { return value }
            var value = value, rows = value["links"] as! [[String: Any]]
            for index in rows.indices {
                rows[index]["resolution"] = "unavailable"
                for key in ["target_revision", "target_sha256", "target_bytes"] { rows[index].removeValue(forKey: key) }
            }
            value["links"] = rows
            return value
        }
        do {
            _ = try await model.openDocumentLink(target, from: source, readerTransport: fixture.readerTransport)
            preconditionFailure("An unavailable capture must not reopen its path")
        } catch AtlasProjectLinksError.unavailableDocumentTarget {}
        precondition(history.current === source && history.canForward)
        fixture.patch { _, value in value }

        // Selecting a different visible projection does not replace the
        // accepted documentation resolver or the retained document source.
        model.selectProjection(.imports); await until { !model.busy }
        guard let imports = model.page else { preconditionFailure(model.notice) }
        precondition(imports.projection == .imports && model.canFollowDocumentLinks(from: source))
        let another = try await model.openDocumentLink(target, from: source,
            readerTransport: fixture.readerTransport, document: fixture.documentTransport)
        precondition(model.page?.id == imports.id && another.link.projection == .documentation)
        another.reader.close()

        // A foreign reader registry is refused before any destination exists.
        let foreign = ProjectLinksFixture()
        do {
            _ = try await model.openDocumentLink(target, from: source, readerTransport: foreign.readerTransport)
            preconditionFailure("Foreign reader domain authorized a relationship")
        } catch AtlasProjectLinksError.staleDocumentLink {}
        precondition(!foreign.log.contains("create-reader"))

        // Entered destination work drains after cancellation. A late verified
        // receipt must retire its candidate and leave Forward intact.
        let closesBefore = fixture.log.filter { $0 == "close-reader" }.count
        fixture.pause("reader")
        history.follow {
            try await model.openDocumentLink(target, from: source,
                readerTransport: fixture.readerTransport, document: fixture.documentTransport)
        }
        await until { fixture.isBlocked }
        history.cancel()
        precondition(history.busy && history.canceling && history.current === source && history.canForward == false)
        fixture.gate.signal()
        await until { !history.busy && !model.busy }
        await until { fixture.log.filter { $0 == "close-reader" }.count > closesBefore }
        precondition(history.current === source && history.canForward && history.captures.count == 2)

        // A replaced preview generation revokes an old occurrence before I/O.
        source.reader.documentPreview.prepareLinks(); await until { !source.reader.documentPreview.busy }
        let callsBefore = fixture.log.count
        do {
            _ = try await model.openDocumentLink(target, from: source, readerTransport: fixture.readerTransport)
            preconditionFailure("A replaced preview page authorized an old occurrence")
        } catch AtlasProjectLinksError.staleDocumentLink {}
        precondition(fixture.log.count == callsBefore)

        model.prepare(refresh: true); await until { !model.busy }
        precondition(!model.canFollowDocumentLinks(from: source))
        history.forward(); precondition(history.current === linked && linked.reader.page?.start == 34)
        model.close(); history.back()
        precondition(history.current === source && source.reader.page?.text == ProjectLinksFixture.source(1),
            "Refreshing or closing relationships preserves retained history")
        history.close()
        print("AtlasProjectLinkJourneysTests: exact occurrences, late pages, preserved filters/history and canceled transfers passed")
    }
}
