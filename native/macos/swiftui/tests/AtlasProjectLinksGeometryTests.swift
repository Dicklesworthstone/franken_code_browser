import Foundation

/// Independent malformed-wire and presentation-budget scenarios. Execution
/// belongs to the code-first verification batch, not this implementation pass.
enum ProjectLinksGeometryTests {
    private static func parcel(_ file: UInt64) -> [String: String] {
        ["x": String(((file - 1) % 8) * 400), "y": String(((file - 1) / 8) * 200), "w": "200", "h": "100"]
    }
    private static func optionalParcel(_ value: Any?) -> Any {
        if let text = value as? String, let file = UInt64(text) { return parcel(file) }
        return NSNull()
    }
    private static func place(_ input: [String: Any]) -> [String: Any] {
        var value = input
        value["geometry_projection"] = "retained-atlas-parcels-v1"
        value["world"] = ["x": "0", "y": "0", "w": "4096", "h": "4096"]
        value["filter_parcel"] = optionalParcel(value["filter_file_id"])
        value["links"] = (input["links"] as! [[String: Any]]).map { row in
            var row = row
            row["source_parcel"] = parcel(UInt64(row["source_file_id"] as! String)!)
            row["target_parcel"] = optionalParcel(row["target_file_id"])
            return row
        }
        return value
    }
    static func run(_ index: AtlasPreparedIndex, projection: AtlasProjectRelationshipProjection) throws {
        let sourceID: UInt64 = projection == .documentation ? 1 : 3
        let targetID = sourceID + 1
        let sourcePath = ProjectLinksFixture.name(1, projection: projection)
        let targetPath = ProjectLinksFixture.name(2, projection: projection)
        func page(direction: AtlasProjectLinkDirection = .outgoing, path: String? = nil) -> [String: Any] {
            if projection == .documentation {
                return ProjectLinksFixture.page(owner: index.basis.owner, generation: 1,
                    command: "links-page", direction: direction, path: path ?? sourcePath)
            }
            return ProjectLinksFixture.importPage(owner: index.basis.owner, generation: 1,
                command: "imports-page", direction: direction, path: path ?? sourcePath)
        }
        func decode(_ value: [String: Any], direction: AtlasProjectLinkDirection = .outgoing,
            path: String? = nil) throws -> AtlasProjectLinksPage {
            try AtlasProjectLinksPage.decode(ProjectLinksFixture.json(value), index: index, generation: 1,
                command: projection.command("page"), direction: direction,
                path: Array((path ?? sourcePath).utf8), projection: projection)
        }
        func reject(_ value: [String: Any]) {
            do { _ = try decode(value); preconditionFailure("Malformed relationship geometry admitted") }
            catch {}
        }
        let legacy = try decode(page())
        precondition(legacy.geometry == nil && legacy.neighborhood == nil && legacy.links.count == 64)
        let valid = place(page()), admitted = try decode(valid)
        let map = admitted.neighborhood!
        precondition(map.selectedFile == sourceID && map.nodes.count == 2 && map.connections.count == 1)
        precondition(map.connections[0].source == sourceID && map.connections[0].target == targetID
            && map.connections[0].evidenceIDs == Array(UInt64(1)...64))
        precondition(map.representedLinks == 64 && map.otherPageLinks == 6
            && map.limitedLinks == 0 && map.unlocatedLinks == 0)
        let incoming = try decode(place(page(direction: .incoming, path: targetPath)), direction: .incoming, path: targetPath)
        precondition(incoming.neighborhood?.selectedFile == targetID)
        let empty = try decode(place(page(path: targetPath)), path: targetPath)
        precondition(empty.total == 0 && empty.neighborhood?.nodes.count == 1
            && empty.neighborhood?.connections.isEmpty == true,
            "A selected zero-edge file must retain its own catalog parcel")
        var whole = valid
        whole["direction"] = "all"; whole["filter_file_id"] = NSNull()
        whole["filter_path"] = NSNull(); whole["filter_parcel"] = NSNull()
        let wholePage = try AtlasProjectLinksPage.decode(ProjectLinksFixture.json(whole), index: index,
            generation: 1, command: projection.command("page"), projection: projection)
        precondition(wholePage.geometry != nil && wholePage.neighborhood == nil,
            "Whole-project rows must not silently become a selected-neighborhood graph")

        for key in ["geometry_projection", "world", "filter_parcel"] {
            var bad = valid; bad.removeValue(forKey: key); reject(bad)
            bad = valid; bad[key] = NSNull(); reject(bad)
        }
        var bad = valid; bad["geometry_projection"] = "current-app-frame"; reject(bad)
        bad = valid; bad["layout_revision"] = "4"; reject(bad)
        bad = valid; bad["filter_parcel"] = parcel(targetID); reject(bad)
        bad = page(); bad["world"] = NSNull(); reject(bad)
        for key in ["source_parcel", "target_parcel"] {
            var bad = valid, rows = valid["links"] as! [[String: Any]]
            rows[0].removeValue(forKey: key); bad["links"] = rows; reject(bad)
            rows = valid["links"] as! [[String: Any]]
            rows[0][key] = NSNull(); bad["links"] = rows; reject(bad)
        }
        let malformedCoordinates: [Any] = ["NaN", "Infinity", "-1", "4097", "1e309", 0]
        for malformed in malformedCoordinates {
            var bad = valid, rows = valid["links"] as! [[String: Any]]
            var wrong = parcel(targetID).mapValues { $0 as Any }
            wrong["x"] = malformed; rows[0]["target_parcel"] = wrong; bad["links"] = rows; reject(bad)
        }
        bad = valid
        var rows = valid["links"] as! [[String: Any]]
        rows[1]["target_parcel"] = parcel(targetID + 1); bad["links"] = rows; reject(bad)
        bad = valid; bad["world"] = ["x": "0", "y": "0", "w": "4095", "h": "4096"]; reject(bad)

        if projection == .imports {
            var receipt = ProjectLinksFixture.importReceipt(owner: index.basis.owner, reader: 77,
                generation: 1, link: 1, side: .evidence)
            receipt["relationship"] = (valid["links"] as! [[String: Any]])[0]
            _ = try AtlasProjectLinkReaderReceipt.decode(ProjectLinksFixture.json(receipt), index: index,
                generation: 1, link: admitted.links[0], side: .evidence, destination: 77)
            var row = receipt["relationship"] as! [String: Any]
            row["target_parcel"] = parcel(targetID + 1); receipt["relationship"] = row
            do {
                _ = try AtlasProjectLinkReaderReceipt.decode(ProjectLinksFixture.json(receipt), index: index,
                    generation: 1, link: admitted.links[0], side: .evidence, destination: 77)
                preconditionFailure("Reader substituted different relationship geometry")
            } catch {}
        } else {
            var selfPage = valid, row = (valid["links"] as! [[String: Any]])[0]
            for (target, source) in [("target_file_id", "source_file_id"), ("target_path", "source_path"),
                ("target_revision", "source_revision"), ("target_sha256", "source_sha256"),
                ("target_bytes", "source_bytes"), ("target_parcel", "source_parcel")] { row[target] = row[source] }
            selfPage["links"] = [row]; selfPage["matching_links"] = "1"; selfPage["next_offset"] = NSNull()
            let selfMap = try decode(selfPage).neighborhood!
            precondition(selfMap.nodes.count == 1 && selfMap.connections.count == 1
                && selfMap.connections[0].source == selfMap.connections[0].target)
        }

        // More distinct direct neighbors than the map budget; a repeated pair
        // after the cutoff must still contribute its independent evidence ID.
        var crowded = page(), candidates: [[String: Any]] = []
        for id in UInt64(1)...40 {
            var row = projection == .documentation ? ProjectLinksFixture.row(id) : ProjectLinksFixture.importRow(id)
            row["target_file_id"] = String(targetID + id - 1)
            row["target_path"] = ProjectLinksFixture.path("neighbors/\(id).rs")
            candidates.append(row)
        }
        var duplicate = candidates[0]; duplicate["link_id"] = "41"; duplicate["source_link_id"] = "41"
        candidates.append(duplicate)
        var external = candidates[0]; external["link_id"] = "42"; external["source_link_id"] = "42"
        external["resolution"] = "external"
        if projection == .imports { external["resolution_basis"] = "external-specifier" }
        for key in ["target_file_id", "target_path", "target_revision", "target_sha256", "target_bytes"] { external[key] = NSNull() }
        candidates.append(external); crowded["links"] = candidates
        crowded = place(crowded)
        let links = try (crowded["links"] as! [[String: Any]]).map {
            try AtlasProjectLink(AtlasIndexWire.decode(ProjectLinksFixture.json($0)), projection: projection)
        }
        let geometry = try AtlasProjectLinksGeometry(AtlasIndexWire.decode(ProjectLinksFixture.json(crowded)),
            filterFile: sourceID, links: links)
        let capped = AtlasProjectLinksNeighborhood.make(geometry: geometry, filterFile: sourceID,
            path: try AtlasProjectLinkPath(bytes: Array(sourcePath.utf8)), links: links, total: 42)!
        precondition(capped.connections.count == 32 && capped.nodes.count == 33
            && capped.connections.map(\.id) == Array(UInt64(1)...32))
        precondition(capped.connections[0].evidenceIDs == [1, 41] && capped.representedLinks == 33
            && capped.limitedLinks == 8 && capped.unlocatedLinks == 1 && capped.otherPageLinks == 0)
    }
}
