import Foundation
import AppKit
import CoreText
import CryptoKit

/// Production native admission, replay, placement and original-source mapping.
/// Typed display fixtures do not claim upstream parsing or physical GPU cadence.
@main enum AtlasParcelDocumentDisplayTests {
    static let text = "\u{feff}# Read e\u{301} 📘\r\n\r\n- [x] item\r\n\r\n| A | B |\r\n|---|---|\r\n| one | two |\r\n\r\n~~~rs\r\nlet x = 1;\r\n~~~\r\n![Chart](chart.png)\r\n"

    static func sha(_ source: String) -> String {
        SHA256.hash(data: Data(source.utf8)).map { String(format: "%02x", $0) }.joined()
    }
    static func source(_ text: String = AtlasParcelDocumentDisplayTests.text) -> AtlasDocument {
        let capture = AtlasHighlightCapture(schema: "fcb.source-document/1", text: text,
            runs: [.init(start: "0", length: String(text.utf16.count), role: "plain")])
        guard let value = AtlasDocument(path: "docs/README.md", capture: capture) else {
            preconditionFailure("Source fixture did not prepare")
        }
        return value
    }
    static func original(_ needle: String, in source: String = AtlasParcelDocumentDisplayTests.text) -> AtlasDocumentByteRange {
        guard let range = source.range(of: needle) else { preconditionFailure("Missing source fixture") }
        return .init(start: UInt64(source.utf8.distance(from: source.utf8.startIndex, to: range.lowerBound)),
            end: UInt64(source.utf8.distance(from: source.utf8.startIndex, to: range.upperBound)))
    }
    static func font() throws -> AtlasDocumentDisplayFont {
        // A real local fixture face is imported as bytes, then every glyph is
        // generated from that imported face. Production never reads this path.
        let native = CTFontCreateWithName("ArialMT" as CFString, 14, nil)
        guard let url = CTFontCopyAttribute(native, kCTFontURLAttribute) as? URL else {
            preconditionFailure("The macOS Arial fixture font is unavailable")
        }
        return try AtlasDocumentDisplayFont(index: 0, data: Data(contentsOf: url))
    }
    static func glyphs(_ text: String, font: AtlasDocumentDisplayFont) -> [AtlasDocumentDisplayGlyph] {
        let native = CTFontCreateWithGraphicsFont(font.face, 14, nil, nil)
        let characters = Array(text.utf16)
        var glyphs = [CGGlyph](repeating: 0, count: characters.count)
        precondition(CTFontGetGlyphsForCharacters(native, characters, &glyphs, characters.count))
        return glyphs.enumerated().map { .init(id: $0.element, font: 0, x: 1.25 + Double($0.offset) * 9, y: 0.5) }
    }
    static func item(_ id: UInt64, _ label: String, x: Double = 0, y: Double, height: Double = 20,
                     range: AtlasDocumentByteRange, font: AtlasDocumentDisplayFont,
                     role: String = "body") -> AtlasDocumentDisplayItem {
        .init(id: id, bounds: .init(x: x, y: y, width: 220, height: height), original: range,
            content: .text(label, role: role, size: height == 32 ? 24 : 14, glyphs: glyphs(label, font: font)))
    }
    static func pages(source: String = AtlasParcelDocumentDisplayTests.text, items: [AtlasDocumentDisplayItem],
                      font: AtlasDocumentDisplayFont, height: Double,
                      width: Double = 640, qualified: Bool = true, generation: UInt64 = 9) -> [AtlasDocumentDisplayPage] {
        let identity = AtlasReaderIdentity(owner: 7, file: 2, revision: 3,
            capturedBytes: UInt64(source.utf8.count), pathBytes: Array("/project/docs/README.md".utf8),
            encoding: "utf8", displayPath: "/project/docs/README.md")
        let document = AtlasReaderDocumentInventory(identity: identity, generation: generation,
            width: 80, totalLines: UInt64(items.count), totalHeadings: 1, renderedBytes: 100,
            sourceBase: source.utf8.starts(with: [0xef, 0xbb, 0xbf]) ? 3 : 0)
        let inventory = AtlasDocumentDisplayInventory(document: document, width: 640,
            bounds: .init(x: 0, y: 0, width: width, height: height), totalItems: UInt64(items.count),
            unresolvedImages: 1, qualifiedSources: qualified, fonts: [.init(index: 0, bytes: UInt64(font.data.count))])
        return stride(from: 0, to: items.count, by: 128).map { first in
            let end = min(first + 128, items.count)
            return .init(id: UUID(), inventory: inventory, first: UInt64(first),
                items: Array(items[first..<end]), next: end < items.count ? UInt64(end) : nil)
        }
    }
    static func prepare(_ source: AtlasDocument, pages: [AtlasDocumentDisplayPage],
                        font: AtlasDocumentDisplayFont, digest: String? = nil,
                        glyphLimit: Int = 262_144, tileLimit: Int = 64,
                        canceled: () -> Bool = { false }) -> AtlasDocument? {
        AtlasParcelDocumentDisplay.prepare(source: source, pages: pages, fonts: [font],
            captureSHA256: digest ?? sha(source.source.text), glyphLimit: glyphLimit,
            runLimit: 16_384, tileLimit: tileLimit, canceled: canceled)
    }

    static func completeDisplay(_ font: AtlasDocumentDisplayFont) {
        let base = source()
        let heading = original("# Read e\u{301} 📘"), list = original("- [x] item")
        let table = original("| A | B |\r\n|---|---|\r\n| one | two |")
        let fence = original("~~~rs\r\nlet x = 1;\r\n~~~"), image = original("![Chart](chart.png)")
        let items: [AtlasDocumentDisplayItem] = [
            item(0, "Read", y: 0, height: 32, range: heading, font: font),
            .init(id: 1, bounds: .init(x: 0, y: 46, width: 12, height: 12), original: list,
                content: .vector(shape: "checkbox-outline", role: "body", stroke: 1)),
            .init(id: 2, bounds: .init(x: 2, y: 48, width: 8, height: 8), original: list,
                content: .vector(shape: "checkbox-check", role: "body", stroke: 1)),
            item(3, "item", x: 20, y: 44, range: list, font: font),
            .init(id: 4, bounds: .init(x: 0, y: 76, width: 480, height: 60), original: table,
                content: .vector(shape: "table-border", role: "table-border", stroke: 1)),
            item(5, "A", y: 76, range: table, font: font),
            item(6, "B", x: 240, y: 76, range: table, font: font),
            item(7, "one two", y: 100, range: table, font: font),
            item(8, "let x = 1;", y: 160, range: fence, font: font, role: "keyword"),
            .init(id: 9, bounds: .init(x: 0, y: 210, width: 320, height: 100), original: image,
                content: .image(alt: "Chart")),
            .init(id: 10, bounds: .init(x: 0, y: 200, width: 480, height: 0), original: fence,
                content: .vector(shape: "horizontal-rule", role: "rule", stroke: 1))
        ]
        let wire = pages(items: items, font: font, height: 310)
        guard let preview = prepare(base, pages: wire, font: font), let tile = preview.tiles.first else {
            preconditionFailure("Complete qualified display was refused")
        }
        precondition(preview.presentation == .markdown && preview.tiles.count == 1)
        precondition(preview.source === base.source && preview.capture.text.utf8.elementsEqual(text.utf8))
        precondition(preview.presentationBytes >= font.data.count)
        precondition(tile.height == 332 && tile.display?.rows.count == tile.preparedLines?.count)
        precondition(!tile.supportsMetalGlyphs, "A decorated tile must keep complete native coverage")
        precondition(tile.preparedLines?.first?.sourceRange.length == 0, "Rendered glyphs acquired source offsets")
        precondition(tile.preparedLines?.first?.runs.first?.positions.first == CGPoint(x: 1.25, y: 0.5))
        precondition(tile.preparedLines?.first?.runs.first?.glyphs == glyphs("Read", font: font).map(\.id))
        precondition(tile.display?.drawings.count == items.count + 1, "An image slot lost its visible alt label")
        guard let last = tile.display?.drawings.last, case .vector(let rule) = last else {
            preconditionFailure("A zero-height rule lost its retained stroke")
        }
        precondition(rule.bounds.height == 0 && rule.shape == "horizontal-rule")
        precondition(base.tiles.allSatisfy { $0.display == nil && $0.rect == .zero })

        let parcel = CGRect(x: 10, y: 20, width: 1800, height: 1200)
        guard let fitted = AtlasParcelLayout.reflow(preview, inside: parcel), let shown = fitted.first else {
            preconditionFailure("Upstream display did not fit its existing parcel")
        }
        precondition(shown !== tile && tile.rect == .zero)
        precondition(shown.parcelRect == parcel && parcel.contains(shown.rect))
        precondition(shown.display?.rows.first?.origin == tile.display?.rows.first?.origin)
        let emoji = original("📘")
        guard let match = AtlasMatch.resolve(document: preview, byteStart: emoji.start, byteEnd: emoji.end,
            expectedSHA256: sha(text), expectedByteCount: UInt64(text.utf8.count)) else {
            preconditionFailure("Original Unicode bytes did not map to the enclosing rendered block")
        }
        precondition(match.mapsEnclosingBlocks && match.sourceRange.length == 2 && match.rowRects.count == 1)
        precondition(shown.rect.contains(match.focusRect))
        let missing = original("\r\n\r\n")
        precondition(AtlasMatch.resolve(document: preview, byteStart: missing.start, byteEnd: missing.end,
            expectedSHA256: sha(text), expectedByteCount: UInt64(text.utf8.count)) == nil,
            "Unmapped source bytes were assigned invented rendered coordinates")
        precondition(AtlasMatch.resolve(document: preview, byteStart: emoji.start, byteEnd: emoji.end,
            expectedSHA256: String(repeating: "0", count: 64), expectedByteCount: UInt64(text.utf8.count)) == nil)

        shown.prepareRaster(pixelBudget: 20_000)
        precondition(shown.raster != nil && shown.raster!.width * shown.raster!.height <= 20_000)
        precondition(base.capture.text.utf8.elementsEqual(text.utf8), "Rendering changed exact captured bytes")
        precondition(prepare(base, pages: wire, font: font, digest: String(repeating: "0", count: 64)) == nil)
        precondition(prepare(base, pages: wire, font: font, glyphLimit: 1) == nil)
        precondition(prepare(base, pages: wire, font: font, canceled: { true }) == nil)
        precondition(prepare(base, pages: pages(items: items, font: font, height: 310, qualified: false), font: font) == nil)
        precondition(prepare(base, pages: pages(items: items, font: font, height: 310, width: 641), font: font) == nil)
    }

    static func retainedWindows(_ font: AtlasDocumentDisplayFont) {
        let plain = "# Heading\nbody\n", base = source("# Heading\nbody\n")
        let range = original("# Heading", in: plain)
        let items = [
            item(0, "Heading", y: 0, range: range, font: font),
            item(1, "body", y: 1268, height: 24, range: range, font: font),
            item(2, "body", y: 2500, range: range, font: font)
        ]
        let wire = pages(source: plain, items: items, font: font, height: 2600)
        guard let preview = prepare(base, pages: wire, font: font) else {
            preconditionFailure("Bounded retained vertical windows did not prepare")
        }
        precondition(preview.tiles.count == 3 && preview.tiles[0].height == 22 + 1268,
            "A shaped line crossing the proposed boundary was cut in half")
        precondition(preview.tiles.allSatisfy(\.supportsMetalGlyphs))
        precondition(prepare(base, pages: wire, font: font, tileLimit: 1) == nil, "A display prefix was admitted")
        let firstParcel = CGRect(x: 0, y: 0, width: 2200, height: 1600)
        guard let first = AtlasParcelLayout.reflow(preview, inside: firstParcel) else {
            preconditionFailure("First display placement failed")
        }
        let oldGeometry = first.map(\.rect)
        let replacement = AtlasDocument(source: preview.source, capture: preview.capture, tiles: preview.tiles,
            presentation: preview.presentation, presentationBytes: preview.presentationBytes)
        guard let second = AtlasParcelLayout.reflow(replacement, inside: CGRect(x: 0, y: 0, width: 900, height: 2100)) else {
            preconditionFailure("Replacement display placement failed")
        }
        precondition(first.map(\.rect) == oldGeometry && second[0] !== first[0],
            "Repacking a retained display moved an older frame's tiles")
        precondition(second.map { $0.display!.rows.map(\.origin) } == first.map { $0.display!.rows.map(\.origin) })
    }

    static func pageAndMappingAdmission(_ font: AtlasDocumentDisplayFont) {
        let plain = "# Heading\n", base = source("# Heading\n"), range = original("# Heading", in: "# Heading\n")
        let items = (0..<129).map { index in
            item(UInt64(index), "row", y: Double(index) * 20, range: range, font: font)
        }
        let wire = pages(source: plain, items: items, font: font, height: 2580)
        precondition(wire.count == 2 && prepare(base, pages: wire, font: font) != nil)
        precondition(prepare(base, pages: [wire[0]], font: font) == nil, "Missing continuation was accepted")
        precondition(prepare(base, pages: [wire[1], wire[0]], font: font) == nil, "Reordered pages were accepted")
        let stale = pages(source: plain, items: items, font: font, height: 2580, generation: 10)
        precondition(prepare(base, pages: [wire[0], stale[1]], font: font) == nil, "A foreign generation was spliced into the display")

        // Transport pages may be shorter than the maximum item count because
        // upstream applies a separate glyph budget. They are not render tiles.
        let shortItems = Array(items.prefix(127))
        let shortInventory = pages(source: plain, items: shortItems, font: font, height: 2540)[0].inventory
        let shortPages = shortItems.enumerated().map { index, item in
            AtlasDocumentDisplayPage(id: UUID(), inventory: shortInventory, first: UInt64(index),
                items: [item], next: index + 1 < shortItems.count ? UInt64(index + 1) : nil)
        }
        precondition(prepare(base, pages: shortPages, font: font) != nil,
            "Transport pagination was mistaken for the maximum number of rendered windows")

        let unicode = "📘\n", unicodeSource = source("📘\n")
        let badBoundary = AtlasDocumentByteRange(start: 1, end: 4)
        let bad = [item(0, "book", y: 0, range: badBoundary, font: font)]
        precondition(prepare(unicodeSource, pages: pages(source: unicode, items: bad, font: font, height: 20), font: font) == nil,
            "A byte offset inside a Unicode scalar became source authority")

        let tall = [item(0, "row", y: 0, height: 1400, range: range, font: font)]
        precondition(prepare(base, pages: pages(source: plain, items: tall, font: font, height: 1400), font: font) == nil,
            "A single oversized protected element was silently clipped")
        let tooLarge = source(String(repeating: "x", count: AtlasParcelDocumentDisplay.sourceBytes + 1))
        precondition(prepare(tooLarge, pages: wire, font: font) == nil, "Source-byte admission was ignored")
    }

    static func main() throws {
        let font = try font()
        completeDisplay(font)
        retainedWindows(font)
        pageAndMappingAdmission(font)
        print("Atlas document display: exact captures, supplied glyph replay, bounded windows, retained placement, and enclosing-source mapping passed")
    }
}
