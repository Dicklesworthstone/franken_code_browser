import Foundation
import CoreGraphics
import CoreText
import CryptoKit

@main struct AtlasMatchTests {
    static func hash(_ text: String) -> String {
        SHA256.hash(data: Data(text.utf8)).map { String(format: "%02x", $0) }.joined()
    }
    static func document(_ text: String) -> AtlasDocument {
        let document = AtlasDocument(path: "src/large.rs", capture: AtlasHighlightCapture(
            schema: "fcb.source-document/1", text: text,
            runs: [.init(start: "0", length: String(text.utf16.count), role: "plain")]))!
        for (index, tile) in document.tiles.enumerated() {
            tile.rect = CGRect(x: Double(index) * 1500 + 100, y: 250,
                               width: AtlasTextTile.width * 2, height: tile.height * 2)
        }
        return document
    }
    static func resolve(_ document: AtlasDocument, _ needle: String) -> AtlasMatch? {
        let text = document.source.text
        // Search raw bytes so the test can deliberately address a combining
        // scalar inside one grapheme; String.range(of:) respects graphemes.
        let bytes = Array(text.utf8), needleBytes = Array(needle.utf8)
        precondition(!needleBytes.isEmpty && needleBytes.count <= bytes.count)
        let offset = (0...(bytes.count - needleBytes.count)).first {
            bytes[$0..<($0 + needleBytes.count)].elementsEqual(needleBytes)
        }!
        let start = UInt64(offset)
        return AtlasMatch.resolve(document: document, byteStart: start,
            byteEnd: start + UInt64(needle.utf8.count), expectedSHA256: hash(text), expectedByteCount: UInt64(text.utf8.count))
    }
    static func main() {
        let prefix = (0..<175).map { "let value\($0) = \($0);\n" }.joined()
        let text = prefix + "日本語 😀 cafe\u{301} שלום target\nnext line\n"
        let cold = document(text)
        let result = resolve(cold, "שלום target")!
        precondition(result.sourceRange == (text as NSString).range(of: "שלום target"))
        precondition(result.rowRects.count == 1)
        let lastTile = cold.tiles.last!
        precondition(lastTile.rect.contains(result.rowRects[0]))
        precondition(result.rowRects[0].minX == lastTile.rect.minX + 8)
        precondition(result.rowRects[0].height == 32)
        precondition(result.focusRect.contains(result.rowRects[0]))
        precondition(result.focusRect.height <= 7 * 32)
        let relocated = AtlasTextTile(path: lastTile.path, lines: lastTile.lines,
            sourceRange: lastTile.sourceRange, part: 0, retainedHeader: lastTile.header)
        relocated.rect = lastTile.rect.offsetBy(dx: 7000, dy: 3000)
        cold.displayTiles = [relocated]
        let displayMatch = resolve(cold, "שלום target")!
        precondition(displayMatch.sourceRange == result.sourceRange &&
                     displayMatch.rowRects[0] == result.rowRects[0].offsetBy(dx: 7000, dy: 3000),
                     "match navigation follows presentation rows rather than immutable archive pages")
        cold.displayTiles = nil
        precondition(resolve(cold, "😀")!.sourceRange.length == 2)
        precondition(resolve(cold, "\u{301}")!.sourceRange.length == 1)
        precondition(resolve(cold, "target\nnext")!.rowRects.count == 2)
        precondition(resolve(cold, "line\n") != nil, "EOF is a valid scalar boundary")

        // Exercise the archive-style replay representation made from real shaped
        // lines, without asking the resolver to construct any CoreText lines.
        let warmTiles = cold.tiles.map { tile -> AtlasTextTile in
            let prepared = tile.lines.map { AtlasPreparedLine($0, source: text)! }
            let warmed = AtlasTextTile(path: tile.path, preparedLines: prepared,
                header: AtlasPreparedLine(tile.header!)!, sourceRange: tile.sourceRange)
            warmed.rect = tile.rect
            return warmed
        }
        let warm = AtlasDocument(source: cold.source, capture: cold.capture, tiles: warmTiles)
        let restored = resolve(warm, "שלום target")!
        let batchWarm = AtlasMatch.resolveBatch(document: warm,
            ranges: [(UInt64(prefix.utf8.count), UInt64(prefix.utf8.count + "日本語".utf8.count))],
            expectedSHA256: hash(text), expectedByteCount: UInt64(text.utf8.count))
        precondition(batchWarm[0]?.rowRects == resolve(warm, "日本語")?.rowRects)
        precondition(restored.sourceRange == result.sourceRange && restored.rowRects == result.rowRects && restored.focusRect == result.focusRect)

        let start = UInt64(prefix.utf8.count)
        func rejected(_ a: UInt64, _ b: UInt64, _ digest: String? = nil, _ count: UInt64? = nil) {
            precondition(AtlasMatch.resolve(document: cold, byteStart: a, byteEnd: b,
                expectedSHA256: digest ?? hash(text), expectedByteCount: count ?? UInt64(text.utf8.count)) == nil)
        }
        rejected(start + 1, start + 3) // inside Japanese scalar
        rejected(start, start + 1)
        rejected(0, 0)
        rejected(2, 1)
        rejected(0, UInt64.max)
        rejected(0, 1, String(repeating: "z", count: 64))
        rejected(0, 1, hash(text), UInt64.max)
        let stale = text.replacingOccurrences(of: "value0", with: "valueX")
        rejected(start, start + 3, hash(stale), UInt64(stale.utf8.count))
        cold.tiles.last!.rect = .zero
        precondition(resolve(cold, "שלום target") == nil)

        let composed = document("é\n")
        let decomposed = AtlasDocument(source: AtlasSource(path: "src/large.rs", text: "e\u{301}\n"),
            capture: composed.capture, tiles: composed.tiles)
        precondition(AtlasMatch.resolve(document: decomposed, byteStart: 0, byteEnd: 3,
            expectedSHA256: hash(decomposed.source.text), expectedByteCount: UInt64(decomposed.source.text.utf8.count)) == nil,
            "canonically equivalent text is not an identical searched capture")
        let long = document(String(repeating: "x\n", count: 300))
        precondition(resolve(long, long.source.text) == nil, "oversized match refuses instead of silently truncating highlights")
        // Compare batched results with the established scalar resolver for all
        // byte boundaries, including invalid UTF8 interiors and overlapping hits.
        let batchDoc = document("a😀e\u{301}日本語\nlast\n")
        let batchText = batchDoc.source.text
        let n = UInt64(batchText.utf8.count)
        let ranges = (0...n).flatMap { a in (a...n).map { (a, $0) } } + [(UInt64.max, UInt64.max)]
        let batch = AtlasMatch.resolveBatch(document: batchDoc, ranges: ranges,
            expectedSHA256: hash(batchText), expectedByteCount: n)
        for ((a,b), actual) in zip(ranges, batch) {
            let expected = AtlasMatch.resolve(document: batchDoc, byteStart: a, byteEnd: b,
                expectedSHA256: hash(batchText), expectedByteCount: n)
            precondition(actual?.sourceRange == expected?.sourceRange && actual?.rowRects == expected?.rowRects && actual?.focusRect == expected?.focusRect)
        }
        precondition(AtlasMatch.resolveBatch(document: batchDoc, ranges: [(0,1)],
            expectedSHA256: hash("stale"), expectedByteCount: n)[0] == nil)
        precondition(AtlasMatch.resolveBatch(document: batchDoc, ranges: [],
            expectedSHA256: hash(batchText), expectedByteCount: n).isEmpty)
        print("AtlasMatch: late continuation, Unicode scalar/UTF16 mapping, RTL, cold/prepared parity, stale capture and malformed range refusals passed")
    }
}
