import Foundation
import AppKit
import CoreText
import CryptoKit

/// Admission and replay of the upstream styled display inside metadata parcels.
/// The original source remains the document's authority; this adapter neither
/// parses Markdown nor wraps, reshapes or infers source offsets from glyphs.
enum AtlasParcelDocumentDisplay {
    static let width: UInt64 = 640
    static let sourceBytes = 64 * 1024
    static let maximumItems = 8192
    static let maximumGlyphs = 262_144
    static let maximumTiles = 64
    static let maximumPages = 128
    static let maximumTileItems = 512
    static let maximumTileHeight = 1280.0

    private struct FontKey: Hashable { let index: UInt64; let size: Double }
    private enum Content {
        case text(AtlasPreparedLine, origin: CGPoint, clip: CGRect?)
        case vector(AtlasTileDisplay.Vector)
        case anchor
    }
    private struct Element {
        let bounds: CGRect
        let sourceRange: NSRange?
        let content: Content
        let protectsBoundary: Bool
    }

    static func prepare(source: AtlasDocument, pages: [AtlasDocumentDisplayPage],
                        fonts: [AtlasDocumentDisplayFont], captureSHA256: String,
                        glyphLimit: Int, runLimit: Int, tileLimit: Int,
                        canceled: () -> Bool) -> AtlasDocument? {
        guard !canceled(), source.presentation == .source,
              source.capture.text.utf8.elementsEqual(source.source.text.utf8),
              source.source.text.utf8.count <= sourceBytes,
              glyphLimit > 0, runLimit > 0, tileLimit > 0,
              captureSHA256.utf8.count == 64,
              captureSHA256.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
              !pages.isEmpty, pages.count <= maximumPages, let first = pages.first else { return nil }
        let digest = SHA256.hash(data: Data(source.source.text.utf8))
            .map { String(format: "%02x", $0) }.joined()
        guard digest == captureSHA256 else { return nil }
        let inventory = first.inventory
        let bytes = UInt64(source.source.text.utf8.count)
        let sourceBase: UInt64 = source.source.text.utf8.starts(with: [0xef, 0xbb, 0xbf]) ? 3 : 0
        guard inventory.document.identity.encoding == "utf8",
              inventory.document.identity.capturedBytes == bytes,
              inventory.document.sourceBase == sourceBase,
              inventory.width == width, inventory.qualifiedSources,
              inventory.totalItems > 0, inventory.totalItems <= UInt64(maximumItems),
              inventory.bounds.valid, inventory.bounds.width <= Double(width),
              inventory.bounds.width > 0, inventory.bounds.height > 0,
              inventory.bounds.height <= maximumTileHeight * Double(maximumTiles),
              fonts.count <= AtlasDocumentDisplayLimits.fonts else { return nil }

        // Page order and every page's identity are checked before a single
        // display item is published. No prefix is accepted as a whole document.
        var items: [AtlasDocumentDisplayItem] = []
        items.reserveCapacity(Int(inventory.totalItems))
        var next: UInt64 = 0
        for (index, page) in pages.enumerated() {
            guard !canceled(), page.inventory.matches(inventory), page.first == next,
                  !page.items.isEmpty, page.items.count <= Int(AtlasDocumentDisplayLimits.pageItems),
                  page.items.count <= maximumItems - items.count else { return nil }
            do { try page.validateFonts(fonts) } catch { return nil }
            for item in page.items {
                guard item.id == next, item.bounds.valid else { return nil }
                next += 1
            }
            let expectedNext: UInt64? = next < inventory.totalItems ? next : nil
            guard page.next == expectedNext, (index == pages.count - 1) == (expectedNext == nil) else { return nil }
            items.append(contentsOf: page.items)
        }
        guard next == inventory.totalItems else { return nil }

        // Convert only engine-qualified original byte ranges. The conversion
        // scans the immutable UTF-8 source, never rendered text or font data.
        var originalRanges: [NSRange?] = []
        originalRanges.reserveCapacity(items.count)
        for start in stride(from: 0, to: items.count, by: 4096) {
            let end = min(start + 4096, items.count)
            let batch = items[start..<end].map { item -> (UInt64, UInt64) in
                guard let original = item.original, original.valid(upTo: bytes, nonempty: true),
                      original.start >= sourceBase else { return (0, 0) }
                return (original.start, original.end)
            }
            let converted = source.source.utf16Ranges(byteRanges: batch)
            for (offset, item) in items[start..<end].enumerated() {
                if item.original != nil && converted[offset] == nil { return nil }
            }
            originalRanges.append(contentsOf: converted)
        }

        var nativeFonts: [FontKey: CTFont] = [:]
        func font(_ index: UInt64, size: Double) -> CTFont? {
            guard size.isFinite, size > 0, size <= 256 else { return nil }
            let key = FontKey(index: index, size: size)
            if let found = nativeFonts[key] { return found }
            guard nativeFonts.count < 128, let supplied = fonts.first(where: { $0.index == index }) else { return nil }
            let value = CTFontCreateWithGraphicsFont(supplied.face, CGFloat(size), nil, nil)
            nativeFonts[key] = value
            return value
        }
        var elements: [Element] = []
        elements.reserveCapacity(items.count + min(items.count, 128))
        var glyphs = 0, runs = 0
        let allowedGlyphs = min(glyphLimit, maximumGlyphs)
        func admit(_ line: AtlasPreparedLine) -> Bool {
            let count = line.runs.reduce(0) { $0 + $1.glyphs.count }
            guard count <= allowedGlyphs - glyphs, line.runs.count <= runLimit - runs else { return false }
            glyphs += count; runs += line.runs.count
            return true
        }
        let full = CGRect(x: inventory.bounds.x, y: inventory.bounds.y,
            width: inventory.bounds.width, height: inventory.bounds.height)
        for (index, item) in items.enumerated() {
            guard !canceled() else { return nil }
            let box = CGRect(x: item.bounds.x, y: item.bounds.y,
                width: item.bounds.width, height: item.bounds.height)
            // An overflowing layout is a source fallback, never silent clipping
            // of a wide code fence, oversized image or malformed display list.
            guard box.minX >= full.minX - 0.001, box.maxX <= full.maxX + 0.001,
                  box.minY >= full.minY - 0.001, box.maxY <= full.maxY + 0.001 else { return nil }
            let sourceRange = item.readingText == nil ? nil : originalRanges[index]
            switch item.content {
            case .text(let text, let role, let size, let suppliedGlyphs):
                guard suppliedGlyphs.count <= AtlasDocumentDisplayLimits.runGlyphs else { return nil }
                if suppliedGlyphs.isEmpty { continue }
                guard suppliedGlyphs.allSatisfy({
                    $0.id > 0 && $0.x.isFinite && $0.y.isFinite
                        && abs($0.x) <= 1_000_000 && abs($0.y) <= 1_000_000
                }) else { return nil }
                guard let firstGlyph = suppliedGlyphs.first, let firstFont = font(firstGlyph.font, size: size) else { return nil }
                let ascent = CTFontGetAscent(firstFont), descent = CTFontGetDescent(firstFont)
                let origin = CGPoint(x: box.minX,
                    y: box.minY + (box.height - ascent - descent) / 2 + ascent)
                var shaped: [AtlasPreparedRun] = []
                var start = 0
                while start < suppliedGlyphs.count {
                    let fontIndex = suppliedGlyphs[start].font
                    var end = start + 1
                    while end < suppliedGlyphs.count && suppliedGlyphs[end].font == fontIndex { end += 1 }
                    guard let face = font(fontIndex, size: size) else { return nil }
                    let values = suppliedGlyphs[start..<end]
                    shaped.append(AtlasPreparedRun(font: face,
                        color: Monokai.color(AtlasTileDisplay.colorRole(role)).cgColor, matrix: .identity,
                        glyphs: values.map(\.id), positions: values.map { CGPoint(x: $0.x, y: $0.y) },
                        fallbackText: String(String.UnicodeScalarView(text.unicodeScalars.prefix(64)))))
                    start = end
                }
                let line = AtlasPreparedLine(runs: shaped)
                guard admit(line) else { return nil }
                elements.append(.init(bounds: box, sourceRange: sourceRange,
                    content: .text(line, origin: origin, clip: nil), protectsBoundary: true))
            case .vector(let shape, let role, let stroke):
                guard stroke.isFinite, stroke >= 0, stroke <= 256 else { return nil }
                elements.append(.init(bounds: box, sourceRange: nil,
                    content: .vector(.init(bounds: box, shape: shape, role: role, stroke: stroke)),
                    protectsBoundary: false))
            case .image(let alt):
                // Atlas enrichment has no asset-root authority. Preserve the
                // engine's slot and alt label; the retained reader can explicitly
                // acquire images through its existing grant-qualified route.
                let label = alt.isEmpty ? "Image unavailable" : "Image: " + String(alt.prefix(512))
                let native = CTLineCreateWithAttributedString(NSAttributedString(string: label, attributes: [
                    .font: NSFont.systemFont(ofSize: 12), .foregroundColor: Monokai.color("comment")
                ]))
                guard var line = AtlasPreparedLine(native, source: label), admit(line) else { return nil }
                if line.hasColorGlyphs { line.retainedColorLine = native }
                elements.append(.init(bounds: box, sourceRange: sourceRange,
                    content: .vector(.init(bounds: box, shape: "image-placeholder", role: "muted", stroke: 1)),
                    protectsBoundary: true))
                elements.append(.init(bounds: box, sourceRange: nil,
                    content: .text(line, origin: CGPoint(x: box.minX + 6, y: box.midY + 4),
                        clip: box.insetBy(dx: min(6, box.width / 2), dy: min(4, box.height / 2))),
                    protectsBoundary: true))
            case .anchor:
                // Anchor identities remain in the retained reader. They have no
                // ink to duplicate and never become made-up source glyph rows.
                continue
            }
        }
        guard !elements.isEmpty, elements.count <= maximumItems * 2 else { return nil }
        let title = (source.source.path as NSString).lastPathComponent + " · Markdown"
        let nativeHeader = AtlasTextTile.makeHeader(title)
        guard var header = AtlasPreparedLine(nativeHeader, source: title) else { return nil }
        if header.hasColorGlyphs { header.retainedColorLine = nativeHeader }
        guard admit(header) else { return nil }

        // Merge intersecting protected boxes once. Adjacent line-box edges
        // remain valid cut positions; dense overlapping boxes cannot turn each
        // window into an unbounded repeated scan.
        var protected: [(start: Double, end: Double)] = []
        for box in elements.filter(\.protectsBoundary).map(\.bounds).sorted(by: { $0.minY < $1.minY }) {
            if let last = protected.last, box.minY < last.end {
                protected[protected.count - 1].end = max(last.end, box.maxY)
            } else { protected.append((box.minY, box.maxY)) }
        }
        var tiles: [AtlasTextTile] = []
        var top = full.minY, replayItems = 0
        while top < full.maxY {
            guard !canceled(), tiles.count < min(tileLimit, maximumTiles) else { return nil }
            var bottom = min(top + maximumTileHeight, full.maxY)
            // Cut a retained vertical window only between shaped line/image
            // boxes. Long vector borders may continue into the following tile.
            // This moves a clip boundary, never glyphs or upstream block layout.
            if let crossing = protected.first(where: { $0.start < bottom && $0.end > bottom }) {
                guard crossing.start > top else { return nil }
                bottom = crossing.start
            }
            guard bottom > top else { return nil }
            let viewport = CGRect(x: full.minX, y: top, width: Double(width), height: bottom - top)
            let localOffset = CGPoint(x: 4 - full.minX, y: 22 - top)
            var lines: [AtlasPreparedLine] = [], rows: [AtlasTileDisplay.Row] = []
            var drawings: [AtlasTileDisplay.Drawing] = [], regions: [AtlasTileSourceRegion] = []
            for element in elements {
                // Rules and quote accents may have a zero-width/height
                // geometric box but still draw a nonzero stroke.
                let coverage = element.bounds.isEmpty ? element.bounds.insetBy(dx: -1, dy: -1) : element.bounds
                guard coverage.intersects(viewport) else { continue }
                guard drawings.count < maximumTileItems, replayItems < maximumItems * 2 else { return nil }
                let bounds = element.bounds.offsetBy(dx: localOffset.x, dy: localOffset.y)
                switch element.content {
                case .text(let line, let origin, let clip):
                    drawings.append(.text(lines.count))
                    lines.append(line)
                    rows.append(.init(origin: CGPoint(x: origin.x + localOffset.x, y: origin.y + localOffset.y),
                        bounds: bounds, clip: clip?.offsetBy(dx: localOffset.x, dy: localOffset.y)))
                case .vector(let vector):
                    drawings.append(.vector(.init(bounds: bounds, shape: vector.shape,
                        role: vector.role, stroke: vector.stroke)))
                case .anchor: continue
                }
                replayItems += 1
                if let sourceRange = element.sourceRange {
                    let rect = element.bounds.intersection(viewport).offsetBy(dx: localOffset.x, dy: localOffset.y)
                    if !rect.isEmpty && !rect.isNull { regions.append(.init(sourceRange: sourceRange, rect: rect)) }
                }
            }
            let display = AtlasTileDisplay(height: 22 + bottom - top, rows: rows,
                drawings: drawings, sourceRegions: regions)
            let range: NSRange
            if let start = regions.map(\.sourceRange.location).min(),
               let end = regions.map({ $0.sourceRange.location + $0.sourceRange.length }).max() {
                range = NSRange(location: start, length: end - start)
            } else { range = NSRange(location: 0, length: 0) }
            tiles.append(AtlasTextTile(path: source.source.path, preparedLines: lines,
                header: header, sourceRange: range, display: display))
            top = bottom
        }
        let retainedBytes = fonts.reduce(0) { $0 + $1.data.count }
            + nativeFonts.count * 4096 + tiles.reduce(0) { $0 + ($1.display?.estimatedBytes ?? 0) }
        let document = AtlasDocument(source: source.source, capture: source.capture, tiles: tiles,
            presentation: .markdown, presentationBytes: retainedBytes)
        let counts = document.preparationCounts
        guard counts.glyphs <= allowedGlyphs, counts.runs <= runLimit, !canceled() else { return nil }
        return document
    }
}
