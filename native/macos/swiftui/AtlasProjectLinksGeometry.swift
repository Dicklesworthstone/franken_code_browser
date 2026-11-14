import Foundation

/// Immutable positions from the same atlas owner as the relationship receipt.
/// Geometry never grants access to a source or describes a presented app frame.
struct AtlasProjectLinkParcel: Equatable, Sendable {
    let x, y, width, height: Double
    var maxX: Double { x + width }
    var maxY: Double { y + height }

    init(_ wire: AtlasIndexWire) throws {
        func coordinate(_ key: String) throws -> Double {
            let text = try wire.text(key)
            guard !text.isEmpty, text.utf8.count <= 400,
                  text.utf8.allSatisfy({ (48...57).contains($0) || [43, 45, 46, 69, 101].contains($0) }),
                  let value = Double(text), value.isFinite else { throw AtlasProjectLinksError.invalidResponse }
            return value
        }
        x = try coordinate("x"); y = try coordinate("y")
        width = try coordinate("w"); height = try coordinate("h")
        guard [x, y, width, height, maxX, maxY].allSatisfy({ $0.isFinite && $0 >= 0 && $0 <= 4096.000001 }) else {
            throw AtlasProjectLinksError.invalidResponse
        }
    }
    private init(x: Double, y: Double, width: Double, height: Double) {
        self.x = x; self.y = y; self.width = width; self.height = height
    }
    func contains(_ other: Self) -> Bool {
        other.x >= x && other.y >= y && other.maxX <= maxX + 0.000001 && other.maxY <= maxY + 0.000001
    }
    func union(_ other: Self) -> Self {
        let left = min(x, other.x), top = min(y, other.y)
        return Self(x: left, y: top, width: max(maxX, other.maxX) - left, height: max(maxY, other.maxY) - top)
    }
}

struct AtlasProjectLinkGeometry: Equatable, Sendable {
    let source: AtlasProjectLinkParcel
    let target: AtlasProjectLinkParcel?

    init?(_ wire: AtlasIndexWire, hasTarget: Bool) throws {
        let sourcePresent = wire.fields.contains(.init("source_parcel"))
        let targetPresent = wire.fields.contains(.init("target_parcel"))
        if !sourcePresent && !targetPresent { return nil }
        guard sourcePresent && targetPresent else { throw AtlasProjectLinksError.invalidResponse }
        source = try AtlasProjectLinkParcel(wire.object("source_parcel"))
        if hasTarget {
            target = try AtlasProjectLinkParcel(wire.object("target_parcel"))
        } else {
            guard try wire.fields.decodeNil(forKey: .init("target_parcel")) else { throw AtlasProjectLinksError.invalidResponse }
            target = nil
        }
    }
}

struct AtlasProjectLinksGeometry: Equatable, Sendable {
    let world: AtlasProjectLinkParcel
    let selection: AtlasProjectLinkParcel?

    init?(_ wire: AtlasIndexWire, filterFile: UInt64?, links: [AtlasProjectLink]) throws {
        if !wire.fields.contains(.init("geometry_projection")) {
            guard !wire.fields.contains(.init("world")), !wire.fields.contains(.init("filter_parcel")),
                  links.allSatisfy({ $0.geometry == nil }) else { throw AtlasProjectLinksError.invalidResponse }
            return nil // Older transports remain a fully usable evidence list.
        }
        guard try wire.text("geometry_projection") == "retained-atlas-parcels-v1",
              wire.fields.contains(.init("filter_parcel")) else { throw AtlasProjectLinksError.invalidResponse }
        world = try AtlasProjectLinkParcel(wire.object("world"))
        guard world.x == 0, world.y == 0, world.width == 4096, world.height == 4096 else {
            throw AtlasProjectLinksError.invalidResponse
        }
        if filterFile != nil {
            selection = try AtlasProjectLinkParcel(wire.object("filter_parcel"))
        } else {
            guard try wire.fields.decodeNil(forKey: .init("filter_parcel")) else { throw AtlasProjectLinksError.invalidResponse }
            selection = nil
        }
        let checkedWorld = world
        var parcels: [UInt64: AtlasProjectLinkParcel] = [:]
        func admit(_ parcel: AtlasProjectLinkParcel, file: UInt64) throws {
            guard checkedWorld.contains(parcel), parcels[file].map({ $0 == parcel }) != false else {
                throw AtlasProjectLinksError.invalidResponse
            }
            parcels[file] = parcel
        }
        if let filterFile, let selection { try admit(selection, file: filterFile) }
        for link in links {
            guard let geometry = link.geometry else { throw AtlasProjectLinksError.invalidResponse }
            try admit(geometry.source, file: link.source.id)
            if let target = geometry.target, let file = link.targetFile { try admit(target, file: file) }
        }
    }
}

/// A presentation of this page's direct occurrences only. This performs no
/// traversal, reachability or dependency analysis; pair counts retain every
/// admitted occurrence ID and leave the original evidence rows untouched.
struct AtlasProjectLinksNeighborhood: Sendable {
    static let maximumConnections = 32
    struct Node: Identifiable, Sendable {
        let id: UInt64
        let path: AtlasProjectLinkPath
        let parcel: AtlasProjectLinkParcel
    }
    struct Connection: Identifiable, Sendable {
        let source, target: UInt64
        var evidenceIDs: [UInt64]
        var id: UInt64 { evidenceIDs[0] }
        var count: Int { evidenceIDs.count }
    }
    private struct Pair: Hashable { let source, target: UInt64 }

    let selectedFile: UInt64
    let nodes: [Node]
    let connections: [Connection]
    let bounds: AtlasProjectLinkParcel
    let representedLinks, unlocatedLinks, limitedLinks: Int
    let otherPageLinks: UInt64

    static func make(geometry: AtlasProjectLinksGeometry?, filterFile: UInt64?, path: AtlasProjectLinkPath?,
        links: [AtlasProjectLink], total: UInt64) -> Self? {
        guard links.count <= 128, UInt64(links.count) <= total,
              let geometry, let filterFile, let path, let selection = geometry.selection else { return nil }
        var nodes = [Node(id: filterFile, path: path, parcel: selection)]
        var nodeIDs: Set<UInt64> = [filterFile]
        var connections: [Connection] = [], positions: [Pair: Int] = [:]
        var bounds = selection, represented = 0, unlocated = 0, limited = 0
        for link in links {
            guard let placement = link.geometry, let target = placement.target,
                  let targetFile = link.targetFile, let targetPath = link.targetPath else { unlocated += 1; continue }
            let pair = Pair(source: link.source.id, target: targetFile)
            if let position = positions[pair] {
                connections[position].evidenceIDs.append(link.id); represented += 1
                continue
            }
            guard connections.count < maximumConnections else { limited += 1; continue }
            positions[pair] = connections.count
            connections.append(Connection(source: pair.source, target: pair.target, evidenceIDs: [link.id]))
            represented += 1
            for node in [Node(id: link.source.id, path: link.source.path, parcel: placement.source),
                         Node(id: targetFile, path: targetPath, parcel: target)] where nodeIDs.insert(node.id).inserted {
                nodes.append(node); bounds = bounds.union(node.parcel)
            }
        }
        return Self(selectedFile: filterFile, nodes: nodes, connections: connections, bounds: bounds,
            representedLinks: represented, unlocatedLinks: unlocated, limitedLinks: limited,
            otherPageLinks: total - UInt64(links.count))
    }

    var summary: String {
        var parts = ["\(connections.count) directed connections represent \(representedLinks) evidence records on this page."]
        if unlocatedLinks > 0 { parts.append("\(unlocatedLinks) records have no catalog target to place.") }
        if limitedLinks > 0 { parts.append("\(limitedLinks) records exceed the \(Self.maximumConnections)-connection map limit.") }
        if otherPageLinks > 0 { parts.append("\(otherPageLinks) records are on other result pages.") }
        return parts.joined(separator: " ")
    }
}
