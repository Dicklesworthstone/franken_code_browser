import Foundation
import CoreGraphics
import CoreText

@_silgen_name("fcb_text_parcel_positions")
private func parcelPositions(_ paths: UnsafePointer<UnsafePointer<CChar>?>?, _ areas: UnsafePointer<Double>?,
                             _ count: UInt64, _ aspect: Double) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_free_string") private func parcelFree(_ value: UnsafeMutablePointer<CChar>)

/// Keep every continuation of a file inside one engine-owned hierarchy parcel.
/// Source shaping stays in native coordinates; this only places retained tiles.
enum AtlasParcelLayout {
    /// Repartition source-order rows, not glyphs. Both cold CTLines and restored
    /// prepared runs are reused without typesetting or changing source ranges.
    static func reflow(_ documents: [String: AtlasDocument]) -> [AtlasTextTile]? {
        var assignments: [(AtlasDocument, [AtlasTextTile])] = []
        var geometry: [(AtlasTextTile, CGRect, CGRect, Bool)] = []
        var total = 0
        for path in documents.keys.sorted() {
            let document = documents[path]!
            guard let firstTile = document.tiles.first else { continue }
            guard let parcel = firstTile.parcelRect else { return nil }
            let inset = min(5.0, min(parcel.width, parcel.height) * 0.02)
            let inner = parcel.insetBy(dx: inset, dy: inset)
            let rowCount = document.tiles.reduce(0) { $0 + $1.lineCount }
            guard rowCount > 0 else { return nil }
            var bestColumns = 1, bestScale = 0.0
            for columns in 1...min(256, rowCount) {
                let longest = (rowCount + columns - 1) / columns
                let scale = min(inner.width / (Double(columns) * (AtlasTextTile.width + 4) - 4),
                                inner.height / (22 + Double(longest) * AtlasTextTile.lineHeight))
                if scale > bestScale { bestScale = scale; bestColumns = columns }
            }
            guard bestScale.isFinite, bestScale > 0, bestColumns <= 65536 - total else { return nil }
            let prepared = document.tiles.allSatisfy { $0.preparedLines != nil }
            guard prepared || document.tiles.allSatisfy({ $0.preparedLines == nil }) else { return nil }
            let coldRows = prepared ? [] : document.tiles.flatMap(\.lines)
            let warmRows = prepared ? document.tiles.flatMap { $0.preparedLines! } : []
            let ranges: [NSRange] = prepared ? warmRows.map(\.sourceRange) : coldRows.map {
                let range = CTLineGetStringRange($0)
                return NSRange(location: range.location, length: range.length)
            }
            var cursor = 0
            let sourceLength = document.source.text.utf16.count
            for range in ranges {
                guard range.location == cursor, range.length > 0,
                      range.length <= sourceLength - cursor else { return nil }
                cursor += range.length
            }
            guard cursor == sourceLength else { return nil }
            var columns: [AtlasTextTile] = [], row = 0
            for column in 0..<bestColumns {
                let count = rowCount / bestColumns + (column < rowCount % bestColumns ? 1 : 0)
                let end = row + count
                let sourceRange = NSRange(location: ranges[row].location,
                    length: ranges[end - 1].location + ranges[end - 1].length - ranges[row].location)
                let tile: AtlasTextTile
                // Reuse existing display images when their exact row partition
                // survives a refresh. Geometry changes are handled by PNG keys.
                if let existing = document.displayTiles, existing.count == bestColumns,
                   existing[column].sourceRange == sourceRange, existing[column].lineCount == count {
                    tile = existing[column]
                } else if prepared {
                    guard let header = firstTile.preparedHeader else { return nil }
                    tile = AtlasTextTile(path: path, preparedLines: Array(warmRows[row..<end]), header: header, sourceRange: sourceRange)
                } else {
                    tile = AtlasTextTile(path: path, lines: Array(coldRows[row..<end]), sourceRange: sourceRange,
                        part: 0, retainedHeader: firstTile.header)
                }
                let rect = CGRect(x: inner.minX + Double(column) * (AtlasTextTile.width + 4) * bestScale,
                    y: inner.minY, width: AtlasTextTile.width * bestScale, height: tile.height * bestScale)
                geometry.append((tile, rect, parcel, column == 0))
                columns.append(tile); row = end
            }
            total += columns.count; assignments.append((document, columns))
        }
        for (tile, rect, parcel, first) in geometry {
            tile.rect = rect; tile.parcelRect = parcel; tile.parcelFirst = first
        }
        for (document, tiles) in assignments { document.displayTiles = tiles }
        return assignments.flatMap { $0.1 }
    }

    static func place(_ tiles: [AtlasTextTile], aspect: Double = 1.9) -> CGRect? {
        guard !tiles.isEmpty, tiles.count <= 65536 else { return nil }
        let groups = Dictionary(grouping: tiles, by: \.path)
        let paths = groups.keys.sorted()
        guard paths.count <= 20000 else { return nil }
        let areas = paths.map { path in
            groups[path]!.reduce(0.0) { $0 + AtlasTextTile.width * $1.height }
        }
        let strings = paths.map { strdup($0) }
        defer { for pointer in strings { free(pointer) } }
        guard strings.allSatisfy({ $0 != nil }) else { return nil }
        let pointers: [UnsafePointer<CChar>?] = strings.map { $0.map { UnsafePointer($0) } }
        let response = pointers.withUnsafeBufferPointer { p in
            areas.withUnsafeBufferPointer { a in parcelPositions(p.baseAddress, a.baseAddress, UInt64(paths.count), aspect) }
        }
        guard let response else { return nil }
        defer { parcelFree(response) }
        guard let data = String(cString: response).data(using: .utf8),
              let rectangles = try? JSONDecoder().decode([[Double]].self, from: data),
              rectangles.count == paths.count else { return nil }
        var bounds = CGRect.null
        // Prepare all placements before mutating the published tile objects.
        var placements: [(AtlasTextTile, CGRect, CGRect, Bool)] = []
        for (path, values) in zip(paths, rectangles) {
            guard values.count == 4, values.allSatisfy(\.isFinite), values[2] > 0, values[3] > 0,
                  let members = groups[path], !members.isEmpty else { return nil }
            let parcel = CGRect(x: values[0], y: values[1], width: values[2], height: values[3])
            let inset = min(5.0, min(parcel.width, parcel.height) * 0.02)
            let inner = parcel.insetBy(dx: inset, dy: inset)
            guard let positions = fit(members, inside: inner) else { return nil }
            for (index, tile) in members.enumerated() {
                placements.append((tile, positions[index], parcel, index == 0))
            }
            bounds = bounds.union(parcel)
        }
        for (tile, rect, parcel, first) in placements {
            tile.rect = rect; tile.parcelRect = parcel; tile.parcelFirst = first
        }
        return bounds
    }

    /// Optimize bounded contiguous-column arrangements; never scatter a file's
    /// continuations among other files, clip text, or distort glyph proportions.
    static func fit(_ tiles: [AtlasTextTile], inside rect: CGRect) -> [CGRect]? {
        guard !tiles.isEmpty, tiles.count <= 65536, rect.width > 0, rect.height > 0,
              rect.minX.isFinite, rect.minY.isFinite, rect.width.isFinite, rect.height.isFinite else { return nil }
        let heights = tiles.map(\.height)
        guard heights.allSatisfy({ $0.isFinite && $0 > 0 }) else { return nil }
        let gap = 4.0
        let width = AtlasTextTile.width
        let maximumColumns = min(256, tiles.count)
        // For a fixed scale, maximal source-order prefixes minimize column
        // count. This feasibility test considers every contiguous partition,
        // including unequal tile counts and shorter final source pages.
        func partition(at scale: Double) -> [Int]? {
            guard scale.isFinite, scale > 0 else { return nil }
            let capacity = min(Double(maximumColumns), floor((rect.width / scale + gap) / (width + gap)))
            guard capacity >= 1 else { return nil }
            let availableHeight = rect.height / scale
            var ends: [Int] = [], usedHeight = 0.0
            for (index, height) in heights.enumerated() {
                guard height <= availableHeight else { return nil }
                let next = usedHeight == 0 ? height : usedHeight + gap + height
                if next > availableHeight {
                    ends.append(index)
                    if ends.count >= Int(capacity) { return nil }
                    usedHeight = height
                } else { usedHeight = next }
            }
            ends.append(heights.count)
            return ends
        }
        var lower = 0.0
        var upper = min(rect.width / width, rect.height / (heights.max() ?? 1))
        guard upper.isFinite, upper > 0 else { return nil }
        var bestEnds: [Int] = []
        if let ends = partition(at: upper) { lower = upper; bestEnds = ends }
        else {
            // Fixed work bound, independent of page count or aspect ratio.
            for _ in 0..<48 {
                let middle = lower + (upper - lower) / 2
                if let ends = partition(at: middle) { lower = middle; bestEnds = ends }
                else { upper = middle }
            }
        }
        guard lower > 0, !bestEnds.isEmpty else { return nil }
        var positions: [CGRect] = []
        positions.reserveCapacity(tiles.count)
        var start = 0
        for (column, end) in bestEnds.enumerated() {
            var y = rect.minY
            for index in start..<end {
                let height = heights[index] * lower
                positions.append(CGRect(x: rect.minX + Double(column) * (width + gap) * lower,
                                        y: y, width: width * lower, height: height))
                y += height + gap * lower
            }
            start = end
        }
        return positions.count == tiles.count ? positions : nil
    }
}
