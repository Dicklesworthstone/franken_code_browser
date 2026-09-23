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
        guard ranges.count <= 4096 else { return ranges.map { _ in nil } }
        let text = document.source.text
        let bytes = text.utf8
        guard expectedByteCount == UInt64(bytes.count),
              expectedSHA256.utf8.count == 64,
              expectedSHA256.utf8.allSatisfy({ (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0) }),
              document.capture.text.utf8.elementsEqual(bytes),
              SHA256.hash(data: Data(bytes)).map({ String(format: "%02x", $0) }).joined() == expectedSHA256.lowercased()
        else { return ranges.map { _ in nil } }
        let endpoints = Set(ranges.flatMap { [$0.0, $0.1] }).sorted()
        var positions: [UInt64: Int] = [:]
        var next = 0, utf16 = 0
        var offset: UInt64 = 0
        for byte in bytes {
            while next < endpoints.count && endpoints[next] < offset { next += 1 }
            if next == endpoints.count { break }
            if endpoints[next] == offset {
                if byte & 0xC0 != 0x80 { positions[offset] = utf16 }
                next += 1
            }
            if byte & 0xC0 != 0x80 { utf16 += byte >= 0xF0 ? 2 : 1 }
            offset += 1
        }
        if offset == expectedByteCount { positions[offset] = utf16 }
        let length = text.utf16.count
        return ranges.map { a, b in
            guard a < b, b <= expectedByteCount, let start = positions[a], let end = positions[b], start < end else { return nil }
            return geometry(document: document, match: NSRange(location: start, length: end - start), sourceLength: length)
        }
    }

    static func resolve(document: AtlasDocument, byteStart: UInt64, byteEnd: UInt64,
                        expectedSHA256: String, expectedByteCount: UInt64) -> AtlasMatch? {
        let text = document.source.text
        let bytes = text.utf8
        guard byteStart < byteEnd, byteEnd <= expectedByteCount,
              expectedByteCount == UInt64(bytes.count),
              expectedSHA256.utf8.count == 64,
              expectedSHA256.utf8.allSatisfy({ (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0) }),
              document.capture.text.utf8.elementsEqual(bytes) else { return nil }
        let digest = SHA256.hash(data: Data(bytes)).map { String(format: "%02x", $0) }.joined()
        guard digest == expectedSHA256.lowercased() else { return nil }

        // Count UTF16 units directly, accepting scalar boundaries (including a
        // boundary within a combining sequence), without allocating two prefixes.
        var offset: UInt64 = 0, utf16 = 0
        var start: Int?, end: Int?
        for byte in bytes {
            let continuation = byte & 0xC0 == 0x80
            if offset == byteStart {
                guard !continuation else { return nil }
                start = utf16
            }
            if offset == byteEnd {
                guard !continuation else { return nil }
                end = utf16
                break
            }
            if !continuation { utf16 += byte >= 0xF0 ? 2 : 1 }
            offset += 1
        }
        if byteEnd == expectedByteCount { end = utf16 }
        guard let start, let end, start < end else { return nil }
        let match = NSRange(location: start, length: end - start)
        return geometry(document: document, match: match, sourceLength: text.utf16.count)
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
