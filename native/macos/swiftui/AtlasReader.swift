import Foundation

/// Native admission is deliberately separate from whole-file text presentation.
/// These limits mirror the retained-reader ABI; paging is not an unbounded file
/// capture. Plain pages carry no search authority; only an explicitly bound
/// retained-file hit admits its own selection, never a workspace hit's identity.
enum AtlasReaderLimits {
    // Live file opening retains one bounded complete observation. Optional
    // source transfers and comparisons keep their independent smaller caps.
    static let captureBytes: UInt64 = 32 * 1024 * 1024
    static let transferBytes: UInt64 = 4 * 1024 * 1024
    static let comparisonBytes: UInt64 = 4 * 1024 * 1024
    static let pageBytes: UInt64 = 64 * 1024
    static let maximumPageBytes: UInt64 = 256 * 1024
    static let responseBytes = 4 * 1024 * 1024
}

enum AtlasReaderError: Error, Sendable {
    case invalidInput, unavailable, invalidResponse, canceled
    case engine(String)

    var message: String {
        switch self {
        case .invalidInput: return "Choose a valid project file, byte offset, or one-based line and column."
        case .unavailable: return "The retained reader could not admit this source. No empty or partial capture was substituted."
        case .invalidResponse: return "The reader response could not be verified. The previous capture has not been replaced."
        case .canceled: return "Reader operation canceled. In-flight work may still be draining."
        case .engine(let code): return "Retained reader unavailable (\(code))."
        }
    }
}

/// Raw relative identity supplied by the catalog, never an escaped UI label.
/// This validates a lexical join, not a symlink-confinement or atomic-read claim.
struct AtlasReaderInput: Sendable {
    let fullPath: String

    init(root: String, path: String) throws {
        guard !root.isEmpty, !root.utf8.contains(0), root.utf8.count <= 16_384,
              !path.isEmpty, !path.hasPrefix("/"), !path.utf8.contains(0),
              path.utf8.count <= 16_384,
              !path.split(separator: "/", omittingEmptySubsequences: false).contains(where: {
                  $0.isEmpty || $0 == "." || $0 == ".."
              }) else { throw AtlasReaderError.invalidInput }
        fullPath = root + (root.hasSuffix("/") ? "" : "/") + path
        guard fullPath.utf8.count <= 16_384 else { throw AtlasReaderError.invalidInput }
    }
}

/// Logical source units, never visual columns or an inferred tab width. The
/// shared reader resolves these coordinates against its immutable capture.
enum AtlasReaderColumnKind: String, CaseIterable, Identifiable, Sendable {
    case unicodeScalar = "Unicode scalar"
    case utf16CodeUnit = "UTF-16 unit"
    case decodedUtf8Byte = "Decoded UTF-8 byte"

    var id: Self { self }
    var abi: UInt64 {
        switch self { case .unicodeScalar: 1; case .utf16CodeUnit: 2; case .decodedUtf8Byte: 3 }
    }
    var wire: String {
        switch self {
        case .unicodeScalar: "unicode_scalar"
        case .utf16CodeUnit: "utf16_code_unit"
        case .decodedUtf8Byte: "decoded_utf8_byte"
        }
    }
    var label: String {
        switch self {
        case .unicodeScalar: "Unicode scalar"
        case .utf16CodeUnit: "UTF-16 code unit"
        case .decodedUtf8Byte: "Decoded UTF-8 byte"
        }
    }
}

enum AtlasReaderRequest: Equatable, Sendable {
    case window(offset: UInt64, bytes: UInt64)
    case lines(first: UInt64, count: UInt64, bytes: UInt64)
    case position(line: UInt64, column: UInt64, kind: AtlasReaderColumnKind, bytes: UInt64)
    case hit(AtlasReaderHitTarget)
    case symbol(AtlasOutlineTarget)
    case reference(AtlasReferenceTarget)

    static var firstPage: Self { .window(offset: 0, bytes: AtlasReaderLimits.pageBytes) }
    var command: String { switch self { case .window: return "window"; case .lines: return "lines"; case .position: return "position"; case .hit: return "hit"; case .symbol: return "symbol"; case .reference: return "reference" } }
    var byteLimit: UInt64 {
        switch self {
        case .window(_, let bytes), .lines(_, _, let bytes), .position(_, _, _, let bytes): return bytes
        case .hit, .symbol, .reference: return AtlasReaderLimits.maximumPageBytes
        }
    }

    func validate(capturedBytes: UInt64) throws {
        guard (4...AtlasReaderLimits.maximumPageBytes).contains(byteLimit) else {
            throw AtlasReaderError.invalidInput
        }
        switch self {
        case .window(let offset, _):
            guard offset <= capturedBytes else { throw AtlasReaderError.invalidInput }
        case .lines(let first, let count, _):
            guard first > 0, (1...1024).contains(count) else { throw AtlasReaderError.invalidInput }
        case .position(let line, let column, _, _):
            guard line > 0, column > 0 else { throw AtlasReaderError.invalidInput }
        case .hit(let target):
            try AtlasReaderFindReport.validateMatchedText(target.needle)
            guard target.generation > 0, target.index < AtlasReaderFindReport.maxHits,
                  target.start < target.end, target.end <= capturedBytes,
                  target.end - target.start <= AtlasReaderFindReport.maximumWitnessBytes,
                  AtlasReaderWire.validHex(target.originalHex, bytes: target.end - target.start) else {
                throw AtlasReaderError.invalidInput
            }
        case .symbol(let target):
            let selected = target.symbol.selectedRange
            guard target.generation > 0, target.symbol.id > 0,
                  target.symbol.id <= AtlasOutlinePage.maxItems,
                  target.identity.capturedBytes == capturedBytes,
                  selected.start < selected.end, selected.end <= capturedBytes,
                  selected.length <= AtlasOutlinePage.sourceLimit else {
                throw AtlasReaderError.invalidInput
            }
        case .reference(let target):
            try AtlasReferencePage.validateName(target.name)
            let selected = target.occurrence.range
            guard target.generation > 0, (1...AtlasReferencePage.maxItems).contains(target.occurrence.id),
                  target.identity.capturedBytes == capturedBytes,
                  capturedBytes <= AtlasReferencePage.sourceLimit,
                  selected.start < selected.end, selected.end <= capturedBytes else {
                throw AtlasReaderError.invalidInput
            }
        }
    }
}

struct AtlasReaderIdentity: Equatable, Sendable {
    let owner: UInt64
    let file: UInt64
    let revision: UInt64
    let capturedBytes: UInt64
    let pathBytes: [UInt8]
    let encoding: String
    let displayPath: String

    /// Display labels are not authority. Canonically equivalent Swift strings
    /// still denote different Unix path byte sequences.
    func matches(_ other: Self) -> Bool {
        owner == other.owner && file == other.file && revision == other.revision
            && capturedBytes == other.capturedBytes && pathBytes == other.pathBytes
            && encoding == other.encoding
    }
}

struct AtlasReaderInfo: Sendable {
    let identity: AtlasReaderIdentity
    let origin: String
    /// Additive retained-source witness. Legacy transports may omit it; pair
    /// comparison requires witnesses for both captures before source transfer.
    let captureSHA256: String?

    static func decode(_ json: String?, handle: UInt64, path: String) throws -> Self {
        guard path.utf8.count <= 16_384 else { throw AtlasReaderError.invalidInput }
        return try decode(json, handle: handle, pathBytes: Array(path.utf8))
    }
    static func decode(_ json: String?, handle: UInt64, pathBytes: [UInt8]) throws -> Self {
        let data = try AtlasReaderWire.admit(json, handle: handle)
        do {
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            let identity = try wire.header.identity(handle: handle)
            guard wire.header.command == "info", identity.pathBytes == pathBytes,
                  ["regular-file-observation-not-atomic", "host-supplied"].contains(wire.capture_origin) else {
                throw AtlasReaderError.invalidResponse
            }
            if let digest = wire.capture_sha256 {
                guard digest.utf8.count == 64,
                      digest.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else {
                    throw AtlasReaderError.invalidResponse
                }
            }
            return Self(identity: identity, origin: wire.capture_origin, captureSHA256: wire.capture_sha256)
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }

    /// A capture-only initializer cannot inherit query authority or claim a
    /// filesystem observation. Its label is provenance, including raw controls,
    /// and is never interpreted as a path to open or refresh.
    static func decodeRetainedInitialization(_ json: String?, handle: UInt64, label: String) throws -> Self {
        guard label.utf8.count <= 16_384 else { throw AtlasReaderError.invalidInput }
        return try decodeRetainedInitialization(json, handle: handle, pathBytes: Array(label.utf8))
    }
    /// Saved archives can retain native names that are not UTF-8. The exact
    /// native bytes bind this capture; display strings never become open paths.
    static func decodeRetainedInitialization(_ json: String?, handle: UInt64, pathBytes: [UInt8]) throws -> Self {
        let info = try decode(json, handle: handle, pathBytes: pathBytes)
        guard info.origin == "host-supplied", info.captureSHA256 != nil else {
            throw AtlasReaderError.invalidResponse
        }
        let data = try AtlasReaderWire.admit(json, handle: handle)
        do { _ = try JSONDecoder().decode(RetainedInitialization.self, from: data) }
        catch { throw AtlasReaderError.invalidResponse }
        return info
    }

    private struct RetainedInitialization: Decodable {
        enum CodingKeys: CodingKey {
            case initial_source_bytes_read, initial_read_calls,
                 accepted_query_generation, accepted_outline_generation, accepted_document_generation,
                 accepted_reference_generation
        }
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            guard try c.decode(String.self, forKey: .initial_source_bytes_read) == "0",
                  try c.decode(String.self, forKey: .initial_read_calls) == "0" else {
                throw AtlasReaderError.invalidResponse
            }
            for key in [CodingKeys.accepted_query_generation, .accepted_outline_generation,
                        .accepted_document_generation] {
                guard c.contains(key), try c.decodeNil(forKey: key) else {
                    throw AtlasReaderError.invalidResponse
                }
            }
            // Older transports predate occurrences. If advertised, the new
            // namespace must explicitly start empty in a capture-only owner.
            if c.contains(.accepted_reference_generation) {
                guard try c.decodeNil(forKey: .accepted_reference_generation) else {
                    throw AtlasReaderError.invalidResponse
                }
            }
        }
    }

    private struct Wire: Decodable {
        let header: AtlasReaderWire.Header
        let capture_origin: String
        let capture_sha256: String?
        init(from decoder: Decoder) throws {
            header = try .init(from: decoder)
            let c = try decoder.container(keyedBy: CodingKeys.self)
            capture_origin = try c.decode(String.self, forKey: .capture_origin)
            capture_sha256 = try c.decodeIfPresent(String.self, forKey: .capture_sha256)
        }
        enum CodingKeys: CodingKey { case capture_origin, capture_sha256 }
    }
}

/// Styling is derived by the shared lexer from the complete retained capture,
/// then clipped by the engine to this exact decoded page. Native code never
/// starts a lexer at a page boundary or treats UTF-16 offsets as source bytes.
struct AtlasReaderHighlight: Sendable {
    static let maximumRuns = 8192

    enum State: String, Decodable, Sendable, CaseIterable {
        case highlighted, plain
        case unsupportedEncoding = "unsupported_encoding"
        case sourceLimit = "source_limit"
        case admissionDenied = "admission_denied"
        case invalidSpans = "invalid_spans"
        case pageLimit = "page_limit"
    }
    enum Role: String, Sendable, CaseIterable, Hashable {
        case plain, keyword, type, function, string, number, comment, `operator`, punctuation
    }
    struct Run: Equatable, Sendable {
        let range: NSRange
        let role: Role
    }

    let state: State
    let language: String
    let runs: [Run]

    private init(state: State, language: String, runs: [Run]) {
        self.state = state; self.language = language; self.runs = runs
    }

    var summary: String {
        switch state {
        case .highlighted: return "Syntax highlighting: \(language)."
        case .plain: return "Plain text; syntax highlighting is unavailable for this language."
        case .unsupportedEncoding: return "Plain text; syntax highlighting is unavailable for this source encoding."
        case .sourceLimit: return "Plain text; source exceeds the syntax highlighting limit."
        case .admissionDenied: return "Plain text; syntax highlighting memory is unavailable."
        case .invalidSpans: return "Plain text; syntax highlighting could not be verified."
        case .pageLimit: return "Plain text; this page exceeds the syntax highlighting detail limit."
        }
    }

    struct Wire: Decodable {
        let state: State
        let language: String
        private let runs: [Run]
        private struct Run: Decodable { let start, length, role: String }
        private enum CodingKeys: CodingKey { case state, language, coordinate_space, source_context, runs }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            state = try c.decode(State.self, forKey: .state)
            language = try c.decode(String.self, forKey: .language)
            guard !language.isEmpty, language.utf8.count <= 64,
                  language.utf8.allSatisfy({ (97...122).contains($0) || (48...57).contains($0)
                      || [35, 43, 45, 46, 95].contains($0) }),
                  try c.decode(String.self, forKey: .coordinate_space) == "window-utf16",
                  try c.decode(String.self, forKey: .source_context) == "complete-capture" else {
                throw AtlasReaderError.invalidResponse
            }
            var array = try c.nestedUnkeyedContainer(forKey: .runs)
            guard array.count.map({ $0 <= AtlasReaderHighlight.maximumRuns }) ?? true else {
                throw AtlasReaderError.invalidResponse
            }
            var admitted: [Run] = []
            admitted.reserveCapacity(array.count ?? 0)
            while !array.isAtEnd {
                guard admitted.count < AtlasReaderHighlight.maximumRuns else { throw AtlasReaderError.invalidResponse }
                admitted.append(try array.decode(Run.self))
            }
            runs = admitted
        }

        func validated(text: String, encoding: String, hasReplacements: Bool) throws -> AtlasReaderHighlight {
            guard state == .highlighted else {
                guard runs.isEmpty else { throw AtlasReaderError.invalidResponse }
                return AtlasReaderHighlight(state: state, language: language, runs: [])
            }
            guard encoding == "utf8", !hasReplacements, language != "plain" else {
                throw AtlasReaderError.invalidResponse
            }
            let source = text as NSString
            var cursor = 0
            var admitted: [AtlasReaderHighlight.Run] = []
            admitted.reserveCapacity(runs.count)
            for run in runs {
                guard let start = AtlasReaderWire.integer(run.start), start == UInt64(cursor),
                      let length = AtlasReaderWire.integer(run.length), length > 0,
                      length <= UInt64(source.length - cursor), let role = Role(rawValue: run.role) else {
                    throw AtlasReaderError.invalidResponse
                }
                let end = cursor + Int(length)
                // A token may split a composed character, but never a UTF-16
                // surrogate pair. NSString indexing keeps this check linear.
                guard end == source.length || !(0xDC00...0xDFFF).contains(source.character(at: end)) else {
                    throw AtlasReaderError.invalidResponse
                }
                admitted.append(.init(range: NSRange(location: cursor, length: Int(length)), role: role))
                cursor = end
            }
            guard cursor == source.length else { throw AtlasReaderError.invalidResponse }
            return AtlasReaderHighlight(state: state, language: language, runs: admitted)
        }
    }
}

/// An engine-resolved logical caret in this page's exact captured source. The
/// AppKit conversion below does not interpret columns or original encodings:
/// it converts the already verified window UTF-8 offset to UTF-16, once.
struct AtlasReaderPosition: Equatable, Sendable {
    enum CaretLimitation: Equatable, Sendable {
        case composedCharacter, unverifiedDecodedOffset
        var message: String {
            switch self {
            case .composedCharacter:
                "The exact logical position is inside a composed character. A native caret is unavailable there; the location has not been rounded."
            case .unverifiedDecodedOffset:
                "A native caret is unavailable because replacement text prevents verifying its exact original-byte location. The engine's logical position and original bytes are retained."
            }
        }
    }
    let line: UInt64
    let column: UInt64
    let kind: AtlasReaderColumnKind
    let originalOffset: UInt64
    let windowUtf8Offset: UInt64
    let windowUTF16Offset: Int
    let requestedBytes: UInt64
    /// A logical scalar boundary can lie inside a composed character. Keep the
    /// exact logical target, but never round the native caret to another byte.
    let nativeCaretRange: NSRange?
    let caretLimitation: CaretLimitation?

    var request: AtlasReaderRequest {
        .position(line: line, column: column, kind: kind, bytes: requestedBytes)
    }
    var summary: String {
        "Line \(line), \(kind.label.lowercased()) column \(column) · original byte \(originalOffset)"
    }

    static func caret(in text: String, atUTF8 target: UInt64) -> (offset: Int, range: NSRange?)? {
        var offset: UInt64 = 0
        var units = 0
        for byte in text.utf8 {
            if offset == target {
                guard byte & 0xC0 != 0x80 else { return nil }
                return nativeCaret(in: text, utf16: units)
            }
            if byte & 0xC0 != 0x80 { units += byte >= 0xF0 ? 2 : 1 }
            offset += 1
        }
        guard offset == target else { return nil }
        return nativeCaret(in: text, utf16: units)
    }

    private static func nativeCaret(in text: String, utf16: Int) -> (offset: Int, range: NSRange?) {
        let native = text as NSString
        let available = utf16 == 0 || utf16 == native.length
            || native.rangeOfComposedCharacterSequence(at: utf16).location == utf16
        return (utf16, available ? NSRange(location: utf16, length: 0) : nil)
    }

    /// Correlate the two wire coordinates by reverse encoding only. Logical
    /// column resolution and malformed-input decoding remain in the engine.
    /// A replacement-containing page can still prove a caret using an exact
    /// prefix or suffix; otherwise it retains a logical position without an
    /// unproved native caret. Every buffer is bounded by reader-page admission.
    static func verifiesSourceOffset(text: String, utf8Offset: UInt64, utf16Offset: Int,
        originalHex: String, visibleStart: UInt64, originalOffset: UInt64,
        encoding: String, hasReplacements: Bool) throws -> Bool {
        let encoded: Data
        let prefixBytes: Int
        switch encoding {
        case "utf8": encoded = Data(text.utf8); prefixBytes = Int(utf8Offset)
        case "utf16le", "utf16be":
            guard let bytes = text.data(using: encoding == "utf16le" ? .utf16LittleEndian : .utf16BigEndian) else {
                throw AtlasReaderError.invalidResponse
            }
            encoded = bytes; prefixBytes = utf16Offset * 2
        default: throw AtlasReaderError.invalidResponse
        }
        let hex = Array(originalHex.utf8)
        let count = hex.count / 2
        // The shared extent decoder already excludes the capture-leading BOM
        // from both visible_range and original_hex. A U+FEFF in this text is
        // ordinary content and must participate in the exact round trip.
        guard originalOffset >= visibleStart, originalOffset - visibleStart <= UInt64(count),
              prefixBytes >= 0, prefixBytes <= encoded.count else { throw AtlasReaderError.invalidResponse }
        let rawOffset = Int(originalOffset - visibleStart)
        func nibble(_ value: UInt8) -> UInt8 { value <= 57 ? value - 48 : value - 87 }
        func matches<Bytes: Collection>(_ bytes: Bytes, at start: Int) -> Bool where Bytes.Element == UInt8 {
            guard start >= 0, start <= count, bytes.count <= count - start else { return false }
            for (index, byte) in bytes.enumerated() {
                let at = (start + index) * 2
                if nibble(hex[at]) * 16 + nibble(hex[at + 1]) != byte { return false }
            }
            return true
        }
        if !hasReplacements {
            guard encoded.count == count, matches(encoded, at: 0),
                  rawOffset == prefixBytes else { throw AtlasReaderError.invalidResponse }
            return true
        }
        let prefixProvesOffset = rawOffset == prefixBytes
            && matches(encoded.prefix(prefixBytes), at: 0)
        let suffix = encoded.dropFirst(prefixBytes)
        let suffixProvesOffset = count - rawOffset == suffix.count && matches(suffix, at: rawOffset)
        return prefixProvesOffset || suffixProvesOffset
    }
}

struct AtlasReaderPage: Sendable {
    let identity: AtlasReaderIdentity
    let start: UInt64
    let end: UInt64
    let text: String
    /// Lossless original capture bytes, not the possibly replaced decoded text.
    let originalHex: String
    let firstPhysicalLine: UInt64?
    let rangeLimited: Bool
    let boundariesAdjusted: Bool
    let hasReplacements: Bool
    /// Absent in older transports. Missing metadata is an ordinary plain page.
    let highlight: AtlasReaderHighlight?
    let selection: AtlasReaderHitSelection?
    let symbolSelection: AtlasReaderSymbolSelection?
    let referenceSelection: AtlasReaderReferenceSelection?
    let position: AtlasReaderPosition?
    var nextOffset: UInt64? { end < identity.capturedBytes ? end : nil }

    init(identity: AtlasReaderIdentity, start: UInt64, end: UInt64, text: String,
         originalHex: String, firstPhysicalLine: UInt64?, rangeLimited: Bool,
         boundariesAdjusted: Bool, hasReplacements: Bool, highlight: AtlasReaderHighlight?,
         selection: AtlasReaderHitSelection?, symbolSelection: AtlasReaderSymbolSelection?,
         position: AtlasReaderPosition? = nil, referenceSelection: AtlasReaderReferenceSelection? = nil) {
        self.identity = identity; self.start = start; self.end = end; self.text = text
        self.originalHex = originalHex; self.firstPhysicalLine = firstPhysicalLine
        self.rangeLimited = rangeLimited; self.boundariesAdjusted = boundariesAdjusted
        self.hasReplacements = hasReplacements; self.highlight = highlight
        self.selection = selection; self.symbolSelection = symbolSelection; self.position = position
        self.referenceSelection = referenceSelection
    }

    static func decode(_ json: String?, info: AtlasReaderInfo, request: AtlasReaderRequest) throws -> Self {
        try request.validate(capturedBytes: info.identity.capturedBytes)
        let data = try AtlasReaderWire.admit(json, handle: info.identity.owner)
        do {
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            let identity = try wire.header.identity(handle: info.identity.owner)
            let requested = try wire.requested_original_range.values()
            let visible = try wire.visible_range.values()
            guard identity.matches(info.identity), wire.header.command == request.command,
                  wire.text_kind == "logical-captured-text-not-shaped",
                  requested.end <= identity.capturedBytes, visible.end <= identity.capturedBytes,
                  visible.end - visible.start <= request.byteLimit + 16,
                  wire.text.utf8.count <= Int((request.byteLimit + 16) * 3),
                  AtlasReaderWire.validHex(wire.original_hex, bytes: visible.end - visible.start) else {
                throw AtlasReaderError.invalidResponse
            }
            // The engine admits eight context bytes on each side for scalar and
            // CRLF boundaries. They cannot authorize an arbitrary wider page.
            let end = min(requested.end, requested.start + min(request.byteLimit, identity.capturedBytes - requested.start))
            guard visible.start >= requested.start - min(8, requested.start),
                  visible.start <= min(identity.capturedBytes, requested.start + 8),
                  visible.end >= end - min(8, end),
                  visible.end <= min(identity.capturedBytes, end + 8),
                  wire.range_limited == (end < requested.end),
                  wire.boundaries_adjusted == (visible.start != requested.start || visible.end != end),
                  visible.start < visible.end || visible.end == identity.capturedBytes,
                  visible.end > requested.start || requested.start == identity.capturedBytes else {
                throw AtlasReaderError.invalidResponse
            }
            if let next = wire.next_offset {
                guard let value = AtlasReaderWire.integer(next), value == visible.end else {
                    throw AtlasReaderError.invalidResponse
                }
            }
            let firstLine = try wire.first_physical_line.map { text in
                guard let value = AtlasReaderWire.integer(text), value > 0 else { throw AtlasReaderError.invalidResponse }
                return value
            }
            var selection: AtlasReaderHitSelection?
            var symbolSelection: AtlasReaderSymbolSelection?
            var referenceSelection: AtlasReaderReferenceSelection?
            var position: AtlasReaderPosition?
            switch request {
            case .window(let offset, let bytes):
                guard wire.selection == nil, wire.position == nil, firstLine == nil, requested.start == offset,
                      requested.end == offset + min(bytes, identity.capturedBytes - offset) else {
                    throw AtlasReaderError.invalidResponse
                }
            case .lines(let first, _, _):
                guard wire.selection == nil, wire.position == nil, firstLine == first else { throw AtlasReaderError.invalidResponse }
            case .position(let line, let column, let kind, let bytes):
                guard wire.selectionIsNull, firstLine == nil, !wire.range_limited,
                      identity.encoding != "unsupported", let resolved = wire.position,
                      AtlasReaderWire.integer(resolved.line) == line,
                      AtlasReaderWire.integer(resolved.column) == column,
                      resolved.column_kind == kind.wire,
                      let originalOffset = AtlasReaderWire.integer(resolved.original_offset),
                      originalOffset >= visible.start, originalOffset <= visible.end,
                      let utf8Offset = AtlasReaderWire.integer(resolved.window_utf8_offset),
                      let caret = AtlasReaderPosition.caret(in: wire.text, atUTF8: utf8Offset) else {
                    throw AtlasReaderError.invalidResponse
                }
                // Reserve four bytes after the target, even in the minimum
                // window, then permit only the existing scalar/CRLF adjustment.
                let before = (bytes - 4) / 2
                let start = originalOffset - min(originalOffset, before)
                guard requested.start == start,
                      requested.end == start + min(bytes, identity.capturedBytes - start) else {
                    throw AtlasReaderError.invalidResponse
                }
                let verified = try AtlasReaderPosition.verifiesSourceOffset(text: wire.text,
                    utf8Offset: utf8Offset, utf16Offset: caret.offset,
                    originalHex: wire.original_hex, visibleStart: visible.start, originalOffset: originalOffset,
                    encoding: identity.encoding, hasReplacements: wire.has_replacements)
                let limitation: AtlasReaderPosition.CaretLimitation? = !verified ? .unverifiedDecodedOffset
                    : (caret.range == nil ? .composedCharacter : nil)
                position = AtlasReaderPosition(line: line, column: column, kind: kind,
                    originalOffset: originalOffset, windowUtf8Offset: utf8Offset,
                    windowUTF16Offset: caret.offset, requestedBytes: bytes,
                    nativeCaretRange: verified ? caret.range : nil, caretLimitation: limitation)
            case .hit(let target):
                let padding = AtlasReaderHitTarget.contextBytes + 4
                guard wire.position == nil, firstLine == nil, !wire.range_limited,
                      requested.start == target.start - min(padding, target.start),
                      requested.end == target.end + min(padding, identity.capturedBytes - target.end),
                      let selected = wire.selection,
                      selected.query_generation.flatMap(AtlasReaderWire.integer) == target.generation,
                      selected.selection_namespace == nil, selected.outline_generation == nil,
                      selected.document_generation == nil, selected.symbol_id == nil,
                      selected.reference_generation == nil, selected.reference_id == nil,
                      selected.reference_line == nil, selected.compiler_resolved == nil,
                      selected.includes_comments_and_literals == nil,
                      selected.declaration_line == nil, selected.evidence_level == nil,
                      selected.semantic_resolution == nil else { throw AtlasReaderError.invalidResponse }
                let raw = try selected.original_range.values()
                let utf8 = try selected.window_utf8_range.values()
                guard raw.start == target.start, raw.end == target.end,
                      raw.start >= visible.start, raw.end <= visible.end,
                      selected.original_hex == target.originalHex,
                      utf8.start < utf8.end, utf8.end <= UInt64(wire.text.utf8.count) else {
                    throw AtlasReaderError.invalidResponse
                }
                let sourceHex = Array(wire.original_hex.utf8)
                let hexStart = Int((raw.start - visible.start) * 2)
                let hexEnd = Int((raw.end - visible.start) * 2)
                let text = Array(wire.text.utf8)
                guard sourceHex[hexStart..<hexEnd].elementsEqual(target.originalHex.utf8),
                      text[Int(utf8.start)..<Int(utf8.end)].elementsEqual(target.needle.utf8) else {
                    throw AtlasReaderError.invalidResponse
                }
                selection = AtlasReaderHitSelection(target: target, utf8Start: utf8.start, utf8End: utf8.end)
            case .symbol(let target):
                let expected = target.symbol.selectedRange
                let padding = AtlasOutlineTarget.contextBytes + 4
                guard wire.position == nil, target.identity.matches(identity), firstLine == nil, !wire.range_limited,
                      requested.start == expected.start - min(padding, expected.start),
                      requested.end == expected.end + min(padding, identity.capturedBytes - expected.end),
                      let selected = wire.selection, selected.query_generation == nil,
                      selected.document_generation == nil, selected.selection_namespace == "outline",
                      selected.reference_generation == nil, selected.reference_id == nil,
                      selected.reference_line == nil, selected.compiler_resolved == nil,
                      selected.includes_comments_and_literals == nil,
                      selected.outline_generation.flatMap(AtlasReaderWire.integer) == target.generation,
                      selected.symbol_id.flatMap(AtlasReaderWire.integer) == target.symbol.id,
                      selected.declaration_line.flatMap(AtlasReaderWire.integer) == target.symbol.line,
                      selected.evidence_level == "heuristic-outline-candidate",
                      selected.semantic_resolution == false else { throw AtlasReaderError.invalidResponse }
                let raw = try selected.original_range.admit(length: identity.capturedBytes)
                let utf8 = try selected.window_utf8_range.admit(length: UInt64(wire.text.utf8.count))
                guard raw == expected, raw.start >= visible.start, raw.end <= visible.end,
                      AtlasReaderWire.validHex(selected.original_hex, bytes: raw.length) else {
                    throw AtlasReaderError.invalidResponse
                }
                let sourceHex = Array(wire.original_hex.utf8)
                let hexStart = Int((raw.start - visible.start) * 2)
                let hexEnd = Int((raw.end - visible.start) * 2)
                let decoded = Array(wire.text.utf8)[Int(utf8.start)..<Int(utf8.end)]
                guard sourceHex[hexStart..<hexEnd].elementsEqual(selected.original_hex.utf8),
                      String(bytes: decoded, encoding: .utf8) != nil,
                      target.symbol.nameRange == nil || decoded.elementsEqual(target.symbol.name.utf8) else {
                    throw AtlasReaderError.invalidResponse
                }
                symbolSelection = AtlasReaderSymbolSelection(target: target, utf8Start: utf8.start,
                    utf8End: utf8.end, originalHex: selected.original_hex)
            case .reference(let target):
                let expected = target.occurrence.range
                let padding = AtlasReferenceTarget.contextBytes + 4
                guard wire.position == nil, target.identity.matches(identity), firstLine == nil, !wire.range_limited,
                      !wire.has_replacements,
                      requested.start == expected.start - min(padding, expected.start),
                      requested.end == expected.end + min(padding, identity.capturedBytes - expected.end),
                      let selected = wire.selection, selected.query_generation == nil,
                      selected.outline_generation == nil, selected.document_generation == nil,
                      selected.symbol_id == nil, selected.declaration_line == nil,
                      selected.semantic_resolution == nil, selected.selection_namespace == "reference",
                      selected.reference_generation.flatMap(AtlasReaderWire.integer) == target.generation,
                      selected.reference_id.flatMap(AtlasReaderWire.integer) == target.occurrence.id,
                      selected.reference_line.flatMap(AtlasReaderWire.integer) == target.occurrence.line,
                      selected.evidence_level == "whole-token-text-candidate",
                      selected.includes_comments_and_literals == true, selected.compiler_resolved == false else {
                    throw AtlasReaderError.invalidResponse
                }
                let raw = try selected.original_range.admit(length: identity.capturedBytes)
                let utf8 = try selected.window_utf8_range.admit(length: UInt64(wire.text.utf8.count))
                guard raw == expected, raw.start >= visible.start, raw.end <= visible.end,
                      AtlasReaderWire.validHex(selected.original_hex, bytes: raw.length) else {
                    throw AtlasReaderError.invalidResponse
                }
                let hexStart = Int((raw.start - visible.start) * 2)
                let hexEnd = Int((raw.end - visible.start) * 2)
                let sourceHex = Array(wire.original_hex.utf8)
                let decoded = Array(wire.text.utf8)[Int(utf8.start)..<Int(utf8.end)]
                guard sourceHex[hexStart..<hexEnd].elementsEqual(selected.original_hex.utf8),
                      decoded.elementsEqual(target.name.utf8),
                      let start = AtlasReaderPosition.caret(in: wire.text, atUTF8: utf8.start),
                      let end = AtlasReaderPosition.caret(in: wire.text, atUTF8: utf8.end),
                      try AtlasReaderPosition.verifiesSourceOffset(text: wire.text,
                        utf8Offset: utf8.start, utf16Offset: start.offset,
                        originalHex: wire.original_hex, visibleStart: visible.start, originalOffset: raw.start,
                        encoding: identity.encoding, hasReplacements: false),
                      try AtlasReaderPosition.verifiesSourceOffset(text: wire.text,
                        utf8Offset: utf8.end, utf16Offset: end.offset,
                        originalHex: wire.original_hex, visibleStart: visible.start, originalOffset: raw.end,
                        encoding: identity.encoding, hasReplacements: false) else {
                    throw AtlasReaderError.invalidResponse
                }
                // Equal names can appear repeatedly in one page. Reverse
                // encoding binds the selected decoded slice to these exact
                // original boundaries, rather than another equal occurrence.
                referenceSelection = AtlasReaderReferenceSelection(target: target,
                    utf8Start: utf8.start, utf8End: utf8.end, originalHex: selected.original_hex)
            }
            let highlight = try wire.highlight?.validated(text: wire.text, encoding: identity.encoding,
                                                         hasReplacements: wire.has_replacements)
            return Self(identity: identity, start: visible.start, end: visible.end,
                text: wire.text, originalHex: wire.original_hex, firstPhysicalLine: firstLine,
                rangeLimited: wire.range_limited, boundariesAdjusted: wire.boundaries_adjusted,
                hasReplacements: wire.has_replacements, highlight: highlight,
                selection: selection, symbolSelection: symbolSelection, position: position,
                referenceSelection: referenceSelection)
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }

    private struct Wire: Decodable {
        let header: AtlasReaderWire.Header
        let text_kind, text, original_hex: String
        let requested_original_range, visible_range: AtlasReaderWire.Range
        let range_limited, boundaries_adjusted, has_replacements: Bool
        let first_physical_line, next_offset: String?
        let highlight: AtlasReaderHighlight.Wire?
        // A matching accepted search, outline or occurrence request admits its
        // own namespace. Byte/line pages cannot borrow selection authority.
        let selection: Selection?
        let selectionIsNull: Bool
        let position: Position?
        struct Position: Decodable {
            let line, column, column_kind, original_offset, window_utf8_offset: String
        }
        struct Selection: Decodable {
            let original_hex: String
            let query_generation: String?
            let original_range, window_utf8_range: AtlasReaderWire.Range
            let selection_namespace, outline_generation, document_generation: String?
            let symbol_id, declaration_line, evidence_level: String?
            let semantic_resolution: Bool?
            let reference_generation, reference_id, reference_line: String?
            let includes_comments_and_literals, compiler_resolved: Bool?
        }
        enum CodingKeys: CodingKey {
            case text_kind, text, original_hex, requested_original_range, visible_range,
                 range_limited, boundaries_adjusted, has_replacements, first_physical_line, next_offset, selection, highlight, position
        }
        init(from decoder: Decoder) throws {
            header = try .init(from: decoder)
            let c = try decoder.container(keyedBy: CodingKeys.self)
            text_kind = try c.decode(String.self, forKey: .text_kind)
            text = try c.decode(String.self, forKey: .text)
            original_hex = try c.decode(String.self, forKey: .original_hex)
            requested_original_range = try c.decode(AtlasReaderWire.Range.self, forKey: .requested_original_range)
            visible_range = try c.decode(AtlasReaderWire.Range.self, forKey: .visible_range)
            range_limited = try c.decode(Bool.self, forKey: .range_limited)
            boundaries_adjusted = try c.decode(Bool.self, forKey: .boundaries_adjusted)
            has_replacements = try c.decode(Bool.self, forKey: .has_replacements)
            first_physical_line = try c.decodeIfPresent(String.self, forKey: .first_physical_line)
            next_offset = try c.decodeIfPresent(String.self, forKey: .next_offset)
            selection = try c.decodeIfPresent(Selection.self, forKey: .selection)
            if c.contains(.selection) { selectionIsNull = try c.decodeNil(forKey: .selection) }
            else { selectionIsNull = false }
            position = try c.decodeIfPresent(Position.self, forKey: .position)
            highlight = try c.decodeIfPresent(AtlasReaderHighlight.Wire.self, forKey: .highlight)
        }
    }
}

/// Shared bounded marshaling for source pages and retained-file queries.
enum AtlasReaderWire {
    static func integer(_ text: String) -> UInt64? {
        guard let value = UInt64(text), String(value) == text else { return nil }
        return value
    }
    static func validHex(_ text: String, bytes: UInt64) -> Bool {
        bytes <= AtlasReaderLimits.maximumPageBytes + 16 && text.utf8.count == Int(bytes * 2)
            && text.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
    static func pathBytes(_ text: String) throws -> [UInt8] {
        guard text.utf8.count <= 32_768, text.utf8.count.isMultiple(of: 2),
              text.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else {
            throw AtlasReaderError.invalidResponse
        }
        let input = Array(text.utf8)
        func nibble(_ b: UInt8) -> UInt8 { b <= 57 ? b - 48 : b - 87 }
        return stride(from: 0, to: input.count, by: 2).map { nibble(input[$0]) * 16 + nibble(input[$0 + 1]) }
    }
    static func admit(_ json: String?, handle: UInt64) throws -> Data {
        guard let json else { throw AtlasReaderError.unavailable }
        guard json.utf8.count <= AtlasReaderLimits.responseBytes, let data = json.data(using: .utf8) else {
            throw AtlasReaderError.invalidResponse
        }
        struct Status: Decodable {
            let schema, status, owner: String
            let error: Diagnostic?
            struct Diagnostic: Decodable { let code: String }
        }
        guard let status = try? JSONDecoder().decode(Status.self, from: data),
              status.schema == "fcb.reader-session/1", integer(status.owner) == handle, handle > 0 else {
            throw AtlasReaderError.invalidResponse
        }
        if status.status == "error" {
            guard let code = status.error?.code, !code.isEmpty, code.utf8.count <= 512,
                  code.unicodeScalars.allSatisfy({ $0.value >= 32 && $0.value != 127 }) else {
                throw AtlasReaderError.invalidResponse
            }
            throw AtlasReaderError.engine(code)
        }
        guard status.status == "ok", status.error == nil else { throw AtlasReaderError.invalidResponse }
        return data
    }
    struct Range: Decodable {
        let start, end: String
        func values() throws -> (start: UInt64, end: UInt64) {
            guard let start = integer(start), let end = integer(end), start <= end else {
                throw AtlasReaderError.invalidResponse
            }
            return (start, end)
        }
    }
    struct Header: Decodable {
        let command, owner, file_id, source_revision, captured_bytes, encoding, additional_source_bytes_read: String
        let native_presented: Bool
        let path: NativePath
        struct NativePath: Decodable { let encoding, hex, display: String }
        func identity(handle: UInt64) throws -> AtlasReaderIdentity {
            guard integer(owner) == handle, let file = integer(file_id), file > 0,
                  let revision = integer(source_revision), revision > 0,
                  let count = integer(captured_bytes), count <= AtlasReaderLimits.captureBytes,
                  additional_source_bytes_read == "0", !native_presented,
                  ["utf8", "utf16le", "utf16be", "unsupported"].contains(encoding),
                  path.encoding == "unix-bytes", path.display.utf8.count <= 65_536 else {
                throw AtlasReaderError.invalidResponse
            }
            let raw = try pathBytes(path.hex)
            guard !raw.isEmpty, !raw.contains(0) else { throw AtlasReaderError.invalidResponse }
            return AtlasReaderIdentity(owner: handle, file: file, revision: revision,
                capturedBytes: count, pathBytes: raw, encoding: encoding, displayPath: path.display)
        }
    }
}
