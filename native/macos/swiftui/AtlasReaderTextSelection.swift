import Foundation

/// Coordinates supplied by the native text control belong only to this decoded
/// page. Converting them to UTF-8 is display-coordinate work, not a source-byte
/// mapping. The retained Rust decoder alone resolves original source offsets.
struct AtlasReaderTextSelectionRequest: Sendable {
    let page: AtlasReaderPage
    let nativeRange: NSRange
    let utf8Start: UInt64
    let utf8End: UInt64
    let text: String

    /// Runs on the bounded reader worker, never on the selection callback.
    init(page: AtlasReaderPage, nativeRange: NSRange) throws {
        guard page.start <= page.end, page.end <= page.identity.capturedBytes,
              page.end - page.start <= AtlasReaderLimits.maximumPageBytes + 16,
              page.text.utf8.count <= Int((AtlasReaderLimits.maximumPageBytes + 16) * 3),
              AtlasReaderWire.validHex(page.originalHex, bytes: page.end - page.start) else {
            throw AtlasReaderError.invalidInput
        }
        let native = page.text as NSString
        guard nativeRange.location != NSNotFound, nativeRange.location >= 0,
              nativeRange.length > 0, nativeRange.location <= native.length,
              nativeRange.length <= native.length - nativeRange.location,
              native.rangeOfComposedCharacterSequences(for: nativeRange) == nativeRange,
              let range = Range(nativeRange, in: page.text) else {
            throw AtlasReaderError.invalidInput
        }
        let start = page.text.utf8.distance(from: page.text.utf8.startIndex, to: range.lowerBound)
        let end = page.text.utf8.distance(from: page.text.utf8.startIndex, to: range.upperBound)
        guard start >= 0, start < end else { throw AtlasReaderError.invalidInput }
        self.page = page; self.nativeRange = nativeRange
        utf8Start = UInt64(start); utf8End = UInt64(end)
        text = String(page.text[range])
    }
}

/// A verified, bounded selection of one immutable captured page. The logical
/// text preserves source order and CR/LF; originalHex preserves original
/// encoding and any malformed bytes. Neither value reads a live pathname.
struct AtlasReaderTextSelection: Sendable, Identifiable {
    let id: UUID
    let identity: AtlasReaderIdentity
    let pageStart: UInt64
    let pageEnd: UInt64
    let nativeRange: NSRange
    let utf8Start: UInt64
    let utf8End: UInt64
    let originalStart: UInt64
    let originalEnd: UInt64
    let text: String
    let originalHex: String
    let containsReplacements: Bool
    let withLocationText: String

    var summary: String {
        let kind = containsReplacements ? "decoded text with replacement characters" : "logical Unicode text"
        return "Selected original bytes \(originalStart)..<\(originalEnd) · \(kind); line endings preserved."
    }

    static func decode(_ json: String?, request: AtlasReaderTextSelectionRequest,
                       captureSHA256: String? = nil) throws -> Self {
        let page = request.page
        let data = try AtlasReaderWire.admit(json, handle: page.identity.owner)
        do {
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            let identity = try wire.header.identity(handle: page.identity.owner)
            let window = try wire.window_original_range.values()
            let utf8 = try wire.window_utf8_range.values()
            let original = try wire.original_range.values()
            guard wire.header.command == "resolve-selection",
                  wire.selection_namespace == "native-text", identity.matches(page.identity),
                  window.start == page.start, window.end == page.end,
                  utf8.start == request.utf8Start, utf8.end == request.utf8End,
                  original.start >= page.start, original.start < original.end,
                  original.end <= page.end,
                  original.end - original.start <= AtlasReaderLimits.maximumPageBytes,
                  wire.text.utf8.elementsEqual(request.text.utf8),
                  AtlasReaderWire.validHex(wire.original_hex, bytes: original.end - original.start),
                  !wire.contains_replacements || page.hasReplacements,
                  !wire.contains_replacements || wire.text.unicodeScalars.contains(where: { $0.value == 0xfffd }) else {
                throw AtlasReaderError.invalidResponse
            }
            // The engine's original range must also carry the exact bytes of
            // the accepted page, not a valid-looking witness from another page.
            let raw = Array(page.originalHex.utf8)
            let start = Int((original.start - page.start) * 2)
            let end = Int((original.end - page.start) * 2)
            guard raw[start..<end].elementsEqual(wire.original_hex.utf8) else {
                throw AtlasReaderError.invalidResponse
            }
            // Reverse encoding checks lossless consistency; it does not
            // interpret malformed source or recreate the shared decoder map.
            // A duplicate equal token elsewhere in a lossless page cannot
            // substitute its original offsets for the selected display range.
            if !page.hasReplacements {
                guard try AtlasReaderPosition.verifiesSourceOffset(text: page.text,
                    utf8Offset: request.utf8Start, utf16Offset: request.nativeRange.location,
                    originalHex: page.originalHex, visibleStart: page.start,
                    originalOffset: original.start, encoding: identity.encoding, hasReplacements: false),
                    try AtlasReaderPosition.verifiesSourceOffset(text: page.text,
                    utf8Offset: request.utf8End,
                    utf16Offset: request.nativeRange.location + request.nativeRange.length,
                    originalHex: page.originalHex, visibleStart: page.start,
                    originalOffset: original.end, encoding: identity.encoding, hasReplacements: false) else {
                    throw AtlasReaderError.invalidResponse
                }
            }
            let encoded: Data
            switch identity.encoding {
            case "utf8": encoded = Data(wire.text.utf8)
            case "utf16le", "utf16be":
                guard let bytes = wire.text.data(using: identity.encoding == "utf16le"
                    ? .utf16LittleEndian : .utf16BigEndian) else { throw AtlasReaderError.invalidResponse }
                encoded = bytes
            default: throw AtlasReaderError.invalidResponse
            }
            func nibble(_ byte: UInt8) -> UInt8 { byte <= 57 ? byte - 48 : byte - 87 }
            let roundTrips = encoded.count * 2 == end - start && encoded.enumerated().allSatisfy { index, byte in
                let at = start + index * 2
                return nibble(raw[at]) * 16 + nibble(raw[at + 1]) == byte
            }
            guard roundTrips == !wire.contains_replacements else { throw AtlasReaderError.invalidResponse }
            var provenance: [String: Any] = [
                "source": "retained-capture", "owner": String(identity.owner),
                "file_id": String(identity.file), "source_revision": String(identity.revision),
                "original_range": ["start": String(original.start), "end": String(original.end)],
                "path": ["encoding": "unix-bytes", "hex": identity.pathBytes.map { String(format: "%02x", $0) }.joined(),
                         "display": identity.displayPath],
                "encoding": identity.encoding, "copy_representation": "logical-unicode",
                "line_endings": "preserved", "contains_replacements": wire.contains_replacements
            ]
            if let captureSHA256 {
                guard captureSHA256.utf8.count == 64,
                      captureSHA256.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else {
                    throw AtlasReaderError.invalidResponse
                }
                provenance["capture_sha256"] = captureSHA256
            }
            let location = String(decoding: try JSONSerialization.data(withJSONObject: provenance,
                options: [.sortedKeys]), as: UTF8.self)
            return Self(id: UUID(), identity: identity, pageStart: page.start, pageEnd: page.end,
                nativeRange: request.nativeRange, utf8Start: utf8.start, utf8End: utf8.end,
                originalStart: original.start, originalEnd: original.end,
                text: wire.text, originalHex: wire.original_hex,
                containsReplacements: wire.contains_replacements,
                withLocationText: "FCB captured selection (original byte offsets)\n" + location + "\n\n" + wire.text)
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }

    private struct Wire: Decodable {
        let header: AtlasReaderWire.Header
        let selection_namespace: String
        let window_original_range, window_utf8_range, original_range: AtlasReaderWire.Range
        let text, original_hex: String
        let contains_replacements: Bool
        enum CodingKeys: String, CodingKey {
            case selection_namespace, window_original_range, window_utf8_range, original_range,
                 text, original_hex, contains_replacements
        }
        init(from decoder: Decoder) throws {
            header = try AtlasReaderWire.Header(from: decoder)
            let c = try decoder.container(keyedBy: CodingKeys.self)
            selection_namespace = try c.decode(String.self, forKey: .selection_namespace)
            window_original_range = try c.decode(AtlasReaderWire.Range.self, forKey: .window_original_range)
            window_utf8_range = try c.decode(AtlasReaderWire.Range.self, forKey: .window_utf8_range)
            original_range = try c.decode(AtlasReaderWire.Range.self, forKey: .original_range)
            text = try c.decode(String.self, forKey: .text)
            original_hex = try c.decode(String.self, forKey: .original_hex)
            contains_replacements = try c.decode(Bool.self, forKey: .contains_replacements)
        }
    }
}

struct AtlasReaderTextSelectionTransport: Sendable {
    let resolve: @Sendable (UInt64, UInt64, UInt64, UInt64, UInt64) -> String?
}
