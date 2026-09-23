import Foundation
import CoreGraphics
import CoreText

@main struct AtlasParcelLayoutTests {
    static func document(_ path: String, _ lines: Int) -> AtlasDocument {
        let text = (0..<lines).map { "let value\($0) = \($0);" }.joined(separator: "\n")
        return AtlasDocument(path: path, capture: AtlasHighlightCapture(schema: "fcb.source-document/1", text: text,
            runs: [.init(start: "0", length: String(text.utf16.count), role: "keyword")]))!
    }
    static func close(_ a: Double, _ b: Double) -> Bool { abs(a - b) <= 1e-7 * max(1, max(abs(a), abs(b))) }
    static func contained(_ child: CGRect, in parent: CGRect) -> Bool {
        child.minX >= parent.minX - 1e-7 && child.minY >= parent.minY - 1e-7 &&
        child.maxX <= parent.maxX + 1e-7 && child.maxY <= parent.maxY + 1e-7
    }
    // Exhaust every contiguous partition for tiny cases, independently of the
    // production feasibility search. The optimal scale is an exact upper bound.
    static func exhaustiveScale(_ tiles: [AtlasTextTile], _ box: CGRect) -> Double {
        var best = 0.0
        for mask in 0..<(1 << (tiles.count - 1)) {
            var columns = 1, height = tiles[0].height, tallest = 0.0
            for index in 1..<tiles.count {
                if mask & (1 << (index - 1)) != 0 {
                    tallest = max(tallest, height); height = tiles[index].height; columns += 1
                } else { height += 4 + tiles[index].height }
            }
            tallest = max(tallest, height)
            best = max(best, min(box.width / (Double(columns) * (AtlasTextTile.width + 4) - 4), box.height / tallest))
        }
        return best
    }
    static func heightAwarePartitions() {
        let tall = document("tall.rs", 80).tiles[0]
        let short = document("short.rs", 10).tiles[0]
        let box = CGRect(x: 0, y: 0, width: 2 * AtlasTextTile.width + 4, height: tall.height)
        let unequal = [tall, short, short]
        let placed = AtlasParcelLayout.fit(unequal, inside: box)!
        precondition(close(placed[0].width / AtlasTextTile.width, 1),
                     "one tall page plus two short pages should fill two columns at native scale")
        precondition(close(placed[1].minX, placed[2].minX) && placed[1].minX > placed[0].minX,
                     "height-aware partition must use unequal page counts")
        // Equal-count two-column packing needs tall+short in the first column;
        // three columns sacrifice width. Neither can reach the proven scale 1.
        let oldScale = max(box.height / (tall.height + 2 * short.height + 8),
            max(box.height / (tall.height + short.height + 4), box.width / (3 * AtlasTextTile.width + 8)))
        precondition(placed[0].width / AtlasTextTile.width > oldScale * 1.05,
                     "regression must materially improve the old sparse arrangement")
        for members in [[tall, short, short], [short, tall, short, tall], [tall, tall, short, short, short]] {
            for size in [CGSize(width: 900, height: 1800), CGSize(width: 2400, height: 700), CGSize(width: 1300, height: 1302)] {
                let target = CGRect(origin: .zero, size: size)
                let result = AtlasParcelLayout.fit(members, inside: target)!
                let scale = result[0].width / AtlasTextTile.width
                precondition(close(scale, exhaustiveScale(members, target)), "fit must reach independent exhaustive optimum")
                for (index, rect) in result.enumerated() {
                    precondition(contained(rect, in: target))
                    precondition(close(rect.height / members[index].height, scale), "no glyph distortion")
                    for previous in result[..<index] {
                        let overlap = previous.intersection(rect)
                        precondition(overlap.isNull || overlap.width <= 1e-7 || overlap.height <= 1e-7)
                    }
                }
            }
        }
    }
    static func main() {
        // Native filtering repacks cached documents, then restores All. No
        // source/shaping call is involved and every captured row survives.
        let docs = [document("src/lib.rs", 180), document("docs/README.md", 53), document("tool.py", 31)]
        let all = Dictionary(uniqueKeysWithValues: docs.map { ($0.source.path, $0) })
        let scopedTiles = docs.flatMap(\.tiles)
        precondition(AtlasParcelLayout.place(scopedTiles) != nil)
        let original = AtlasParcelLayout.reflow(all)!
        let originalRects = original.map(\.rect)
        let originalRanges = original.map(\.sourceRange)
        for selected in docs {
            precondition(AtlasParcelLayout.place(selected.tiles) != nil)
            let filtered = AtlasParcelLayout.reflow([selected.source.path: selected])!
            precondition(filtered.reduce(0) { $0 + $1.sourceRange.length } == selected.source.text.utf16.count)
            precondition(filtered.allSatisfy { $0.path == selected.source.path })
        }
        precondition(AtlasParcelLayout.place(scopedTiles) != nil)
        let restoredAll = AtlasParcelLayout.reflow(all)!
        precondition(restoredAll.map(\.rect) == originalRects && restoredAll.map(\.sourceRange) == originalRanges)

        heightAwarePartitions()
        balancedRows()
        let documents = [document("src/a.rs", 170), document("src/feature/b.rs", 1050),
                         document("docs/guide.md", 50), document("README.md", 1)]
        let tiles = documents.flatMap(\.tiles)
        for size in [CGSize(width: 1000, height: 400), CGSize(width: 400, height: 1000), CGSize(width: 20, height: 20)] {
            let box = CGRect(origin: CGPoint(x: 71, y: 93), size: size)
            let positions = AtlasParcelLayout.fit(tiles, inside: box)!
            precondition(positions.count == tiles.count)
            let scale = positions[0].width / AtlasTextTile.width
            for (index, rect) in positions.enumerated() {
                precondition(contained(rect, in: box), "every continuation must remain inside parcel")
                precondition(close(rect.width / AtlasTextTile.width, scale) && close(rect.height / tiles[index].height, scale),
                             "one uniform scale must preserve all glyph proportions")
                if index > 0 {
                    let previous = positions[index - 1]
                    precondition(rect.minX >= previous.minX && (rect.minX > previous.minX || rect.minY > previous.minY),
                                 "continuations preserve column-major reading order")
                }
                for earlier in positions[..<index] {
                    let overlap = rect.intersection(earlier)
                    precondition(overlap.isNull || overlap.width <= 1e-7 || overlap.height <= 1e-7, "no overlapping source tiles")
                }
            }
        }
        let bounds = AtlasParcelLayout.place(tiles, aspect: 1.9)!
        precondition(bounds.width.isFinite && bounds.height.isFinite && bounds.width > 0 && bounds.height > 0)
        var parcels: [CGRect] = []
        for document in documents {
            let parcel = document.tiles[0].parcelRect!
            precondition(contained(parcel, in: bounds))
            precondition(document.tiles.filter(\.parcelFirst).count == 1 && document.tiles[0].parcelFirst)
            let scale = document.tiles[0].contentScale
            var cursor = 0
            for tile in document.tiles {
                precondition(tile.parcelRect == parcel && contained(tile.rect, in: parcel))
                precondition(close(tile.contentScale, scale) && close(tile.rect.height / tile.height, scale))
                precondition(tile.sourceRange.location == cursor); cursor += tile.sourceRange.length
            }
            precondition(cursor == document.source.text.utf16.count, "layout preserves every source UTF16 unit")
            for earlier in parcels { let overlap = parcel.intersection(earlier); precondition(overlap.isNull || overlap.width <= 1e-7 || overlap.height <= 1e-7) }
            parcels.append(parcel)
        }
        let placed = tiles.map(\.rect)
        precondition(AtlasParcelLayout.place(tiles, aspect: .nan) == nil)
        precondition(tiles.map(\.rect) == placed, "failed native layout does not mutate published rectangles")
        precondition(AtlasParcelLayout.fit([], inside: bounds) == nil)
        precondition(AtlasParcelLayout.fit(tiles, inside: .zero) == nil)
        precondition(AtlasParcelLayout.place([]) == nil)
        print("AtlasParcelLayout: real hierarchy ABI, grouping, complete continuation coverage, uniform scaling and failure atomicity passed")
    }

    static func balancedRows() {
        let source = document("src/balanced.rs", 170)
        let parcel = CGRect(x: 71, y: 93, width: 1600, height: 1000)
        for tile in source.tiles { tile.parcelRect = parcel }
        let originalRanges = source.tiles.map(\.sourceRange)
        let originalLines = source.tiles.flatMap(\.lines)
        let oldPositions = AtlasParcelLayout.fit(source.tiles, inside: parcel.insetBy(dx: 5, dy: 5))!
        let oldArea = oldPositions.reduce(0.0) { $0 + $1.width * $1.height }
        let display = AtlasParcelLayout.reflow([source.source.path: source])!
        let newArea = display.reduce(0.0) { $0 + $1.rect.width * $1.rect.height }
        precondition(newArea > oldArea * 1.10, "balanced source rows must materially eliminate fixed-page voids")
        precondition(source.tiles.map(\.sourceRange) == originalRanges && source.tiles.count == 3,
                     "presentation does not modify original archive pages")
        precondition(display.map(\.lineCount).max()! - display.map(\.lineCount).min()! <= 1,
                     "columns differ by at most one source row")
        let replayed = display.flatMap(\.lines)
        precondition(replayed.count == originalLines.count)
        for (old, new) in zip(originalLines, replayed) {
            precondition(old === new, "reflow reuses every exact shaped line once in source order")
        }
        var cursor = 0
        for (index, tile) in display.enumerated() {
            precondition(tile.sourceRange.location == cursor)
            cursor += tile.sourceRange.length
            precondition(contained(tile.rect, in: parcel))
            precondition(close(tile.rect.height / tile.height, tile.contentScale), "source proportions remain uniform")
            precondition(tile.parcelFirst == (index == 0) && tile.parcelRect == parcel)
        }
        precondition(cursor == source.source.text.utf16.count)
        let again = AtlasParcelLayout.reflow([source.source.path: source])!
        for (old, new) in zip(display, again) { precondition(old === new, "stable refresh preserves display image ownership") }
        let preparedTiles = source.tiles.map { tile -> AtlasTextTile in
            let result = AtlasTextTile(path: tile.path,
                preparedLines: tile.lines.map { AtlasPreparedLine($0, source: source.source.text)! },
                header: AtlasPreparedLine(tile.header!)!, sourceRange: tile.sourceRange)
            result.parcelRect = parcel
            return result
        }
        let restored = AtlasDocument(source: source.source, capture: source.capture, tiles: preparedTiles)
        let warm = AtlasParcelLayout.reflow([source.source.path: restored])!
        precondition(warm.map(\.sourceRange) == display.map(\.sourceRange) && warm.map(\.rect) == display.map(\.rect),
                     "cold and restored source rows produce identical presentation geometry")
        let rows = warm.flatMap { $0.preparedLines! }
        precondition(rows.map(\.sourceRange) == preparedTiles.flatMap { $0.preparedLines! }.map(\.sourceRange),
                     "prepared source rows remain complete and ordered")
    }
}
