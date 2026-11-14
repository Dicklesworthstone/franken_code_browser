import Foundation

/// Reconcile a bounded metadata observation without moving surviving parcels.
/// Raw Unix bytes are identity; catalog row numbers are local to one response.
/// Missing paths from an incomplete scan remain explicitly unconfirmed.
enum AtlasRefreshLayout {
    struct Result: Sendable {
        let entries: [AtlasProjectCatalog.Entry]
        let addedRawPaths: Set<[UInt8]>
        let unconfirmedRawPaths: Set<[UInt8]>
        let removedCount: Int
    }

    /// The supplied packer is the existing Rust parcel-layout adapter in the
    /// native host. Only new members need packing; source/size changes do not
    /// displace established landmarks. Explicit scope repack still compacts all.
    static func reconcile(previous: [AtlasProjectCatalog.Entry], observed: [AtlasProjectCatalog.Entry],
        complete: Bool, reservedRightEdge: Double? = nil,
        pack: ([AtlasProjectCatalog.Entry]) -> [AtlasProjectCatalog.Entry]?) -> Result? {
        guard previous.count <= AtlasProjectCatalog.maximumFiles,
              observed.count <= AtlasProjectCatalog.maximumFiles,
              reservedRightEdge.map({ $0.isFinite && abs($0) <= 1e12 }) ?? true else { return nil }
        var earlier: [[UInt8]: AtlasProjectCatalog.Entry] = [:]
        var current: [[UInt8]: AtlasProjectCatalog.Entry] = [:]
        for entry in previous {
            guard earlier.updateValue(entry, forKey: entry.rawPath) == nil else { return nil }
        }
        for entry in observed {
            guard current.updateValue(entry, forKey: entry.rawPath) == nil else { return nil }
        }
        let unconfirmed = complete ? Set<[UInt8]>() : Set(earlier.keys.filter { current[$0] == nil })
        let added = Set(current.keys.filter { earlier[$0] == nil })
        guard current.count + unconfirmed.count <= AtlasProjectCatalog.maximumFiles else { return nil }
        var retained: [AtlasProjectCatalog.Entry] = []
        retained.reserveCapacity(previous.count)
        for old in previous {
            guard let fresh = current[old.rawPath] ?? (complete ? nil : old) else { continue }
            retained.append(copy(fresh, id: fresh.id, x: old.x, y: old.y, width: old.width, height: old.height))
        }
        let additions = observed.filter { added.contains($0.rawPath) }
        var placed: [AtlasProjectCatalog.Entry] = []
        if !additions.isEmpty {
            guard let packed = pack(additions), packed.count == additions.count,
                  Set(packed.map(\.rawPath)) == added else { return nil }
            // Place the new neighborhood beyond the previous coordinate frame,
            // including removed edge parcels, so an old camera location remains
            // meaningful and a new file never inherits a removed file's place.
            let edge = max(previous.map { $0.x + $0.width }.max() ?? 0, reservedRightEdge ?? 0)
            let originX = packed.map(\.x).min() ?? 0
            let originY = packed.map(\.y).min() ?? 0
            let offsetX = previous.isEmpty && reservedRightEdge == nil ? 0 : edge + 32 - originX
            let offsetY = previous.isEmpty ? 0 : (previous.map(\.y).min() ?? 0) - originY
            for entry in packed {
                guard let fresh = current[entry.rawPath] else { return nil }
                placed.append(copy(fresh, id: fresh.id, x: entry.x + offsetX, y: entry.y + offsetY,
                    width: entry.width, height: entry.height))
            }
        }
        let combined = (retained + placed).sorted { $0.rawPath.lexicographicallyPrecedes($1.rawPath) }
        guard combined.allSatisfy({ entry in
            [entry.x, entry.y, entry.width, entry.height, entry.x + entry.width, entry.y + entry.height]
                .allSatisfy { $0.isFinite && abs($0) <= 1e12 }
                && entry.width > 0 && entry.height > 0
        }) else { return nil }
        // IDs remain unique even when a fresh catalog reused an old row number
        // for a different path. All persistent selections continue using bytes.
        let entries = combined.enumerated().map { index, entry in
            copy(entry, id: UInt64(index + 1), x: entry.x, y: entry.y, width: entry.width, height: entry.height)
        }
        return Result(entries: entries, addedRawPaths: added, unconfirmedRawPaths: unconfirmed,
            removedCount: complete ? previous.count - retained.count : 0)
    }

    private static func copy(_ entry: AtlasProjectCatalog.Entry, id: UInt64,
        x: Double, y: Double, width: Double, height: Double) -> AtlasProjectCatalog.Entry {
        AtlasProjectCatalog.Entry(id: id, rawPath: entry.rawPath, displayPath: entry.displayPath,
            sourcePath: entry.sourcePath, observedBytes: entry.observedBytes,
            x: x, y: y, width: width, height: height)
    }
}
