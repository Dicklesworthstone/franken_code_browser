import SwiftUI
import AppKit
import CoreText
import os

enum Monokai {
    static let background = NSColor(srgbRed: 0x16 / 255.0, green: 0x1A / 255.0, blue: 0x1D / 255.0, alpha: 1)
    static func color(_ role: String) -> NSColor {
        let value: UInt32
        switch role {
        case "keyword", "operator": value = 0xF92672
        case "type": value = 0x66D9EF
        case "func", "function": value = 0xA6E22E
        case "str", "string": value = 0xE6DB74
        case "number": value = 0xAE81FF
        case "comment": value = 0x75715E
        default: value = 0xF8F8F2
        }
        return NSColor(srgbRed: Double((value >> 16) & 255) / 255,
                       green: Double((value >> 8) & 255) / 255,
                       blue: Double(value & 255) / 255, alpha: 1)
    }
}

struct AtlasHighlightCapture: Decodable {
    struct Run: Decodable { let start: String; let length: String; let role: String }
    let schema: String
    let text: String
    let runs: [Run]

    func validatedRuns() -> [(NSRange, String)]? {
        guard schema == "fcb.source-document/1" else { return nil }
        let length = text.utf16.count
        var cursor = 0
        var result: [(NSRange, String)] = []
        for run in runs {
            guard let start = Int(run.start), let count = Int(run.length),
                  start == cursor, count > 0, count <= length - cursor else { return nil }
            result.append((NSRange(location: start, length: count), run.role))
            cursor += count
        }
        return cursor == length ? result : nil
    }
}

/// Cached platform-shaped lines. Wheel/drag never tokenize or shape source.
final class AtlasTextTile {
    private static let renderMutationClock = OSAllocatedUnfairLock(initialState: UInt64(0))
    static var renderMutationEpoch: UInt64 { renderMutationClock.withLock { $0 } }
    private func noteRenderMutation() { Self.renderMutationClock.withLock { $0 &+= 1 } }
    static let width = 648.0
    static let lineHeight = 16.0
    let path: String
    let lines: [CTLine]
    let header: CTLine?
    var preparedLines: [AtlasPreparedLine]? { didSet { noteRenderMutation() } }
    var preparedHeader: AtlasPreparedLine? { didSet { noteRenderMutation() } }
    let sourceRange: NSRange
    var rect = CGRect.zero { didSet { noteRenderMutation() } }
    var parcelRect: CGRect? { didSet { noteRenderMutation() } }
    var parcelFirst = false { didSet { noteRenderMutation() } }
    var contentScale: Double { rect.width / Self.width }
    var raster: CGImage? { didSet { noteRenderMutation() } }
    var lineCount: Int { preparedLines?.count ?? lines.count }
    var height: Double { 22 + Double(lineCount) * Self.lineHeight }

    init(path: String, lines: [CTLine], sourceRange: NSRange, part: Int, retainedHeader: CTLine? = nil) {
        self.path = path
        self.lines = lines
        self.sourceRange = sourceRange
        let title = (path as NSString).lastPathComponent + (part > 0 ? " · \(part + 1)" : "")
        header = retainedHeader ?? Self.makeHeader(title)
    }

    static func makeHeader(_ title: String) -> CTLine {
        CTLineCreateWithAttributedString(NSAttributedString(string: title, attributes: [
            .font: NSFont.monospacedSystemFont(ofSize: 10, weight: .medium),
            .foregroundColor: Monokai.color("comment")
        ]))
    }

    init(path: String, preparedLines: [AtlasPreparedLine], header: AtlasPreparedLine, sourceRange: NSRange) {
        self.path = path
        self.lines = []
        self.header = nil
        self.preparedLines = preparedLines
        self.preparedHeader = header
        self.sourceRange = sourceRange
    }

    /// Both cached overview and close-up use these exact same colored glyphs.
    func draw(in context: CGContext, visibleRows: Range<Int>? = nil) {
        context.textMatrix = CGAffineTransform(scaleX: 1, y: -1)
        context.textPosition = CGPoint(x: 4, y: 12)
        if let header { CTLineDraw(header, context) }
        else { preparedHeader?.draw(in: context, origin: CGPoint(x: 4, y: 12)) }
        for index in visibleRows ?? (0..<lineCount) {
            context.textPosition = CGPoint(x: 4, y: 34 + Double(index) * Self.lineHeight)
            if let preparedLines {
                preparedLines[index].draw(in: context, origin: context.textPosition)
            } else { CTLineDraw(lines[index], context) }
        }
    }

    func prepareRaster(pixelBudget: Int) {
        let budget = max(1, pixelBudget)
        let scale = min(1, sqrt(Double(budget) / (Self.width * height)))
        let width = max(1, min(budget, Int(Self.width * scale)))
        let height = max(1, min(budget / width, Int(self.height * scale)))
        guard let bitmap = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
            bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return }
        bitmap.translateBy(x: 0, y: Double(height))
        bitmap.scaleBy(x: Double(width) / Self.width, y: -Double(height) / self.height)
        draw(in: bitmap)
        raster = bitmap.makeImage()
    }
}

final class AtlasDocument {
    let source: AtlasSource
    let capture: AtlasHighlightCapture
    let tiles: [AtlasTextTile]
    // Original archive pages never change. Presentation may repartition their
    // already-shaped rows into balanced columns for the current parcel.
    var displayTiles: [AtlasTextTile]?
    var renderTiles: [AtlasTextTile] { displayTiles ?? tiles }
    let restoredColorLineCount: Int
    lazy var preparationCounts: (glyphs: Int, runs: Int) = {
        var glyphs = 0, runs = 0
        for tile in tiles {
            if let lines = tile.preparedLines {
                for line in lines + [tile.preparedHeader].compactMap({ $0 }) {
                    runs += line.runs.count
                    glyphs += line.runs.reduce(0) { $0 + $1.glyphs.count }
                    if let retained = line.retainedColorLine {
                        glyphs += CTLineGetGlyphCount(retained)
                        runs += CFArrayGetCount(CTLineGetGlyphRuns(retained))
                    }
                }
            } else {
                for line in tile.lines + [tile.header].compactMap({ $0 }) {
                    glyphs += CTLineGetGlyphCount(line)
                    runs += CFArrayGetCount(CTLineGetGlyphRuns(line))
                }
            }
        }
        return (glyphs, runs)
    }()

    // Reader presentation is needed only for selection; opening the atlas does
    // not construct a second attributed copy of every source file.
    lazy var styledSource: AttributedString = AttributedString(AtlasDocument.style(capture))

    static func style(_ capture: AtlasHighlightCapture) -> NSMutableAttributedString {
        let paragraph = NSMutableParagraphStyle()
        paragraph.defaultTabInterval = 8 * 7.83
        let styled = NSMutableAttributedString(string: capture.text, attributes: [
            .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular),
            .paragraphStyle: paragraph,
            .foregroundColor: Monokai.color("plain")
        ])
        for (range, role) in capture.validatedRuns() ?? [] {
            styled.addAttribute(.foregroundColor, value: Monokai.color(role), range: range)
        }
        return styled
    }

    init(source: AtlasSource, capture: AtlasHighlightCapture, tiles: [AtlasTextTile], restoredColorLineCount: Int = 0) {
        self.restoredColorLineCount = restoredColorLineCount
        self.source = source
        self.capture = capture
        self.tiles = tiles
    }

    init?(path: String, capture: AtlasHighlightCapture, glyphLimit: Int = 16 * 1024 * 1024,
          runLimit: Int = 1024 * 1024) {
        guard glyphLimit >= 0, runLimit >= 0, capture.text.utf16.count <= glyphLimit,
              capture.validatedRuns() != nil else { return nil }
        self.capture = capture
        restoredColorLineCount = 0
        source = AtlasSource(path: path, text: capture.text)
        let styled = Self.style(capture)
        let typesetter = CTTypesetterCreateWithAttributedString(styled)
        var result: [AtlasTextTile] = []
        var lines: [CTLine] = []
        var offset = 0
        var start = 0
        var remainingGlyphs = glyphLimit, remainingRuns = runLimit
        while offset < styled.length {
            let count = CTTypesetterSuggestLineBreak(typesetter, offset, AtlasTextTile.width - 8)
            guard count > 0, count <= styled.length - offset else { return nil }
            let line = CTTypesetterCreateLine(typesetter, CFRange(location: offset, length: count))
            let glyphs = CTLineGetGlyphCount(line), runs = CFArrayGetCount(CTLineGetGlyphRuns(line))
            guard glyphs <= remainingGlyphs, runs <= remainingRuns else { return nil }
            remainingGlyphs -= glyphs; remainingRuns -= runs
            lines.append(line)
            offset += count
            if lines.count == 80 || offset == styled.length {
                let tile = AtlasTextTile(path: path, lines: lines,
                    sourceRange: NSRange(location: start, length: offset - start), part: result.count)
                let headerGlyphs = tile.header.map { CTLineGetGlyphCount($0) } ?? 0
                let headerRuns = tile.header.map { CFArrayGetCount(CTLineGetGlyphRuns($0)) } ?? 0
                guard headerGlyphs <= remainingGlyphs, headerRuns <= remainingRuns else { return nil }
                remainingGlyphs -= headerGlyphs; remainingRuns -= headerRuns
                result.append(tile)
                lines = []
                start = offset
            }
        }
        tiles = result
    }
}
