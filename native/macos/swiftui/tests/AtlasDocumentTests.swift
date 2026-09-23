import Foundation
import CoreGraphics

@main struct AtlasDocumentTests {
    static func capture(_ text: String, role: String = "plain") -> AtlasHighlightCapture {
        AtlasHighlightCapture(schema: "fcb.source-document/1", text: text,
            runs: text.isEmpty ? [] : [.init(start: "0", length: String(text.utf16.count), role: role)])
    }
    static func pixels(_ text: String, size: CGSize = CGSize(width: 300, height: 600), budget: Int = 32768,
                       role: String = "plain") -> [UInt8] {
        guard let document = AtlasDocument(path: "unselected.rs", capture: capture(text, role: role)) else {
            preconditionFailure("valid captured source must shape")
        }
        var result: [UInt8] = []
        for tile in document.tiles {
            tile.prepareRaster(pixelBudget: max(1, budget / max(1, document.tiles.count)))
            guard let raster = tile.raster, let data = raster.dataProvider?.data else {
                preconditionFailure("nonempty source must produce actual glyph raster")
            }
            precondition(raster.width * raster.height <= max(1, budget / document.tiles.count),
                         "overview raster has a hard pixel budget")
            result.append(contentsOf: UnsafeBufferPointer(start: CFDataGetBytePtr(data), count: CFDataGetLength(data)))
        }
        return result
    }
    static func main() {
        let source = "fn visible_without_selection() {\n    let value = 123;\n}\n"
        let glyphs = pixels(source)
        let blank = pixels("\n\n\n")
        precondition(glyphs != blank, "overview contains source glyphs, not a blank tile")
        precondition(glyphs.contains { $0 != 0 }, "visible ink exists")
        precondition(glyphs.contains(0), "glyph raster preserves whitespace, not a solid fill")
        precondition(glyphs != pixels(source.replacingOccurrences(of: "123", with: "987")),
                     "changing real source changes overview pixels")
        let late = String(repeating: "\n", count: 699)
        precondition(pixels(late + "last source line") != pixels(late + "\n"),
                     "source beyond the old 600-line preview participates in overview")
        let wide = CGSize(width: 3000, height: 300)
        let prefix = String(repeating: "x", count: 99)
        precondition(pixels(prefix + "A", size: wide, budget: 8 * 1024 * 1024)
            != pixels(prefix + "B", size: wide, budget: 8 * 1024 * 1024),
            "dimension caps preserve aspect and the far end of a wide source line")
        let empty = AtlasDocument(path: "empty", capture: capture(""))!
        precondition(empty.tiles.isEmpty, "empty source has no fabricated glyphs")
        precondition(pixels(source, role: "keyword") != pixels(source, role: "string"),
                     "Monokai roles alter actual rasterized glyph color")
        let longText = String(repeating: "let value = 123; ", count: 3000)
        let wrapped = AtlasDocument(path: "long.rs", capture: capture(longText))!
        precondition(wrapped.tiles.count > 1, "long source wraps into dense bounded-height columns")
        precondition(wrapped.tiles.allSatisfy { $0.lines.count <= 80 }, "no enormous file-sized holes")
        var end = 0
        for tile in wrapped.tiles {
            precondition(tile.sourceRange.location == end, "wrapped tiles preserve contiguous source")
            end += tile.sourceRange.length
        }
        precondition(end == longText.utf16.count, "no source suffix lost on wrapping")
        let bad = AtlasHighlightCapture(schema: "fcb.source-document/1", text: "abc",
            runs: [.init(start: "1", length: "2", role: "keyword")])
        precondition(AtlasDocument(path: "bad", capture: bad) == nil, "invalid highlight ranges refuse")
        print("AtlasDocument: actual glyph, whitespace, late-line and bounded raster checks passed")
    }
}
