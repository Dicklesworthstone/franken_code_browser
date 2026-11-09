import Foundation

enum AtlasObservedReaderSearchLimits {
    static let needleBytes = 4096
    static let matches: UInt64 = 256
    static let scanBytes: UInt64 = 256 * 1024 * 1024
    static let maximumScanBytes: UInt64 = 4 * 1024 * 1024 * 1024
    static let readCalls: UInt64 = 1_048_576
    static let inputBufferBytes: UInt64 = 16 * 1024
}

/// Exact text and an explicit encoding for one bounded scan of the already
/// opened file object. No case folding, normalization or regex is implied.
struct AtlasObservedReaderSearchRequest: Equatable, Sendable {
    let generation: UInt64
    let needle: String
    let encoding: AtlasObservedReaderEncoding
    let maximumMatches, maximumScanBytes: UInt64

    init(generation: UInt64, needle: String, encoding: AtlasObservedReaderEncoding,
         maximumMatches: UInt64 = AtlasObservedReaderSearchLimits.matches,
         maximumScanBytes: UInt64 = AtlasObservedReaderSearchLimits.scanBytes) throws {
        guard generation > 0, !needle.isEmpty,
              needle.utf8.count <= AtlasObservedReaderSearchLimits.needleBytes,
              !needle.utf8.contains(0),
              (1...AtlasObservedReaderSearchLimits.matches).contains(maximumMatches),
              (1...AtlasObservedReaderSearchLimits.maximumScanBytes).contains(maximumScanBytes) else {
            throw AtlasObservedReaderError.invalidInput
        }
        self.generation = generation; self.needle = needle; self.encoding = encoding
        self.maximumMatches = maximumMatches; self.maximumScanBytes = maximumScanBytes
    }

    static func == (a: Self, b: Self) -> Bool {
        a.generation == b.generation && a.needle.utf8.elementsEqual(b.needle.utf8)
            && a.encoding == b.encoding && a.maximumMatches == b.maximumMatches
            && a.maximumScanBytes == b.maximumScanBytes
    }

    /// Reverse encoding is only a correlation check against engine receipts;
    /// source decoding and exact matching remain in the shared Rust engine.
    static func originalBytes(_ text: String, encoding: AtlasObservedReaderEncoding) -> [UInt8] {
        if encoding == .utf8 { return Array(text.utf8) }
        var bytes: [UInt8] = []; bytes.reserveCapacity(text.utf16.count * 2)
        for unit in text.utf16 {
            let low = UInt8(truncatingIfNeeded: unit), high = UInt8(truncatingIfNeeded: unit >> 8)
            bytes.append(contentsOf: encoding == .utf16le ? [low, high] : [high, low])
        }
        return bytes
    }
    var originalHex: String { AtlasObservedReaderWire.hex(Self.originalBytes(needle, encoding: encoding)) }
}

struct AtlasObservedReaderSearchTransport: Sendable {
    let find: @Sendable (UInt64, AtlasObservedReaderSearchRequest, AtlasSearchCancellation) throws -> String?
    let hit: @Sendable (UInt64, UInt64, UInt64) throws -> String?
}

struct AtlasObservedReaderSearchHit: Equatable, Identifiable, Sendable {
    let index, occurrence: UInt64
    let range: AtlasObservedByteRange
    var id: UInt64 { index }
}

/// A row target carries its complete query/owner authority. An old UI action
/// cannot lend its integer row index to a newer report or a reopened file.
struct AtlasObservedReaderSearchTarget: Equatable, Sendable {
    let identity: AtlasObservedReaderIdentity
    let generation, observation: UInt64
    let hit: AtlasObservedReaderSearchHit
}

struct AtlasObservedReaderSearchReport: Sendable {
    let identity: AtlasObservedReaderIdentity
    let request: AtlasObservedReaderSearchRequest
    let observation, observedLength: UInt64
    let finalObservedLength: UInt64?
    let state, consistency: String
    let inputComplete, searchComplete: Bool
    let payloadBytesRead, scannedInputBytes, readCalls, peakInputBufferBytes, matchesSeen: UInt64
    let unsupportedAt: UInt64?
    let originalHex, witnessSHA256: String?
    let hits: [AtlasObservedReaderSearchHit]

    func target(for hit: AtlasObservedReaderSearchHit) -> AtlasObservedReaderSearchTarget {
        .init(identity: identity, generation: request.generation, observation: observation, hit: hit)
    }
    func admits(_ target: AtlasObservedReaderSearchTarget) -> Bool {
        target.identity == identity && target.generation == request.generation && target.observation == observation
            && target.hit.index < UInt64(hits.count) && hits[Int(target.hit.index)] == target.hit
    }
    var countLabel: String {
        if searchComplete { return "\(hits.count) matches in the observed input" }
        if state == "match-limit" { return "At least \(matchesSeen) matches; first \(hits.count) retained" }
        return "\(hits.count) retained matches · partial search"
    }
    var coverage: String {
        let reason: String
        switch state {
        case "complete-observed-input": reason = "Reached the length observed before the scan."
        case "match-limit": reason = "Stopped at the match limit. Later matches are unknown."
        case "byte-limit": reason = "Stopped at the byte limit. Remaining bytes are unsearched."
        case "read-call-limit": reason = "Stopped at the read-call limit. Remaining bytes are unsearched."
        case "short-read": reason = "The opened file ended before its initially observed length."
        default: reason = "The selected encoding could not verify text near byte \(unsupportedAt ?? 0); no matches from this scan are retained."
        }
        let changed = consistency == "unchanged-metadata-not-atomic"
            ? " Metadata checks did not detect a change; this is not an atomic snapshot."
            : " Metadata or read consistency: \(consistency). The total match count is unknown."
        return "Scanned \(scannedInputBytes) of \(observedLength) initially observed bytes; read \(payloadBytesRead) bytes in \(readCalls) calls. "
            + reason + changed + " Only matching literal bytes are retained, without surrounding context."
    }

    static func decode(_ json: String?, handle: UInt64, input: AtlasObservedReaderInput,
                       request: AtlasObservedReaderSearchRequest) throws -> Self {
        let data = try AtlasObservedReaderWire.admit(json, handle: handle)
        do {
            let decoder = JSONDecoder(), w = try decoder.decode(Wire.self, from: data)
            let identity = try decoder.decode(AtlasObservedReaderWire.Header.self, from: data)
                .identity(handle: handle, input: input, command: "find")
            let observation = try w.observation.value(request: request)
            let count = UInt64(w.hits.count)
            let encoded = request.originalHex
            guard w.mode == "exact-decoded-text", w.case_sensitive,
                  w.literal_text.utf8.elementsEqual(request.needle.utf8),
                  w.max_matches.value == request.maximumMatches, w.max_scan_bytes.value == request.maximumScanBytes,
                  w.max_read_calls.value == AtlasObservedReaderSearchLimits.readCalls,
                  w.retained_hits.value == count, count <= request.maximumMatches,
                  w.payload_bytes_read.value <= min(observation.length, request.maximumScanBytes),
                  w.scanned_input_bytes.value <= w.payload_bytes_read.value,
                  w.read_calls.value <= AtlasObservedReaderSearchLimits.readCalls,
                  w.payload_bytes_read.value == 0 || w.read_calls.value > 0,
                  w.peak_input_buffer_bytes.value <= min(w.payload_bytes_read.value, AtlasObservedReaderSearchLimits.inputBufferBytes),
                  w.additional_source_bytes_read.value == w.payload_bytes_read.value,
                  w.additional_read_calls.value == w.read_calls.value,
                  w.truncated == (w.observation.state == "match-limit"),
                  w.matches_seen.value == count + (w.truncated ? 1 : 0),
                  !w.truncated || count == request.maximumMatches,
                  !observation.inputComplete || (w.scanned_input_bytes.value == observation.length
                    && w.payload_bytes_read.value == observation.length),
                  w.observation.state != "byte-limit" || w.payload_bytes_read.value == request.maximumScanBytes,
                  w.observation.state != "read-call-limit" || w.read_calls.value == AtlasObservedReaderSearchLimits.readCalls,
                  (w.observation.state == "unsupported-text") == (w.unsupported_at != nil),
                  w.observation.state != "unsupported-text" || count == 0,
                  w.unsupported_at.map({ $0.value < w.payload_bytes_read.value }) ?? true,
                  (count == 0 ? (w.literal_original_hex == nil && w.literal_sha256 == nil)
                    : (w.literal_original_hex == encoded
                      && w.literal_sha256.map { AtlasObservedReaderWire.validHex($0, bytes: 32) } == true)) else {
                throw AtlasObservedReaderError.invalidResponse
            }
            var hits: [AtlasObservedReaderSearchHit] = []; hits.reserveCapacity(w.hits.count)
            for (index, hit) in w.hits.enumerated() {
                guard hit.index.value == UInt64(index), hit.occurrence_id.value == UInt64(index) + 1,
                      hit.original_range.count == UInt64(encoded.utf8.count / 2),
                      hit.original_range.end <= w.scanned_input_bytes.value,
                      request.encoding == .utf8 || hit.original_range.start % 2 == 0,
                      hits.last.map({ $0.range.start < hit.original_range.start }) ?? true else {
                    throw AtlasObservedReaderError.invalidResponse
                }
                hits.append(.init(index: hit.index.value, occurrence: hit.occurrence_id.value, range: hit.original_range))
            }
            return Self(identity: identity, request: request, observation: observation.id,
                observedLength: observation.length, finalObservedLength: observation.finalLength,
                state: w.observation.state, consistency: w.observation.consistency,
                inputComplete: observation.inputComplete, searchComplete: observation.searchComplete,
                payloadBytesRead: w.payload_bytes_read.value, scannedInputBytes: w.scanned_input_bytes.value,
                readCalls: w.read_calls.value, peakInputBufferBytes: w.peak_input_buffer_bytes.value,
                matchesSeen: w.matches_seen.value, unsupportedAt: w.unsupported_at?.value,
                originalHex: w.literal_original_hex, witnessSHA256: w.literal_sha256, hits: hits)
        } catch { throw AtlasObservedReaderError.invalidResponse }
    }

    private struct Wire: Decodable {
        let observation: AtlasObservedReaderSearchWire.Observation
        let mode, literal_text: String
        let literal_original_hex, literal_sha256: String?
        let case_sensitive, truncated: Bool
        let max_matches, max_scan_bytes, max_read_calls: AtlasObservedReaderWire.Number
        let payload_bytes_read, scanned_input_bytes, read_calls, peak_input_buffer_bytes: AtlasObservedReaderWire.Number
        let matches_seen, retained_hits, additional_source_bytes_read, additional_read_calls: AtlasObservedReaderWire.Number
        let unsupported_at: AtlasObservedReaderWire.Number?
        let hits: [Hit]
        struct Hit: Decodable {
            let index, occurrence_id: AtlasObservedReaderWire.Number
            let original_range: AtlasObservedByteRange
        }
        init(from decoder: Decoder) throws {
            observation = try .init(from: decoder)
            let c = try decoder.container(keyedBy: CodingKeys.self)
            mode = try c.decode(String.self, forKey: .mode); literal_text = try c.decode(String.self, forKey: .literal_text)
            literal_original_hex = try c.decodeIfPresent(String.self, forKey: .literal_original_hex)
            literal_sha256 = try c.decodeIfPresent(String.self, forKey: .literal_sha256)
            case_sensitive = try c.decode(Bool.self, forKey: .case_sensitive); truncated = try c.decode(Bool.self, forKey: .truncated)
            max_matches = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .max_matches)
            max_scan_bytes = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .max_scan_bytes)
            max_read_calls = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .max_read_calls)
            payload_bytes_read = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .payload_bytes_read)
            scanned_input_bytes = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .scanned_input_bytes)
            read_calls = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .read_calls)
            peak_input_buffer_bytes = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .peak_input_buffer_bytes)
            matches_seen = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .matches_seen)
            retained_hits = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .retained_hits)
            additional_source_bytes_read = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .additional_source_bytes_read)
            additional_read_calls = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .additional_read_calls)
            unsupported_at = try c.decodeIfPresent(AtlasObservedReaderWire.Number.self, forKey: .unsupported_at)
            hits = try c.decode([Hit].self, forKey: .hits)
        }
        private enum CodingKeys: CodingKey {
            case mode, literal_text, literal_original_hex, literal_sha256, case_sensitive, truncated
            case max_matches, max_scan_bytes, max_read_calls, payload_bytes_read, scanned_input_bytes, read_calls
            case peak_input_buffer_bytes, matches_seen, retained_hits, additional_source_bytes_read, additional_read_calls
            case unsupported_at, hits
        }
    }
}

struct AtlasObservedReaderSearchWitness: Sendable {
    let target: AtlasObservedReaderSearchTarget
    let encoding: AtlasObservedReaderEncoding
    let text, originalHex, sha256: String

    static func decode(_ json: String?, handle: UInt64, input: AtlasObservedReaderInput,
                       report: AtlasObservedReaderSearchReport, target: AtlasObservedReaderSearchTarget) throws -> Self {
        let data = try AtlasObservedReaderWire.admit(json, handle: handle)
        do {
            let decoder = JSONDecoder(), w = try decoder.decode(Wire.self, from: data)
            let identity = try decoder.decode(AtlasObservedReaderWire.Header.self, from: data)
                .identity(handle: handle, input: input, command: "search-hit")
            let common = try decoder.decode(AtlasObservedReaderSearchWire.Observation.self, from: data)
            let observation = try common.value(request: report.request)
            guard report.admits(target), identity == report.identity, observation.id == report.observation,
                  observation.length == report.observedLength, observation.finalLength == report.finalObservedLength,
                  observation.inputComplete == report.inputComplete, observation.searchComplete == report.searchComplete,
                  common.state == report.state, common.consistency == report.consistency,
                  w.index.value == target.hit.index, w.occurrence_id.value == target.hit.occurrence,
                  w.original_range == target.hit.range,
                  w.text.utf8.elementsEqual(report.request.needle.utf8),
                  w.original_hex == report.originalHex, w.witness_sha256 == report.witnessSHA256,
                  w.additional_source_bytes_read.value == 0, w.additional_read_calls.value == 0 else {
                throw AtlasObservedReaderError.invalidResponse
            }
            return .init(target: target, encoding: report.request.encoding, text: w.text,
                originalHex: w.original_hex, sha256: w.witness_sha256)
        } catch { throw AtlasObservedReaderError.invalidResponse }
    }
    private struct Wire: Decodable {
        let index, occurrence_id, additional_source_bytes_read, additional_read_calls: AtlasObservedReaderWire.Number
        let original_range: AtlasObservedByteRange
        let text, original_hex, witness_sha256: String
    }

    /// New context is a separate observation. It may only receive a selection
    /// after its own original bytes verify the retained literal at this range.
    func verify(context page: AtlasObservedReaderPage) -> AtlasObservedReaderSearchContext {
        guard page.observation.identity == target.identity, page.observation.id > target.observation,
              page.observation.request.encoding == encoding,
              page.visibleRange.contains(target.hit.range) else { return .unavailable }
        let start = Int(target.hit.range.start - page.visibleRange.start)
        let count = Int(target.hit.range.count), raw = Array(page.originalHex.utf8)
        guard raw[(start * 2)..<((start + count) * 2)].elementsEqual(originalHex.utf8) else { return .changed }
        let encoded = AtlasObservedReaderSearchRequest.originalBytes(page.text, encoding: encoding)
        guard !page.hasReplacements, AtlasObservedReaderWire.hex(encoded) == page.originalHex else {
            return .verified(selection: nil)
        }
        let candidate: NSRange?
        if encoding == .utf8 {
            candidate = AtlasSource(path: page.observation.identity.displayPath, text: page.text)
                .utf16Range(byteStart: UInt64(start), byteEnd: UInt64(start + count))
        } else {
            candidate = start % 2 == 0 && count % 2 == 0 ? NSRange(location: start / 2, length: count / 2) : nil
        }
        let native = page.text as NSString
        guard let candidate, candidate.location <= native.length,
              candidate.length <= native.length - candidate.location,
              native.rangeOfComposedCharacterSequences(for: candidate) == candidate,
              native.substring(with: candidate).utf8.elementsEqual(text.utf8) else { return .verified(selection: nil) }
        return .verified(selection: candidate)
    }
}

enum AtlasObservedReaderSearchContext: Equatable, Sendable {
    case verified(selection: NSRange?), changed, unavailable
    var selection: NSRange? { if case .verified(let range) = self { return range }; return nil }
    var message: String {
        switch self {
        case .verified(let selection):
            return selection == nil
                ? "The matching bytes still agree in this new observation. An exact native text selection could not be verified."
                : "Matching bytes verified in this new observation and selected. Surrounding context was read now; it was not retained by the earlier search."
        case .changed: return "The bytes at this result changed. Current context is shown without an old-match highlight; the retained literal remains available."
        case .unavailable: return "The result range is unavailable in this new observation. No old-match highlight was applied."
        }
    }
}

private enum AtlasObservedReaderSearchWire {
    struct Observation: Decodable {
        let query_generation, search_observation, observed_length: AtlasObservedReaderWire.Number
        let final_observed_length: AtlasObservedReaderWire.Number?
        let encoding, encoding_basis, retention, state, consistency: String
        let input_complete, search_complete: Bool
        func value(request: AtlasObservedReaderSearchRequest) throws
            -> (id: UInt64, length: UInt64, finalLength: UInt64?, inputComplete: Bool, searchComplete: Bool) {
            guard query_generation.value == request.generation, search_observation.value > 0,
                  encoding == request.encoding.rawValue, encoding_basis == "explicit-for-this-search",
                  retention == "literal-witnesses-only",
                  ["complete-observed-input", "match-limit", "byte-limit", "read-call-limit", "short-read", "unsupported-text"].contains(state),
                  ["unchanged-metadata-not-atomic", "changed-during-read", "short-read", "metadata-unavailable"].contains(consistency),
                  input_complete == (state == "complete-observed-input"),
                  search_complete == (input_complete && consistency == "unchanged-metadata-not-atomic"),
                  consistency != "unchanged-metadata-not-atomic" || final_observed_length?.value == observed_length.value,
                  state != "short-read" || !search_complete else { throw AtlasObservedReaderError.invalidResponse }
            return (search_observation.value, observed_length.value, final_observed_length?.value, input_complete, search_complete)
        }
    }
}
