import Foundation

/// Native admission is deliberately separate from whole-file text presentation.
/// These limits mirror the retained-reader ABI; paging is not an unbounded file
/// capture. Plain pages carry no search authority; only an explicitly bound
/// retained-file hit admits its own selection, never a workspace hit's identity.
enum AtlasReaderLimits {
    static let captureBytes: UInt64 = 4 * 1024 * 1024
    static let pageBytes: UInt64 = 64 * 1024
    static let maximumPageBytes: UInt64 = 256 * 1024
    static let responseBytes = 4 * 1024 * 1024
}

enum AtlasReaderError: Error, Sendable {
    case invalidInput, unavailable, invalidResponse, canceled
    case engine(String)

    var message: String {
        switch self {
        case .invalidInput: return "Choose a valid project file, byte offset, or one-based line number."
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

enum AtlasReaderRequest: Equatable, Sendable {
    case window(offset: UInt64, bytes: UInt64)
    case lines(first: UInt64, count: UInt64, bytes: UInt64)
    case hit(AtlasReaderHitTarget)

    static var firstPage: Self { .window(offset: 0, bytes: AtlasReaderLimits.pageBytes) }
    var command: String { switch self { case .window: return "window"; case .lines: return "lines"; case .hit: return "hit" } }
    var byteLimit: UInt64 { switch self { case .window(_, let bytes), .lines(_, _, let bytes): return bytes; case .hit: return AtlasReaderLimits.maximumPageBytes } }

    func validate(capturedBytes: UInt64) throws {
        guard (4...AtlasReaderLimits.maximumPageBytes).contains(byteLimit) else {
            throw AtlasReaderError.invalidInput
        }
        switch self {
        case .window(let offset, _):
            guard offset <= capturedBytes else { throw AtlasReaderError.invalidInput }
        case .lines(let first, let count, _):
            guard first > 0, (1...1024).contains(count) else { throw AtlasReaderError.invalidInput }
        case .hit(let target):
            try AtlasReaderFindReport.validateNeedle(target.needle)
            guard target.generation > 0, target.index < AtlasReaderFindReport.maxHits,
                  target.start < target.end, target.end <= capturedBytes,
                  target.end - target.start <= 2048,
                  AtlasReaderWire.validHex(target.originalHex, bytes: target.end - target.start) else {
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

    static func decode(_ json: String?, handle: UInt64, path: String) throws -> Self {
        let data = try AtlasReaderWire.admit(json, handle: handle)
        do {
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            let identity = try wire.header.identity(handle: handle)
            guard wire.header.command == "info", identity.pathBytes.elementsEqual(path.utf8),
                  ["regular-file-observation-not-atomic", "host-supplied"].contains(wire.capture_origin) else {
                throw AtlasReaderError.invalidResponse
            }
            return Self(identity: identity, origin: wire.capture_origin)
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }

    private struct Wire: Decodable {
        let header: AtlasReaderWire.Header
        let capture_origin: String
        init(from decoder: Decoder) throws {
            header = try .init(from: decoder)
            let c = try decoder.container(keyedBy: CodingKeys.self)
            capture_origin = try c.decode(String.self, forKey: .capture_origin)
        }
        enum CodingKeys: CodingKey { case capture_origin }
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
    let selection: AtlasReaderHitSelection?
    var nextOffset: UInt64? { end < identity.capturedBytes ? end : nil }

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
            switch request {
            case .window(let offset, let bytes):
                guard wire.selection == nil, firstLine == nil, requested.start == offset,
                      requested.end == offset + min(bytes, identity.capturedBytes - offset) else {
                    throw AtlasReaderError.invalidResponse
                }
            case .lines(let first, _, _):
                guard wire.selection == nil, firstLine == first else { throw AtlasReaderError.invalidResponse }
            case .hit(let target):
                let padding = AtlasReaderHitTarget.contextBytes + 4
                guard firstLine == nil, !wire.range_limited,
                      requested.start == target.start - min(padding, target.start),
                      requested.end == target.end + min(padding, identity.capturedBytes - target.end),
                      let selected = wire.selection,
                      AtlasReaderWire.integer(selected.query_generation) == target.generation,
                      selected.selection_namespace == nil, selected.outline_generation == nil,
                      selected.document_generation == nil else { throw AtlasReaderError.invalidResponse }
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
            }
            return Self(identity: identity, start: visible.start, end: visible.end,
                text: wire.text, originalHex: wire.original_hex, firstPhysicalLine: firstLine,
                rangeLimited: wire.range_limited, boundariesAdjusted: wire.boundaries_adjusted,
                hasReplacements: wire.has_replacements, selection: selection)
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }

    private struct Wire: Decodable {
        let header: AtlasReaderWire.Header
        let text_kind, text, original_hex: String
        let requested_original_range, visible_range: AtlasReaderWire.Range
        let range_limited, boundaries_adjusted, has_replacements: Bool
        let first_physical_line, next_offset: String?
        // Only a matching accepted hit request admits a search selection.
        // Byte/line pages and other selection namespaces cannot borrow it.
        let selection: Selection?
        struct Selection: Decodable {
            let query_generation, original_hex: String
            let original_range, window_utf8_range: AtlasReaderWire.Range
            let selection_namespace, outline_generation, document_generation: String?
        }
        enum CodingKeys: CodingKey {
            case text_kind, text, original_hex, requested_original_range, visible_range,
                 range_limited, boundaries_adjusted, has_replacements, first_physical_line, next_offset, selection
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
