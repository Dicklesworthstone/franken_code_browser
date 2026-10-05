import Foundation
import CoreGraphics
import CoreText
import CryptoKit

/// Capture-verified row geometry. These are row bands, not substring glyph bounds:
/// archived glyph runs intentionally do not carry character-to-glyph mappings.
struct AtlasMatch {
    let sourceRange: NSRange
    let rowRects: [CGRect]
    let focusRect: CGRect

    /// Hash and map a captured file once for all occurrences. Result order and
    /// per-occurrence refusal are identical to resolving each range separately.
    static func resolveBatch(document: AtlasDocument, ranges: [(UInt64, UInt64)],
                             expectedSHA256: String, expectedByteCount: UInt64) -> [AtlasMatch?] {
        guard ranges.count <= 4096,
              document.capture.text.utf8.elementsEqual(document.source.text.utf8),
              verifies(source: document.source, expectedSHA256: expectedSHA256,
                       expectedByteCount: expectedByteCount)
        else { return ranges.map { _ in nil } }
        let length = document.source.text.utf16.count
        return document.source.utf16Ranges(byteRanges: ranges).map { range in
            guard let range else { return nil }
            return geometry(document: document, match: range, sourceLength: length)
        }
    }

    /// An exact native-reader target does not require atlas geometry. A file can
    /// be absent from the eager atlas, or a match can span more overlay rows than
    /// we admit, without invalidating its independently verified source offsets.
    /// AppKit still decides whether it can select the exact UTF-16 range.
    static func resolveSourceRange(source: AtlasSource, byteStart: UInt64, byteEnd: UInt64,
                                   expectedSHA256: String, expectedByteCount: UInt64) -> NSRange? {
        guard byteStart < byteEnd, byteEnd <= expectedByteCount,
              verifies(source: source, expectedSHA256: expectedSHA256,
                       expectedByteCount: expectedByteCount) else { return nil }
        return source.utf16Range(byteStart: byteStart, byteEnd: byteEnd)
    }

    static func resolve(document: AtlasDocument, byteStart: UInt64, byteEnd: UInt64,
                        expectedSHA256: String, expectedByteCount: UInt64) -> AtlasMatch? {
        guard document.capture.text.utf8.elementsEqual(document.source.text.utf8),
              let range = resolveSourceRange(source: document.source, byteStart: byteStart,
                  byteEnd: byteEnd, expectedSHA256: expectedSHA256,
                  expectedByteCount: expectedByteCount) else { return nil }
        return geometry(document: document, match: range, sourceLength: document.source.text.utf16.count)
    }

    private static func verifies(source: AtlasSource, expectedSHA256: String,
                                 expectedByteCount: UInt64) -> Bool {
        let bytes = source.text.utf8
        guard expectedByteCount == UInt64(bytes.count),
              expectedSHA256.utf8.count == 64,
              expectedSHA256.utf8.allSatisfy({ (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0) })
        else { return false }
        let digest = SHA256.hash(data: Data(bytes)).map { String(format: "%02x", $0) }.joined()
        return digest == expectedSHA256.lowercased()
    }

    private static func geometry(document: AtlasDocument, match: NSRange, sourceLength: Int) -> AtlasMatch? {
        let start = match.location, end = match.location + match.length
        var bands: [CGRect] = []
        var focus: CGRect?
        var coveredUntil = start
        for tile in document.renderTiles {
            let tileRange = tile.sourceRange
            guard tileRange.location >= 0, tileRange.length >= 0,
                  tileRange.location <= sourceLength,
                  tileRange.length <= sourceLength - tileRange.location else { return nil }
            guard NSIntersectionRange(tileRange, match).length > 0 else { continue }
            let scale = tile.contentScale
            guard scale.isFinite, scale > 0,
                  [tile.rect.minX, tile.rect.minY, tile.rect.maxX, tile.rect.maxY].allSatisfy(\.isFinite),
                  tile.rect.height > 0 else { return nil }
            for row in 0..<tile.lineCount {
                let range: NSRange
                if let prepared = tile.preparedLines { range = prepared[row].sourceRange }
                else {
                    let raw = CTLineGetStringRange(tile.lines[row])
                    range = NSRange(location: raw.location, length: raw.length)
                }
                guard range.location >= tileRange.location, range.length >= 0,
                      range.location <= tileRange.location + tileRange.length,
                      range.length <= tileRange.location + tileRange.length - range.location else { return nil }
                let overlap = NSIntersectionRange(range, match)
                guard overlap.length > 0 else { continue }
                guard overlap.location == coveredUntil else { return nil }
                coveredUntil += overlap.length
                guard bands.count < 256 else { return nil }
                let rect = CGRect(x: tile.rect.minX + 4 * scale,
                    y: tile.rect.minY + (22 + Double(row) * AtlasTextTile.lineHeight) * scale,
                    width: (AtlasTextTile.width - 8) * scale, height: AtlasTextTile.lineHeight * scale)
                let clipped = rect.intersection(tile.rect)
                guard !clipped.isNull, !clipped.isEmpty else { return nil }
                bands.append(clipped)
                if focus == nil {
                    focus = clipped.insetBy(dx: -4 * scale, dy: -3 * AtlasTextTile.lineHeight * scale).intersection(tile.rect)
                }
            }
        }
        guard let focus, !bands.isEmpty, coveredUntil == end else { return nil }
        return AtlasMatch(sourceRange: match, rowRects: bands, focusRect: focus)
    }
}
