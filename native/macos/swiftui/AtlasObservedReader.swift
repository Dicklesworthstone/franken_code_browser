import Foundation

/// A live, explicitly chosen file is observed in bounded independent ranges.
/// These values never enter the complete-capture reader, search or desk APIs.
enum AtlasObservedReaderLimits {
    static let pageBytes: UInt64 = 64 * 1024
    static let maximumPageBytes: UInt64 = 256 * 1024
    static let contextBytes: UInt64 = 8
    static let maximumPathBytes = 16_384
    static let retainedObservations = 8
    static let maximumReadCalls: UInt64 = 4096
    static let responseBytes = 8 * 1024 * 1024
}

enum AtlasObservedReaderError: Error, Sendable {
    case invalidInput, unavailable, invalidResponse, canceled
    case engine(String)
    var message: String {
        switch self {
        case .invalidInput: return "Choose a current project file and a valid original-byte offset."
        case .unavailable: return "The observed range reader is unavailable. Close another range reader and try again."
        case .invalidResponse: return "The observed range response could not be verified. The accepted range is unchanged."
        case .canceled: return "Range reading canceled. Entered work may still be draining."
        case .engine(let code): return "The requested observation is unavailable (\(code)). The accepted range is unchanged."
        }
    }
}

enum AtlasObservedReaderEncoding: String, CaseIterable, Identifiable, Sendable {
    case utf8, utf16le, utf16be
    var id: Self { self }
    var abi: UInt64 { switch self { case .utf8: 1; case .utf16le: 2; case .utf16be: 3 } }
    var label: String { switch self { case .utf8: "UTF-8"; case .utf16le: "UTF-16 LE"; case .utf16be: "UTF-16 BE" } }
}

/// This is a lexical request, not a filesystem grant. The App separately binds
/// it to the current project and retains that project's native access lease.
struct AtlasObservedReaderInput: Equatable, Sendable {
    let root: String
    let relativePath: [UInt8]
    let absolutePath: [UInt8]
    let relativeHex: String

    init(root: String, relativePath: [UInt8]) throws {
        let rootBytes = Array(root.utf8)
        guard rootBytes.first == 47, rootBytes.count <= AtlasObservedReaderLimits.maximumPathBytes,
              !rootBytes.contains(0), rootBytes == [47] || !rootBytes.dropFirst().split(separator: 47,
                omittingEmptySubsequences: false).contains(where: {
                    $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
                }), !relativePath.isEmpty, relativePath.first != 47,
              relativePath.count <= AtlasObservedReaderLimits.maximumPathBytes, !relativePath.contains(0),
              relativePath.split(separator: 47, omittingEmptySubsequences: false).count <= 256,
              !relativePath.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                  $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
              }) else { throw AtlasObservedReaderError.invalidInput }
        let absolute = rootBytes + (rootBytes.last == 47 ? [] : [47]) + relativePath
        guard absolute.count <= AtlasObservedReaderLimits.maximumPathBytes else {
            throw AtlasObservedReaderError.invalidInput
        }
        self.root = root; self.relativePath = relativePath; absolutePath = absolute
        relativeHex = AtlasObservedReaderWire.hex(relativePath)
    }

    static func == (a: Self, b: Self) -> Bool {
        a.root.utf8.elementsEqual(b.root.utf8) && a.relativePath == b.relativePath
    }

    var displayPath: String {
        String(bytes: relativePath, encoding: .utf8) ?? "Native path (hex): " + relativeHex
    }
}

struct AtlasObservedReaderRequest: Equatable, Sendable {
    let offset, bytes: UInt64
    let encoding: AtlasObservedReaderEncoding
    init(offset: UInt64, bytes: UInt64 = AtlasObservedReaderLimits.pageBytes,
         encoding: AtlasObservedReaderEncoding = .utf8) throws {
        guard (4...AtlasObservedReaderLimits.maximumPageBytes).contains(bytes),
              !offset.addingReportingOverflow(bytes).overflow else { throw AtlasObservedReaderError.invalidInput }
        self.offset = offset; self.bytes = bytes; self.encoding = encoding
    }
}

struct AtlasObservedByteRange: Equatable, Sendable, Decodable {
    let start, end: UInt64
    var count: UInt64 { end - start }
    init(start: UInt64, end: UInt64) throws {
        guard start <= end else { throw AtlasObservedReaderError.invalidResponse }
        self.start = start; self.end = end
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        try self.init(start: c.decode(AtlasObservedReaderWire.Number.self, forKey: .start).value,
            end: c.decode(AtlasObservedReaderWire.Number.self, forKey: .end).value)
    }
    func contains(_ other: Self) -> Bool { start <= other.start && other.end <= end }
    private enum CodingKeys: CodingKey { case start, end }
}

struct AtlasObservedReaderIdentity: Equatable, Sendable {
    let owner, file: UInt64
    let path: [UInt8]
    let displayPath: String
    static func == (a: Self, b: Self) -> Bool { a.owner == b.owner && a.file == b.file && a.path == b.path }
}

struct AtlasObservedReaderInfo: Sendable {
    let identity: AtlasObservedReaderIdentity
    let initialLength, sourceBytesRead, readCalls, retainedBytes: UInt64
    let retained: [UInt64]

    static func decode(_ json: String?, handle: UInt64, input: AtlasObservedReaderInput,
                       initial: Bool = false) throws -> Self {
        let data = try AtlasObservedReaderWire.admit(json, handle: handle)
        do {
            let header = try JSONDecoder().decode(AtlasObservedReaderWire.Header.self, from: data)
            let identity = try header.identity(handle: handle, input: input, command: "info")
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            let ids = wire.retained_observations.map(\.value)
            guard wire.cache_capacity.value == UInt64(AtlasObservedReaderLimits.retainedObservations),
                  ids.count <= AtlasObservedReaderLimits.retainedObservations,
                  ids.allSatisfy({ $0 > 0 }), zip(ids, ids.dropFirst()).allSatisfy({ $0.0 < $0.1 }),
                  wire.retained_payload_bytes.value <= UInt64(ids.count)
                    * (AtlasObservedReaderLimits.maximumPageBytes + 2 * AtlasObservedReaderLimits.contextBytes),
                  wire.retained_payload_bytes.value <= wire.payload_bytes_read.value,
                  !initial || (ids.isEmpty && wire.payload_bytes_read.value == 0 && wire.read_calls.value == 0) else {
                throw AtlasObservedReaderError.invalidResponse
            }
            return Self(identity: identity, initialLength: wire.initial_observed_length.value,
                sourceBytesRead: wire.payload_bytes_read.value, readCalls: wire.read_calls.value,
                retainedBytes: wire.retained_payload_bytes.value, retained: ids)
        } catch { throw AtlasObservedReaderError.invalidResponse }
    }
    private struct Wire: Decodable {
        let initial_observed_length, payload_bytes_read, read_calls, cache_capacity, retained_payload_bytes: AtlasObservedReaderWire.Number
        let retained_observations: [AtlasObservedReaderWire.Number]
    }
}

/// The digest identifies only capturedRange, including bounded decode context.
/// A different observation is never another page of one immutable whole file.
struct AtlasObservedReaderObservation: Equatable, Sendable {
    let identity: AtlasObservedReaderIdentity
    let id: UInt64
    let request: AtlasObservedReaderRequest
    let requestedVisible, requestedCapture: AtlasObservedByteRange
    let observedLength: UInt64
    let finalObservedLength: UInt64?
    let capturedRange: AtlasObservedByteRange
    let extentSHA256, consistency: String
    let captureFilled, wholeFileObserved: Bool
    let sourceBytesRead, readCalls: UInt64
}

struct AtlasObservedReaderPage: Sendable {
    let observation: AtlasObservedReaderObservation
    let visibleRange: AtlasObservedByteRange
    let boundariesAdjusted: Bool
    let firstPhysicalLine: UInt64?
    let text, originalHex: String
    let hasReplacements: Bool
    let nextOffset: UInt64?
    let findHit: AtlasObservedReaderHitNavigation?

    var verifiedSearchSelection: NSRange? {
        guard let findHit else { return nil }
        return AtlasSource(path: observation.identity.displayPath, text: text)
            .utf16Range(byteStart: findHit.decodedRange.start, byteEnd: findHit.decodedRange.end)
    }

    func matches(_ other: Self) -> Bool {
        observation == other.observation && visibleRange == other.visibleRange
            && boundariesAdjusted == other.boundariesAdjusted && firstPhysicalLine == other.firstPhysicalLine
            && text.utf8.elementsEqual(other.text.utf8) && originalHex == other.originalHex
            && hasReplacements == other.hasReplacements && nextOffset == other.nextOffset
            && findHit == other.findHit
    }

    static func decode(_ json: String?, handle: UInt64, input: AtlasObservedReaderInput,
                       request: AtlasObservedReaderRequest? = nil, retained: Self? = nil,
                       hit: AtlasObservedReaderSearchWitness? = nil) throws -> Self {
        guard (request == nil) != (retained == nil), hit == nil || request != nil else {
            throw AtlasObservedReaderError.invalidInput
        }
        let data = try AtlasObservedReaderWire.admit(json, handle: handle)
        do {
            let header = try JSONDecoder().decode(AtlasObservedReaderWire.Header.self, from: data)
            let identity = try header.identity(handle: handle, input: input,
                command: retained != nil ? "retained-window" : (hit != nil ? "observe-hit" : "window"))
            let basis = try JSONDecoder().decode(AtlasObservedReaderWire.Observation.self, from: data)
            let observation = try basis.value(identity: identity, reads: retained == nil)
            let w = try JSONDecoder().decode(Wire.self, from: data)
            let first = w.first_physical_line?.value, next = w.next_offset?.value
            guard request == nil || observation.request == request,
                  w.text_kind == "logical-observed-text-not-shaped",
                  observation.capturedRange.contains(w.visible_range),
                  w.visible_range.end <= observation.requestedVisible.end,
                  w.visible_range.count <= observation.request.bytes,
                  w.boundaries_adjusted == (w.visible_range != observation.requestedVisible),
                  w.text.utf8.count <= Int(AtlasObservedReaderLimits.maximumPageBytes) * 4,
                  AtlasObservedReaderWire.validHex(w.original_hex, bytes: w.visible_range.count),
                  first == nil || (w.visible_range.count > 0 && w.visible_range.start <= 4
                    && observation.capturedRange.start == 0 && first! > 0
                    && first! <= w.visible_range.start + 1),
                  next == (w.visible_range.end < observation.observedLength ? w.visible_range.end : nil),
                  next == nil || next! > observation.request.offset else {
                throw AtlasObservedReaderError.invalidResponse
            }
            let page = Self(observation: observation, visibleRange: w.visible_range,
                boundariesAdjusted: w.boundaries_adjusted, firstPhysicalLine: first,
                text: w.text, originalHex: w.original_hex, hasReplacements: w.has_replacements, nextOffset: next,
                findHit: w.find_hit)
            if let hit { try hit.validateVerifiedContext(page) }
            else if retained == nil, w.find_hit != nil { throw AtlasObservedReaderError.invalidResponse }
            guard retained == nil || page.matches(retained!) else { throw AtlasObservedReaderError.invalidResponse }
            return page
        } catch { throw AtlasObservedReaderError.invalidResponse }
    }
    private struct Wire: Decodable {
        let text_kind, text, original_hex: String
        let visible_range: AtlasObservedByteRange
        let boundaries_adjusted, has_replacements: Bool
        let first_physical_line, next_offset: AtlasObservedReaderWire.Number?
        let find_hit: AtlasObservedReaderHitNavigation?
    }
}

struct AtlasObservedReaderCopy: Sendable {
    let originalHex: String
    static func decode(_ json: String?, handle: UInt64, input: AtlasObservedReaderInput,
                       page: AtlasObservedReaderPage) throws -> Self {
        let data = try AtlasObservedReaderWire.admit(json, handle: handle)
        do {
            let header = try JSONDecoder().decode(AtlasObservedReaderWire.Header.self, from: data)
            let identity = try header.identity(handle: handle, input: input, command: "copy-range")
            let basis = try JSONDecoder().decode(AtlasObservedReaderWire.Observation.self, from: data)
            let observation = try basis.value(identity: identity, reads: false)
            let w = try JSONDecoder().decode(Wire.self, from: data)
            guard observation == page.observation, w.copy_domain == "original-bytes",
                  w.original_range == page.visibleRange, w.original_hex == page.originalHex,
                  AtlasObservedReaderWire.validHex(w.original_hex, bytes: w.original_range.count) else {
                throw AtlasObservedReaderError.invalidResponse
            }
            return Self(originalHex: w.original_hex)
        } catch { throw AtlasObservedReaderError.invalidResponse }
    }
    private struct Wire: Decodable {
        let copy_domain, original_hex: String
        let original_range: AtlasObservedByteRange
    }
}

struct AtlasObservedReaderTransport: Sendable {
    let create: @Sendable () -> UInt64
    let open: @Sendable (UInt64, AtlasObservedReaderInput, AtlasSearchCancellation) throws -> String?
    let info: @Sendable (UInt64) throws -> String?
    let window: @Sendable (UInt64, AtlasObservedReaderRequest, AtlasSearchCancellation) throws -> String?
    let retained: @Sendable (UInt64, UInt64) throws -> String?
    let copy: @Sendable (UInt64, UInt64, UInt64, UInt64) throws -> String?
    let cancel: @Sendable (UInt64) -> Bool
    let close: @Sendable (UInt64) -> Bool
    let retirementFailed: @Sendable () -> Void
    var search: AtlasObservedReaderSearchTransport? = nil
}

// Shared by the bounded range and search receipts. Neither admits a complete
// capture identity or grants authority to follow a pathname again.
enum AtlasObservedReaderWire {
    struct Number: Decodable {
        let value: UInt64
        init(from decoder: Decoder) throws {
            let string = try decoder.singleValueContainer().decode(String.self)
            guard let value = UInt64(string), String(value) == string else { throw AtlasObservedReaderError.invalidResponse }
            self.value = value
        }
    }
    static func hex(_ bytes: [UInt8]) -> String { bytes.map { String(format: "%02x", $0) }.joined() }
    static func validHex(_ value: String, bytes: UInt64) -> Bool {
        guard bytes <= UInt64(AtlasObservedReaderLimits.responseBytes / 2), value.utf8.count == Int(bytes) * 2 else { return false }
        return value.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
    static func admit(_ json: String?, handle: UInt64) throws -> Data {
        guard let json, handle > 0, json.utf8.count <= AtlasObservedReaderLimits.responseBytes,
              let data = json.data(using: .utf8) else { throw AtlasObservedReaderError.invalidResponse }
        struct Status: Decodable {
            let schema, status: String
            let owner: Number
            let error: Failure?
            struct Failure: Decodable { let code: String }
        }
        guard let status = try? JSONDecoder().decode(Status.self, from: data),
              status.schema == "fcb.observed-reader/1", status.owner.value == handle else {
            throw AtlasObservedReaderError.invalidResponse
        }
        if status.status == "error" {
            guard let code = status.error?.code, !code.isEmpty, code.utf8.count <= 512,
                  code.unicodeScalars.allSatisfy({ $0.value >= 32 && $0.value != 127 }) else {
                throw AtlasObservedReaderError.invalidResponse
            }
            throw AtlasObservedReaderError.engine(code)
        }
        guard status.status == "ok", status.error == nil else { throw AtlasObservedReaderError.invalidResponse }
        return data
    }
    struct Header: Decodable {
        let command, capture_kind, source_observation: String
        let owner, file_id: Number
        let native_presented, whole_file_immutable: Bool
        let path: Path
        let capture_sha256, whole_file_sha256, source_revision: String?
        struct Path: Decodable { let encoding, hex, display: String }
        func identity(handle: UInt64, input: AtlasObservedReaderInput, command expected: String) throws -> AtlasObservedReaderIdentity {
            guard command == expected, owner.value == handle, file_id.value == 1,
                  capture_kind == "observed-extent", source_observation == "open-file-ranges-not-atomic",
                  !native_presented, !whole_file_immutable, capture_sha256 == nil,
                  whole_file_sha256 == nil, source_revision == nil, path.encoding == "unix-bytes",
                  path.hex == AtlasObservedReaderWire.hex(input.absolutePath), path.display.utf8.count <= 65_536 else {
                throw AtlasObservedReaderError.invalidResponse
            }
            return AtlasObservedReaderIdentity(owner: handle, file: file_id.value,
                path: input.absolutePath, displayPath: path.display)
        }
    }
    struct Observation: Decodable {
        let observation, requested_offset, requested_bytes, observed_length, captured_bytes: Number
        let final_observed_length: Number?
        let observation_source_bytes_read, observation_read_calls, additional_source_bytes_read, additional_read_calls: Number
        let encoding, encoding_basis, extent_sha256, consistency: String
        let requested_visible_range, requested_capture_range, captured_range: AtlasObservedByteRange
        let requested_capture_filled, whole_file_observed: Bool
        func value(identity: AtlasObservedReaderIdentity, reads: Bool) throws -> AtlasObservedReaderObservation {
            guard let kind = AtlasObservedReaderEncoding(rawValue: encoding) else { throw AtlasObservedReaderError.invalidResponse }
            let request = try AtlasObservedReaderRequest(offset: requested_offset.value, bytes: requested_bytes.value, encoding: kind)
            let length = observed_length.value
            guard observation.value > 0, encoding_basis == "explicit-for-this-observation",
                  request.offset <= length, requested_visible_range.start == request.offset,
                  requested_visible_range.end == min(length, request.offset + request.bytes),
                  requested_capture_range.contains(requested_visible_range), requested_capture_range.end <= length,
                  requested_visible_range.start - requested_capture_range.start <= AtlasObservedReaderLimits.contextBytes,
                  requested_capture_range.end - requested_visible_range.end <= AtlasObservedReaderLimits.contextBytes,
                  requested_capture_range.contains(captured_range), captured_range.start == requested_capture_range.start,
                  captured_bytes.value == captured_range.count,
                  captured_range.count <= AtlasObservedReaderLimits.maximumPageBytes + 2 * AtlasObservedReaderLimits.contextBytes,
                  AtlasObservedReaderWire.validHex(extent_sha256, bytes: 32),
                  ["unchanged-metadata-not-atomic", "changed-during-read", "short-read", "metadata-unavailable"].contains(consistency),
                  requested_capture_filled == (captured_range == requested_capture_range),
                  !whole_file_observed || (captured_range.start == 0 && captured_range.end == length
                    && consistency == "unchanged-metadata-not-atomic"),
                  observation_source_bytes_read.value == captured_range.count,
                  observation_read_calls.value <= AtlasObservedReaderLimits.maximumReadCalls,
                  captured_range.count == 0 || observation_read_calls.value > 0,
                  consistency != "unchanged-metadata-not-atomic" || final_observed_length?.value == length,
                  (reads ? (additional_source_bytes_read.value == observation_source_bytes_read.value
                    && additional_read_calls.value == observation_read_calls.value)
                    : (additional_source_bytes_read.value == 0 && additional_read_calls.value == 0)) else {
                throw AtlasObservedReaderError.invalidResponse
            }
            return AtlasObservedReaderObservation(identity: identity, id: observation.value, request: request,
                requestedVisible: requested_visible_range, requestedCapture: requested_capture_range,
                observedLength: length, finalObservedLength: final_observed_length?.value,
                capturedRange: captured_range, extentSHA256: extent_sha256, consistency: consistency,
                captureFilled: requested_capture_filled, wholeFileObserved: whole_file_observed,
                sourceBytesRead: observation_source_bytes_read.value, readCalls: observation_read_calls.value)
        }
    }
}
