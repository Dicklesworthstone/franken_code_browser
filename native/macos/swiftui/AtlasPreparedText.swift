import Foundation
import AppKit
import CoreText
import CryptoKit

// Platform-owned replay data, not a lexer or a serialized CoreText object.
// Positions remain Float64 and glyph order is unchanged, including RTL runs.
struct AtlasPreparedRun {
    let font: CTFont
    let color: CGColor
    let matrix: CGAffineTransform
    let glyphs: [CGGlyph]
    let positions: [CGPoint]
    var fallbackText: String = ""
    private let blocks: [CGRect]
    static let blockSize = 16

    init(font: CTFont, color: CGColor, matrix: CGAffineTransform, glyphs: [CGGlyph],
         positions: [CGPoint], fallbackText: String = "") {
        self.font = font; self.color = color; self.matrix = matrix
        self.glyphs = glyphs; self.positions = positions; self.fallbackText = fallbackText
        guard glyphs.count == positions.count, matrix.isIdentity, CTFontGetMatrix(font).isIdentity,
              !CTFontGetSymbolicTraits(font).contains(.traitColorGlyphs) else { blocks = []; return }
        var result: [CGRect] = []
        for start in stride(from: 0, to: glyphs.count, by: Self.blockSize) {
            let end = min(start + Self.blockSize, glyphs.count)
            var bounds = [CGRect](repeating: .zero, count: end - start)
            let count = bounds.count
            glyphs.withUnsafeBufferPointer {
                _ = CTFontGetBoundingRectsForGlyphs(font, .default, $0.baseAddress!.advanced(by: start), &bounds, count)
            }
            var block = CGRect.null
            for index in bounds.indices {
                let position = positions[start + index]
                let box = bounds[index].offsetBy(dx: position.x, dy: position.y)
                guard [box.minX, box.minY, box.maxX, box.maxY].allSatisfy(\.isFinite) else { blocks = []; return }
                // Keep zero-size glyph bounds too: spacing and degenerate glyphs
                // are harmless conservative inclusions at their saved positions.
                block = block.union(box)
            }
            result.append(block)
        }
        blocks = result
    }

    /// Evaluated in the baseline's user coordinates after its y-axis flip.
    /// Nonstandard transforms/fonts retain the original full replay path.
    func visibleGlyphRange(in context: CGContext) -> Range<Int> {
        guard !blocks.isEmpty, context.ctm.b == 0, context.ctm.c == 0,
              context.ctm.a != 0, context.ctm.d != 0 else { return 0..<glyphs.count }
        let devicePixel = context.convertToUserSpace(CGSize(width: 2, height: 2))
        let margin = max(abs(devicePixel.width), abs(devicePixel.height)) + 1
        guard margin.isFinite else { return 0..<glyphs.count }
        let clip = context.boundingBoxOfClipPath.insetBy(dx: -margin, dy: -margin)
        var first: Int?, last = 0
        for (index, box) in blocks.enumerated() where box.intersects(clip) {
            if first == nil { first = index }
            last = index
        }
        guard let first else { return 0..<0 }
        return (first * Self.blockSize)..<min((last + 1) * Self.blockSize, glyphs.count)
    }
}

struct AtlasPreparedLine {
    let runs: [AtlasPreparedRun]
    var sourceRange: NSRange = NSRange(location: 0, length: 0)
    var retainedColorLine: CTLine?
    var hasColorGlyphs: Bool { runs.contains { CTFontGetSymbolicTraits($0.font).contains(.traitColorGlyphs) } }

    init?(_ line: CTLine, source: String = "") {
        var result: [AtlasPreparedRun] = []
        for run in CTLineGetGlyphRuns(line) as! [CTRun] {
            let attributes = CTRunGetAttributes(run) as NSDictionary
            // AppKit-authored attributed strings retain NSFont/NSColor keys.
            // CoreText-authored strings use their kCT equivalents.
            guard let font = attributes[kCTFontAttributeName]
                ?? attributes[NSAttributedString.Key.font.rawValue] else { return nil }
            let value = (attributes[kCTForegroundColorAttributeName]
                ?? attributes[NSAttributedString.Key.foregroundColor.rawValue]) as AnyObject?
            let color: CGColor?
            if let nsColor = value as? NSColor { color = nsColor.cgColor }
            else if let value, CFGetTypeID(value) == CGColor.typeID { color = (value as! CGColor) }
            else { color = nil }
            guard let color else { return nil }
            let count = CTRunGetGlyphCount(run)
            guard count >= 0, count <= 4 * 1024 * 1024 else { return nil }
            var glyphs = [CGGlyph](repeating: 0, count: count)
            var positions = [CGPoint](repeating: .zero, count: count)
            CTRunGetGlyphs(run, CFRange(location: 0, length: 0), &glyphs)
            CTRunGetPositions(run, CFRange(location: 0, length: 0), &positions)
            let range = CTRunGetStringRange(run)
            let text = source as NSString
            let seed: String
            if range.location >= 0, range.length >= 0,
               range.location <= text.length, range.length <= text.length - range.location {
                seed = String(String.UnicodeScalarView(text.substring(with: NSRange(location: range.location, length: range.length)).unicodeScalars.prefix(64)))
            } else { seed = "" }
            result.append(AtlasPreparedRun(font: font as! CTFont, color: color,
                matrix: CTRunGetTextMatrix(run), glyphs: glyphs, positions: positions, fallbackText: seed))
        }
        runs = result
        let range = CTLineGetStringRange(line)
        sourceRange = NSRange(location: range.location, length: range.length)
    }

    init(runs: [AtlasPreparedRun]) { self.runs = runs }

    func draw(in context: CGContext, origin: CGPoint) {
        if let retainedColorLine {
            context.textMatrix = CGAffineTransform(scaleX: 1, y: -1)
            context.textPosition = origin
            CTLineDraw(retainedColorLine, context)
            return
        }
        // All runs share the line origin. Enter the baseline coordinate space
        // once, preserving run order and each run's actual font/color/matrix.
        // This removes repeated state-stack and transform work from dense text.
        context.saveGState()
        context.translateBy(x: origin.x, y: origin.y)
        context.scaleBy(x: 1, y: -1)
        for run in runs {
            context.setFillColor(run.color)
            context.textMatrix = run.matrix
            let visible = run.visibleGlyphRange(in: context)
            if !visible.isEmpty {
                run.glyphs.withUnsafeBufferPointer { glyphs in
                    run.positions.withUnsafeBufferPointer { positions in
                        CTFontDrawGlyphs(run.font, glyphs.baseAddress!.advanced(by: visible.lowerBound),
                            positions.baseAddress!.advanced(by: visible.lowerBound), visible.count, context)
                    }
                }
            }
        }
        context.restoreGState()
    }
}

enum AtlasPreparationError: Error { case invalid, limit, font
    case fontIdentity(expected: String, actual: String)
    case fontFingerprint(name: String, expected: String, candidates: [String])
    case fontUnavailable(String)
}

struct AtlasBinaryWriter {
    static let limit = 64 * 1024 * 1024
    var data = Data()
    mutating func bytes(_ bytes: Data) throws {
        guard bytes.count <= Self.limit - data.count else { throw AtlasPreparationError.limit }
        data.append(bytes)
    }
    mutating func integer<T: FixedWidthInteger>(_ value: T) throws {
        var value = value.littleEndian
        try withUnsafeBytes(of: &value) { try bytes(Data($0)) }
    }
    mutating func number(_ value: Double) throws {
        guard value.isFinite else { throw AtlasPreparationError.invalid }
        try integer(value.bitPattern)
    }
    mutating func string(_ value: String) throws {
        let bytes = Data(value.utf8)
        try integer(UInt64(bytes.count)); try self.bytes(bytes)
    }
}

struct AtlasBinaryReader {
    let data: Data
    var offset = 0
    mutating func bytes(_ count: Int) throws -> Data {
        guard count >= 0, count <= data.count - offset else { throw AtlasPreparationError.invalid }
        defer { offset += count }
        return data.subdata(in: offset..<offset + count)
    }
    mutating func integer<T: FixedWidthInteger>(_ type: T.Type = T.self) throws -> T {
        let size = MemoryLayout<T>.size
        guard size <= data.count - offset else { throw AtlasPreparationError.invalid }
        defer { offset += size }
        return data.withUnsafeBytes { T(littleEndian: $0.loadUnaligned(fromByteOffset: offset, as: T.self)) }
    }
    mutating func count(_ maximum: Int, stride: Int = 1) throws -> Int {
        guard maximum >= 0, stride > 0 else { throw AtlasPreparationError.limit }
        let raw: UInt64 = try integer()
        guard raw <= UInt64(maximum),
              raw <= UInt64((data.count - offset) / stride) else { throw AtlasPreparationError.limit }
        return Int(raw)
    }
    mutating func number() throws -> Double {
        let value = Double(bitPattern: try integer())
        guard value.isFinite else { throw AtlasPreparationError.invalid }
        return value
    }
    mutating func string(_ maximum: Int) throws -> String {
        let count = try count(maximum)
        guard let text = String(data: try bytes(count), encoding: .utf8) else { throw AtlasPreparationError.invalid }
        return text
    }
}

// Shared within ONE project cache. Font validation is paid once per exact face,
// not once per line. No persisted path is ever opened as a font resource.
final class AtlasPreparedFonts {
    private var restored: [Data: CTFont] = [:]
    private var encoded: [Data: Data] = [:]
    func fingerprint(_ font: CTFont) throws -> Data {
        let name = CTFontCopyPostScriptName(font) as String
        // CoreText can drop GDEF from its inventory after shaping a face.
        // The underlying graphics font retains the complete resource tables,
        // so identity is independent of whether layout has already happened.
        let graphics = CTFontCopyGraphicsFont(font, nil)
        guard let tables = graphics.tableTags, CFArrayGetCount(tables) <= 128 else {
            throw AtlasPreparationError.fontUnavailable("tables: \(name)")
        }
        var hash = SHA256()
        var total = 0
        // Graphics font table tags are unboxed values, not NSNumber objects.
        let tags = (0..<CFArrayGetCount(tables)).map {
            UInt32(truncatingIfNeeded: UInt(bitPattern: CFArrayGetValueAtIndex(tables, $0)))
        }
        for tag in tags.sorted() {
            guard let table = graphics.table(for: tag) else {
                throw AtlasPreparationError.fontUnavailable("missing resource table: \(name), tag: \(tag)")
            }
            let data = table as Data
            // AppleColorEmoji carries a ~191 MB bitmap table on the tested OS.
            // This bounded font-validation allowance is separate from artifact size.
            guard data.count <= 256 * 1024 * 1024 - total else {
                throw AtlasPreparationError.fontUnavailable("font table budget: \(name), table bytes: \(data.count), prior: \(total)")
            }
            total += data.count
            var id = tag.littleEndian
            withUnsafeBytes(of: &id) { hash.update(data: Data($0)) }
            hash.update(data: data)
        }
        return Data(hash.finalize())
    }

    func encode(_ font: CTFont, fallbackText: String) throws -> Data {
        var writer = AtlasBinaryWriter()
        try writer.string(CTFontCopyPostScriptName(font) as String)
        try writer.number(CTFontGetSize(font))
        try Self.matrix(CTFontGetMatrix(font), into: &writer)
        let variation = (CTFontCopyVariation(font) as? [NSNumber: NSNumber]) ?? [:]
        guard variation.count <= 64 else { throw AtlasPreparationError.limit }
        try writer.integer(UInt64(variation.count))
        for key in variation.keys.sorted(by: { $0.uint32Value < $1.uint32Value }) {
            try writer.integer(key.uint32Value)
            try writer.number(variation[key]!.doubleValue)
        }
        // A PostScript name can identify more than one physical font resource.
        // This URL is only a memo key, never a persisted font load path.
        var identityWriter = writer
        let resource = CTFontCopyAttribute(font, kCTFontURLAttribute) as? URL
        try identityWriter.string(resource?.absoluteString ?? "")
        let identity = identityWriter.data
        if let existing = encoded[identity] { return existing }
        guard encoded.count < 128 else { throw AtlasPreparationError.limit }
        try writer.string(fallbackText)
        try writer.bytes(fingerprint(font))
        encoded[identity] = writer.data
        return writer.data
    }

    func decode(_ data: Data) throws -> (font: CTFont, fallbackText: String) {
        var reader = AtlasBinaryReader(data: data)
        let name = try reader.string(512)
        let size = try reader.number()
        guard size > 0, size <= 256 else { throw AtlasPreparationError.fontUnavailable("size: \(size), name: \(name)") }
        var matrix = try Self.matrix(from: &reader)
        let count = try reader.count(64, stride: 12)
        var variations: [NSNumber: NSNumber] = [:]
        for _ in 0..<count {
            let key: UInt32 = try reader.integer()
            let value = try reader.number()
            guard variations[NSNumber(value: key)] == nil else { throw AtlasPreparationError.invalid }
            variations[NSNumber(value: key)] = NSNumber(value: value)
        }
        let descriptorEnd = reader.offset
        let fallbackText = try reader.string(4096)
        let expected = try reader.bytes(32)
        guard reader.offset == data.count else { throw AtlasPreparationError.invalid }
        // Different incremental sessions can save different source seeds for
        // the same verified face. Seeds must not consume font identity slots.
        var identity = Data(data.prefix(descriptorEnd))
        identity.append(expected)
        if let found = restored[identity] { return (found, fallbackText) }
        guard restored.count < 128 else { throw AtlasPreparationError.limit }
        let descriptor = CTFontDescriptorCreateWithAttributes([
            kCTFontNameAttribute: name, kCTFontVariationAttribute: variations
        ] as CFDictionary)
        var font = CTFontCreateWithFontDescriptor(descriptor, size, &matrix)
        var candidates: [String] = []
        func matches(_ candidate: CTFont) throws -> Bool {
            guard CTFontCopyPostScriptName(candidate) as String == name else { return false }
            let actual = try fingerprint(candidate)
            let resource = CTFontCopyAttribute(candidate, kCTFontURLAttribute) as? URL
            candidates.append("\(resource?.lastPathComponent ?? "unknown"):" + actual.map { String(format: "%02x", $0) }.joined())
            return actual == expected
        }
        var matched = try matches(font)
        if !matched, !fallbackText.isEmpty {
            // Private UI fallback faces must be obtained through the system's
            // font cascade. Matching a font does not reshape saved glyphs.
            let text = fallbackText as CFString
            for weight: NSFont.Weight in [.regular, .medium] {
                let base = NSFont.monospacedSystemFont(ofSize: size, weight: weight) as CTFont
                let candidate = CTFontCreateForString(base, text, CFRange(location: 0, length: CFStringGetLength(text)))
                guard CTFontCopyPostScriptName(candidate) as String == name else { continue }
                let attributes = CTFontDescriptorCreateWithAttributes([kCTFontVariationAttribute: variations] as CFDictionary)
                let restoredCandidate = CTFontCreateCopyWithAttributes(candidate, size, &matrix, attributes)
                if try matches(restoredCandidate) {
                    font = restoredCandidate
                    matched = true
                    break
                }
            }
        }
        if !matched {
            // A preferred-name lookup may select a different registered face
            // with the same PostScript name. Ask CoreText for alternatives;
            // never open a font path supplied by an artifact.
            let query = CTFontDescriptorCreateWithAttributes([kCTFontNameAttribute: name] as CFDictionary)
            let mandatory = NSSet(object: kCTFontNameAttribute) as CFSet
            let alternatives = CTFontDescriptorCreateMatchingFontDescriptors(query, mandatory) as? [CTFontDescriptor] ?? []
            for alternative in alternatives.prefix(16) {
                let varied = CTFontDescriptorCreateCopyWithAttributes(alternative,
                    [kCTFontVariationAttribute: variations] as CFDictionary)
                let candidate = CTFontCreateWithFontDescriptor(varied, size, &matrix)
                if try matches(candidate) {
                    font = candidate
                    matched = true
                    break
                }
            }
        }
        let actual = CTFontCopyPostScriptName(font) as String
        guard actual == name else {
            throw AtlasPreparationError.fontIdentity(expected: name, actual: actual)
        }
        guard matched else {
            throw AtlasPreparationError.fontFingerprint(name: name,
                expected: expected.map { String(format: "%02x", $0) }.joined(), candidates: candidates)
        }
        restored[identity] = font
        return (font, fallbackText)
    }

    static func matrix(_ matrix: CGAffineTransform, into writer: inout AtlasBinaryWriter) throws {
        for value in [matrix.a, matrix.b, matrix.c, matrix.d, matrix.tx, matrix.ty] {
            try writer.number(value)
        }
    }
    static func matrix(from reader: inout AtlasBinaryReader) throws -> CGAffineTransform {
        let values = try (0..<6).map { _ in try reader.number() }
        guard values.allSatisfy({ abs($0) <= 1_000_000 }) else { throw AtlasPreparationError.invalid }
        return CGAffineTransform(a: values[0], b: values[1], c: values[2], d: values[3], tx: values[4], ty: values[5])
    }
}
