import Foundation
import CoreGraphics
import AppKit

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
        sharedReaderStylesPreserveExactText()
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

    static func sharedReaderStylesPreserveExactText() {
        let chunks: [(String, String)] = [
            ("\u{feff}/*😀 e\u{301}\r\n*/", "comment"), ("\t", "plain"),
            ("fn", "keyword"), (" ", "plain"), ("call", "function"), ("(", "punctuation"),
            ("Foo", "type"), (")", "punctuation"), ("=", "operator"),
            ("42", "number"), ("\"s\"", "string"), ("\0\r", "plain")
        ]
        let text = chunks.map { $0.0 }.joined()
        var cursor = 0
        let runs = chunks.map { chunk -> (NSRange, String) in
            defer { cursor += chunk.0.utf16.count }
            return (NSRange(location: cursor, length: chunk.0.utf16.count), chunk.1)
        }
        let full = AtlasHighlightCapture(schema: "fcb.source-document/1", text: text,
            runs: runs.map { .init(start: String($0.0.location), length: String($0.0.length), role: $0.1) })
        let wholeStyle = AtlasDocument.style(full)
        let pageStyle = AtlasDocument.style(text: text, runs: runs)
        precondition(pageStyle.string.utf8.elementsEqual(text.utf8), "page styling changed source bytes")
        precondition(pageStyle.isEqual(to: wholeStyle), "whole-source and page readers disagree on shared styling")
        for (range, role) in runs {
            precondition(pageStyle.attribute(.foregroundColor, at: range.location, effectiveRange: nil) as? NSColor
                == Monokai.color(role), "page token did not use the shared Monokai role")
        }
        let plain = AtlasDocument.style(text: text, runs: [])
        precondition(plain.string.utf8.elementsEqual(text.utf8), "plain fallback changed source bytes")
        precondition(plain.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor == Monokai.color("plain"),
                     "plain fallback did not retain the reader palette")
        let paragraph = pageStyle.attribute(.paragraphStyle, at: 0, effectiveRange: nil) as? NSParagraphStyle
        precondition(paragraph?.defaultTabInterval == 8 * 7.83, "page tabs diverged from whole-source reader")
        precondition(AtlasDocument.style(text: "", runs: []).length == 0, "empty page acquired decoration")
    }
}
