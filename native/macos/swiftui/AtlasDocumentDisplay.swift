import Foundation
#if canImport(CoreGraphics)
import CoreGraphics
#endif

/// A bounded consumer of the shared engine's already-shaped display list.
/// These coordinates, glyph IDs and source regions are never reconstructed by
/// parsing Markdown or reshaping its strings in the native host.
enum AtlasDocumentDisplayLimits {
    static let pageItems: UInt64 = 128
    static let items: UInt64 = 8192
    static let pageGlyphs = 16_384
    static let runGlyphs = 4096
    static let fonts = 6
    static let fontBytes: UInt64 = 8 * 1024 * 1024
    static let fontChunk: UInt64 = 64 * 1024
}

struct AtlasDocumentDisplayRect: Decodable, Equatable, Sendable {
    let x, y, width, height: Double
    var maxX: Double { x + width }
    var maxY: Double { y + height }
    var valid: Bool {
        [x, y, width, height, maxX, maxY].allSatisfy { $0.isFinite && abs($0) <= 10_000_000 }
            && width >= 0 && height >= 0
    }
}

struct AtlasDocumentDisplayFontSpec: Equatable, Sendable {
    let index: UInt64
    let bytes: UInt64
}

/// Immutable faces are constructed on the existing reader worker, before
/// publication. Drawing uses these exact bytes, not a similarly named system
/// font. The immutable CGFont and its retained provider have no UI ownership.
final class AtlasDocumentDisplayFont: @unchecked Sendable {
    let index: UInt64
    let data: Data
#if canImport(CoreGraphics)
    let face: CGFont
#endif
    init(index: UInt64, data: Data) throws {
        guard !data.isEmpty, data.count <= Int(AtlasDocumentDisplayLimits.fontBytes) else {
            throw AtlasReaderError.invalidResponse
        }
        self.index = index
        self.data = data
#if canImport(CoreGraphics)
        guard let provider = CGDataProvider(data: data as CFData), let face = CGFont(provider),
              face.numberOfGlyphs > 0 else { throw AtlasReaderError.invalidResponse }
        self.face = face
#endif
    }
    func contains(glyph: UInt16) -> Bool {
#if canImport(CoreGraphics)
        Int(glyph) < face.numberOfGlyphs
#else
        // Portable wire tests do not claim native font validation.
        true
#endif
    }
}

struct AtlasDocumentDisplayInventory: Sendable {
    let document: AtlasReaderDocumentInventory
    let width: UInt64
    let bounds: AtlasDocumentDisplayRect
    let totalItems: UInt64
    let unresolvedImages: UInt64
    let qualifiedSources: Bool
    let fonts: [AtlasDocumentDisplayFontSpec]
    func matches(_ other: Self) -> Bool {
        document.matches(other.document) && width == other.width && bounds == other.bounds
            && totalItems == other.totalItems && unresolvedImages == other.unresolvedImages
            && qualifiedSources == other.qualifiedSources && fonts == other.fonts
    }
}

struct AtlasDocumentDisplayGlyph: Sendable {
    let id: UInt16
    let font: UInt64
    let x, y: Double
}

struct AtlasDocumentDisplayItem: Identifiable, Sendable {
    enum Content: Sendable {
        case text(String, role: String, size: Double, glyphs: [AtlasDocumentDisplayGlyph])
        case vector(shape: String, role: String, stroke: Double)
        case image(alt: String)
        case anchor(String, heading: Bool, level: UInt64)
    }
    let id: UInt64
    let bounds: AtlasDocumentDisplayRect
    let original: AtlasDocumentByteRange?
    let content: Content
    var readingText: String? {
        switch content {
        case .text(let text, _, _, _): text
        case .image(let alt): alt.isEmpty ? "Image unavailable" : "Image: " + alt
        case .vector, .anchor: nil
        }
    }
}

struct AtlasDocumentDisplayPage: Sendable {
    let id: UUID
    let inventory: AtlasDocumentDisplayInventory
    let first: UInt64
    let items: [AtlasDocumentDisplayItem]
    let next: UInt64?
    /// Page geometry is a slice of the upstream coordinate system. Pages may
    /// overlap vertically when many items occupy the same table/paragraph.
    var top: Double { items.map(\.bounds.y).min() ?? inventory.bounds.y }
    var bottom: Double { items.map(\.bounds.maxY).max() ?? top }
    func validateFonts(_ fonts: [AtlasDocumentDisplayFont]) throws {
        guard fonts.count == inventory.fonts.count else { throw AtlasReaderError.invalidResponse }
        for spec in inventory.fonts {
            guard let font = fonts.first(where: { $0.index == spec.index }),
                  UInt64(font.data.count) == spec.bytes else { throw AtlasReaderError.invalidResponse }
        }
        for item in items {
            if case .text(_, _, _, let glyphs) = item.content {
                for glyph in glyphs {
                    guard let font = fonts.first(where: { $0.index == glyph.font }),
                          font.contains(glyph: glyph.id) else { throw AtlasReaderError.invalidResponse }
                }
            }
        }
    }
}

/// No header is accepted from a different source/capture, no string counter is
/// rounded through a JSON number, and arrays are bounded while decoding.
enum AtlasDocumentDisplayWire {
    private struct Key: CodingKey {
        let stringValue: String
        var intValue: Int? { nil }
        init(_ value: String) { stringValue = value }
        init?(stringValue: String) { self.init(stringValue) }
        init?(intValue: Int) { return nil }
    }
    private struct Fields: Decodable {
        let header: AtlasReaderWire.Header
        let values: KeyedDecodingContainer<Key>
        init(from decoder: Decoder) throws {
            header = try .init(from: decoder)
            values = try decoder.container(keyedBy: Key.self)
        }
        func text(_ name: String) throws -> String { try values.decode(String.self, forKey: Key(name)) }
        func number(_ name: String) throws -> UInt64 {
            guard let n = AtlasReaderWire.integer(try text(name)) else { throw AtlasReaderError.invalidResponse }
            return n
        }
        func optionalNumber(_ name: String) throws -> UInt64? {
            guard let text = try values.decodeIfPresent(String.self, forKey: Key(name)) else { return nil }
            guard let n = AtlasReaderWire.integer(text) else { throw AtlasReaderError.invalidResponse }
            return n
        }
        func flag(_ name: String) throws -> Bool { try values.decode(Bool.self, forKey: Key(name)) }
        func array<T: Decodable>(_ name: String, limit: Int) throws -> [T] {
            var c = try values.nestedUnkeyedContainer(forKey: Key(name))
            if let count = c.count, count > limit { throw AtlasReaderError.invalidResponse }
            var result: [T] = []
            while !c.isAtEnd {
                guard result.count < limit else { throw AtlasReaderError.invalidResponse }
                result.append(try c.decode(T.self))
            }
            return result
        }
        func validate(_ document: AtlasReaderDocumentInventory, command: String) throws {
            guard try header.identity(handle: document.identity.owner).matches(document.identity),
                  header.command == command, try number("document_generation") == document.generation,
                  try text("rendering") == "bundled-frankenmarkdown-display",
                  try flag("glyph_shaped"), try !flag("native_shaped"),
                  try text("source_mapping") == "enclosing-ast-blocks-not-glyph-exact" else {
                throw AtlasReaderError.invalidResponse
            }
        }
        func inventory(_ document: AtlasReaderDocumentInventory) throws -> AtlasDocumentDisplayInventory {
            struct Font: Decodable { let font_index, byte_length: String }
            let width = try number("display_width_points"), total = try number("total_display_items")
            let images = try number("unresolved_images"), qualified = try flag("source_ranges_qualified")
            let bounds = try values.decode(AtlasDocumentDisplayRect.self, forKey: Key("display_bounds"))
            let rows: [Font] = try array("fonts", limit: AtlasDocumentDisplayLimits.fonts)
            var bytes: UInt64 = 0
            let fonts = try rows.enumerated().map { offset, row -> AtlasDocumentDisplayFontSpec in
                guard let index = AtlasReaderWire.integer(row.font_index), index == UInt64(offset),
                      let count = AtlasReaderWire.integer(row.byte_length), count > 0,
                      count <= AtlasDocumentDisplayLimits.fontBytes - bytes else {
                    throw AtlasReaderError.invalidResponse
                }
                bytes += count
                return .init(index: index, bytes: count)
            }
            guard (160...4096).contains(width), total <= AtlasDocumentDisplayLimits.items,
                  images <= total, bounds.valid, bounds.x >= 0, bounds.y >= 0 else {
                throw AtlasReaderError.invalidResponse
            }
            return .init(document: document, width: width, bounds: bounds, totalItems: total,
                unresolvedImages: images, qualifiedSources: qualified, fonts: fonts)
        }
    }
    private struct Glyph: Decodable {
        let glyph_id, font_index: String
        let x, y: Double
    }
    private struct Item: Decodable {
        let fields: KeyedDecodingContainer<Key>
        let glyphs: [Glyph]
        init(from decoder: Decoder) throws {
            fields = try decoder.container(keyedBy: Key.self)
            if fields.contains(Key("glyphs")) {
                var c = try fields.nestedUnkeyedContainer(forKey: Key("glyphs"))
                if let count = c.count, count > AtlasDocumentDisplayLimits.runGlyphs {
                    throw AtlasReaderError.invalidResponse
                }
                var glyphs: [Glyph] = []
                while !c.isAtEnd {
                    guard glyphs.count < AtlasDocumentDisplayLimits.runGlyphs else { throw AtlasReaderError.invalidResponse }
                    glyphs.append(try c.decode(Glyph.self))
                }
                self.glyphs = glyphs
            } else { glyphs = [] }
        }
        func text(_ key: String) throws -> String { try fields.decode(String.self, forKey: Key(key)) }
        func value(index: UInt64, inventory: AtlasDocumentDisplayInventory,
                   glyphCount: inout Int, textBytes: inout Int) throws -> AtlasDocumentDisplayItem {
            let bounds = try fields.decode(AtlasDocumentDisplayRect.self, forKey: Key("bounds"))
            let original = try fields.decodeIfPresent(AtlasDocumentByteRange.self, forKey: Key("enclosing_original_range"))
            guard AtlasReaderWire.integer(try text("item_index")) == index, bounds.valid,
                  bounds.x >= inventory.bounds.x - 1, bounds.y >= inventory.bounds.y - 1,
                  bounds.maxX <= inventory.bounds.maxX + 1, bounds.maxY <= inventory.bounds.maxY + 1 else {
                throw AtlasReaderError.invalidResponse
            }
            if let original {
                guard inventory.qualifiedSources,
                      original.valid(upTo: inventory.document.identity.capturedBytes, nonempty: true),
                      original.start >= inventory.document.sourceBase else { throw AtlasReaderError.invalidResponse }
            }
            let kind = try text("kind")
            let content: AtlasDocumentDisplayItem.Content
            switch kind {
            case "text":
                let text = try self.text("text"), role = try self.text("color_role")
                let size = try fields.decode(Double.self, forKey: Key("font_size"))
                textBytes += text.utf8.count; glyphCount += glyphs.count
                guard textBytes <= 256 * 1024, glyphCount <= AtlasDocumentDisplayLimits.pageGlyphs,
                      size.isFinite, (1...256).contains(size), role.utf8.count <= 64,
                      text.isEmpty || !glyphs.isEmpty else { throw AtlasReaderError.invalidResponse }
                let glyphs = try glyphs.map { glyph -> AtlasDocumentDisplayGlyph in
                    guard let id = AtlasReaderWire.integer(glyph.glyph_id), id > 0, let exact = UInt16(exactly: id),
                          let font = AtlasReaderWire.integer(glyph.font_index),
                          inventory.fonts.contains(where: { $0.index == font }),
                          glyph.x.isFinite, glyph.y.isFinite,
                          abs(glyph.x) <= 1_000_000, abs(glyph.y) <= 1_000_000 else {
                        throw AtlasReaderError.invalidResponse
                    }
                    return .init(id: exact, font: font, x: glyph.x, y: glyph.y)
                }
                content = .text(text, role: role, size: size, glyphs: glyphs)
            case "vector":
                let shape = try text("shape"), role = try text("color_role")
                let stroke = try fields.decode(Double.self, forKey: Key("stroke_width"))
                guard ["horizontal-rule", "table-border", "quote-accent", "checkbox-outline",
                       "checkbox-check"].contains(shape),
                      stroke.isFinite, (0...64).contains(stroke), role.utf8.count <= 64,
                      glyphs.isEmpty else { throw AtlasReaderError.invalidResponse }
                content = .vector(shape: shape, role: role, stroke: stroke)
            case "image":
                let alt = try text("alt_text")
                guard try !fields.decode(Bool.self, forKey: Key("is_resolved")), glyphs.isEmpty,
                      alt.utf8.count <= 16_384 else { throw AtlasReaderError.invalidResponse }
                content = .image(alt: alt)
            case "anchor":
                let id = try text("anchor_id")
                let heading = try fields.decode(Bool.self, forKey: Key("is_heading"))
                guard id.utf8.count <= 4096, !id.utf8.contains(0), glyphs.isEmpty,
                      let level = AtlasReaderWire.integer(try text("level")), level <= 6 else {
                    throw AtlasReaderError.invalidResponse
                }
                content = .anchor(id, heading: heading, level: level)
            default: throw AtlasReaderError.invalidResponse
            }
            return .init(id: index, bounds: bounds, original: original, content: content)
        }
    }
    static func page(_ json: String?, document: AtlasReaderDocumentInventory,
                     command: String, first expectedFirst: UInt64?, width: UInt64? = nil,
                     sourceAnchor: UInt64? = nil,
                     previous: AtlasDocumentDisplayInventory? = nil) throws -> AtlasDocumentDisplayPage {
        do {
            let data = try AtlasReaderWire.admit(json, handle: document.identity.owner)
            let w = try JSONDecoder().decode(Fields.self, from: data)
            try w.validate(document, command: command)
            let inventory = try w.inventory(document)
            let first = try w.number("first_display_item")
            guard width == nil || width == inventory.width,
                  previous == nil || previous!.matches(inventory),
                  expectedFirst == nil || expectedFirst == first, first <= inventory.totalItems else {
                throw AtlasReaderError.invalidResponse
            }
            if let sourceAnchor {
                guard inventory.qualifiedSources,
                      sourceAnchor >= document.sourceBase,
                      sourceAnchor <= document.identity.capturedBytes,
                      try w.number("original_anchor") == sourceAnchor else {
                    throw AtlasReaderError.invalidResponse
                }
            }
            var rows = try w.values.nestedUnkeyedContainer(forKey: Key("display_items"))
            if let count = rows.count, count > Int(AtlasDocumentDisplayLimits.pageItems) {
                throw AtlasReaderError.invalidResponse
            }
            var items: [AtlasDocumentDisplayItem] = []
            var glyphCount = 0, textBytes = 0
            while !rows.isAtEnd {
                guard items.count < Int(AtlasDocumentDisplayLimits.pageItems) else { throw AtlasReaderError.invalidResponse }
                // Validate each bounded run before decoding the next. This
                // prevents accumulating 128 maximum-size glyph arrays first.
                let row = try rows.decode(Item.self)
                items.append(try row.value(index: first + UInt64(items.count), inventory: inventory,
                    glyphCount: &glyphCount, textBytes: &textBytes))
            }
            let remaining = inventory.totalItems - first
            guard UInt64(items.count) <= remaining, remaining == 0 || !items.isEmpty else {
                throw AtlasReaderError.invalidResponse
            }
            let end = first + UInt64(items.count), next = try w.optionalNumber("next_display_item")
            guard next == (end < inventory.totalItems ? end : nil) else { throw AtlasReaderError.invalidResponse }
            if let sourceAnchor {
                if sourceAnchor == document.identity.capturedBytes {
                    guard first == inventory.totalItems, items.isEmpty else {
                        throw AtlasReaderError.invalidResponse
                    }
                } else {
                    // An echoed request offset does not prove that the returned
                    // page begins at its enclosing captured source block.
                    guard let original = items.first?.original,
                          original.start <= sourceAnchor, sourceAnchor < original.end else {
                        throw AtlasReaderError.invalidResponse
                    }
                }
            }
            return .init(id: UUID(), inventory: inventory, first: first, items: items, next: next)
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }
    static func fontChunk(_ json: String?, inventory: AtlasDocumentDisplayInventory,
                          spec: AtlasDocumentDisplayFontSpec, offset: UInt64) throws -> Data {
        do {
            let data = try AtlasReaderWire.admit(json, handle: inventory.document.identity.owner)
            let w = try JSONDecoder().decode(Fields.self, from: data)
            try w.validate(inventory.document, command: "document-display-font")
            guard try w.number("display_width_points") == inventory.width,
                  try w.number("font_index") == spec.index, try w.number("font_offset") == offset,
                  try w.number("total_font_bytes") == spec.bytes, offset < spec.bytes else {
                throw AtlasReaderError.invalidResponse
            }
            let count = min(AtlasDocumentDisplayLimits.fontChunk, spec.bytes - offset)
            guard try w.number("font_bytes") == count,
                  try w.optionalNumber("next_font_offset") == (offset + count < spec.bytes ? offset + count : nil) else {
                throw AtlasReaderError.invalidResponse
            }
            let hex = try w.text("font_hex")
            guard hex.utf8.count == Int(count * 2),
                  hex.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else {
                throw AtlasReaderError.invalidResponse
            }
            var result = Data(); result.reserveCapacity(Int(count))
            let bytes = Array(hex.utf8)
            func nibble(_ b: UInt8) -> UInt8 { b <= 57 ? b - 48 : b - 87 }
            for i in stride(from: 0, to: bytes.count, by: 2) { result.append(nibble(bytes[i]) * 16 + nibble(bytes[i + 1])) }
            return result
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }
}
