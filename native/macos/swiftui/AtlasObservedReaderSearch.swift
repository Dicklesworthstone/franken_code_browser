import Foundation

enum AtlasObservedReaderSearchLimits {
    static let needleBytes = 4096
    static let maximumWitnessBytes = needleBytes * 12
    static let matches: UInt64 = 256
    static let scanBytes: UInt64 = 256 * 1024 * 1024
    static let maximumScanBytes: UInt64 = 4 * 1024 * 1024 * 1024
    static let readCalls: UInt64 = 1_048_576
    static let inputBufferBytes: UInt64 = 16 * 1024
}

enum AtlasObservedReaderSearchMode: String, CaseIterable, Identifiable, Sendable {
    case text, occurrences
    var id: String { rawValue }
    var label: String { self == .text ? "Text" : "Occurrences" }
}

/// Text, matching policy and explicit encoding for one bounded scan of the
/// already opened file object. Unicode lowercase matching belongs to the engine.
struct AtlasObservedReaderSearchRequest: Equatable, Sendable {
    let generation: UInt64
    let needle: String
    let encoding: AtlasObservedReaderEncoding
    let maximumMatches, maximumScanBytes: UInt64
    let countAllMatches: Bool
    let caseSensitive: Bool
    let mode: AtlasObservedReaderSearchMode

    init(generation: UInt64, needle: String, encoding: AtlasObservedReaderEncoding,
         maximumMatches: UInt64 = AtlasObservedReaderSearchLimits.matches,
         maximumScanBytes: UInt64 = AtlasObservedReaderSearchLimits.scanBytes,
         countAllMatches: Bool = false, caseSensitive: Bool = true,
         mode: AtlasObservedReaderSearchMode = .text) throws {
        guard generation > 0, !needle.isEmpty,
              needle.utf8.count <= AtlasObservedReaderSearchLimits.needleBytes,
              !needle.utf8.contains(0),
              mode != .occurrences || (caseSensitive && needle.utf8.count <= 1024),
              (1...AtlasObservedReaderSearchLimits.matches).contains(maximumMatches),
              (1...AtlasObservedReaderSearchLimits.maximumScanBytes).contains(maximumScanBytes) else {
            throw AtlasObservedReaderError.invalidInput
        }
        self.generation = generation; self.needle = needle; self.encoding = encoding
        self.maximumMatches = maximumMatches; self.maximumScanBytes = maximumScanBytes
        self.countAllMatches = countAllMatches
        self.caseSensitive = caseSensitive; self.mode = mode
    }

    static func == (a: Self, b: Self) -> Bool {
        a.generation == b.generation && a.needle.utf8.elementsEqual(b.needle.utf8)
            && a.encoding == b.encoding && a.maximumMatches == b.maximumMatches
            && a.maximumScanBytes == b.maximumScanBytes && a.countAllMatches == b.countAllMatches
            && a.caseSensitive == b.caseSensitive && a.mode == b.mode
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
    var wireMode: String {
        mode == .occurrences ? "whole-token-occurrences" : (caseSensitive ? "exact-decoded-text" : "unicode-lowercase-text")
    }

    static func validWitnessRange(_ range: AtlasObservedByteRange, needle: String,
        encoding: AtlasObservedReaderEncoding, caseSensitive: Bool) -> Bool {
        guard range.count > 0, encoding == .utf8 || (range.start % 2 == 0 && range.end % 2 == 0) else { return false }
        return caseSensitive ? range.count == UInt64(originalBytes(needle, encoding: encoding).count)
            : range.count <= UInt64(needle.utf8.count * 12)
    }
    /// Check bounded spelling and its original encoding; do not duplicate the
    /// engine's Unicode matching rules in a second native matcher.
    static func validWitnessText(_ text: String, hex: String, range: AtlasObservedByteRange,
        needle: String, encoding: AtlasObservedReaderEncoding, caseSensitive: Bool) -> Bool {
        guard validWitnessRange(range, needle: needle, encoding: encoding, caseSensitive: caseSensitive),
              !text.isEmpty, text.utf8.count <= needle.utf8.count * 24, !text.utf8.contains(0),
              !caseSensitive || text.utf8.elementsEqual(needle.utf8) else { return false }
        let bytes = originalBytes(text, encoding: encoding)
        return UInt64(bytes.count) == range.count && hex == AtlasObservedReaderWire.hex(bytes)
    }
}

struct AtlasObservedReaderSearchTransport: Sendable {
    let find: @Sendable (UInt64, AtlasObservedReaderSearchRequest, AtlasSearchCancellation) throws -> String?
    let hit: @Sendable (UInt64, UInt64, UInt64) throws -> String?
    var observeHit: (@Sendable (UInt64, AtlasObservedReaderSearchWitness, AtlasObservedReaderRequest,
        AtlasSearchCancellation) throws -> String?)? = nil
}

struct AtlasObservedReaderSearchHit: Equatable, Identifiable, Sendable {
    let index, occurrence: UInt64
    let range: AtlasObservedByteRange
    let witnessSHA256: String
    var id: UInt64 { index }
}

/// A row target carries its complete query/owner authority. An old UI action
/// cannot lend its integer row index to a newer report or a reopened file.
struct AtlasObservedReaderSearchTarget: Equatable, Sendable {
    let identity: AtlasObservedReaderIdentity
    let generation, observation: UInt64
    let hit: AtlasObservedReaderSearchHit
}

/// This provenance belongs to a newly observed page, independently of later
/// query replacement. Its exact decoded range remains usable in retained history.
struct AtlasObservedReaderHitNavigation: Equatable, Sendable, Decodable {
    let generation, searchObservation, index, occurrence: UInt64
    let originalRange, decodedRange: AtlasObservedByteRange
    let mode: String?
    let caseSensitive, tokenBoundariesVerified: Bool?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        generation = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .query_generation).value
        searchObservation = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .search_observation).value
        index = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .hit_index).value
        occurrence = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .occurrence_id).value
        originalRange = try c.decode(AtlasObservedByteRange.self, forKey: .original_range)
        decodedRange = try c.decode(AtlasObservedByteRange.self, forKey: .decoded_utf8_range)
        mode = try c.decodeIfPresent(String.self, forKey: .mode)
        caseSensitive = try c.decodeIfPresent(Bool.self, forKey: .case_sensitive)
        tokenBoundariesVerified = try c.decodeIfPresent(Bool.self, forKey: .token_boundaries_verified)
        if mode != nil || caseSensitive != nil || tokenBoundariesVerified != nil {
            guard let mode, let caseSensitive, let tokenBoundariesVerified,
                  ["exact-decoded-text", "unicode-lowercase-text", "whole-token-occurrences"].contains(mode),
                  caseSensitive == (mode != "unicode-lowercase-text"),
                  tokenBoundariesVerified == (mode == "whole-token-occurrences") else {
                throw AtlasObservedReaderError.invalidResponse
            }
        }
        if mode == "whole-token-occurrences" {
            try AtlasObservedReaderModeEvidence(from: decoder).validate(.occurrences)
        }
        guard generation > 0, searchObservation > 0, index < AtlasObservedReaderSearchLimits.matches,
              occurrence == index + 1, originalRange.count > 0, decodedRange.count > 0,
              try c.decode(Bool.self, forKey: .witness_verified),
              try c.decode(String.self, forKey: .verification) == "equal-original-bytes-in-new-observation" else {
            throw AtlasObservedReaderError.invalidResponse
        }
    }
    private enum CodingKeys: CodingKey {
        case query_generation, search_observation, hit_index, occurrence_id, original_range,
             decoded_utf8_range, witness_verified, verification, mode, case_sensitive, token_boundaries_verified
    }
}

struct AtlasObservedReaderSearchReport: Sendable {
    let identity: AtlasObservedReaderIdentity
    let request: AtlasObservedReaderSearchRequest
    let observation, observedLength: UInt64
    let finalObservedLength: UInt64?
    let state, consistency: String
    let inputComplete, searchComplete, matchCountComplete: Bool
    let payloadBytesRead, scannedInputBytes, readCalls, peakInputBufferBytes, matchesSeen: UInt64
    let unsupportedAt: UInt64?
    let originalHex, witnessSHA256: String?
    let retainedWitnessBytes: UInt64
    let hits: [AtlasObservedReaderSearchHit]

    func target(for hit: AtlasObservedReaderSearchHit) -> AtlasObservedReaderSearchTarget {
        .init(identity: identity, generation: request.generation, observation: observation, hit: hit)
    }
    func admits(_ target: AtlasObservedReaderSearchTarget) -> Bool {
        target.identity == identity && target.generation == request.generation && target.observation == observation
            && target.hit.index < UInt64(hits.count) && hits[Int(target.hit.index)] == target.hit
    }
    var countLabel: String {
        if matchCountComplete {
            let total = "\(matchesSeen) matches in the observed input"
            return matchesSeen > UInt64(hits.count) ? total + "; first \(hits.count) retained" : total
        }
        if request.countAllMatches {
            return "At least \(matchesSeen) matches; first \(hits.count) retained · partial count"
        }
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
        let retained = request.countAllMatches && matchesSeen > UInt64(hits.count)
            ? " The result list retains the first \(hits.count) matches; the count includes later occurrences."
            : ""
        return "Scanned \(scannedInputBytes) of \(observedLength) initially observed bytes; read \(payloadBytesRead) bytes in \(readCalls) calls. "
            + reason + changed + retained + " Only matching bytes are retained, without surrounding context."
            + (request.mode == .occurrences
                ? " Whole-token spelling matches include comments and literals; compiler references are not established." : "")
    }

    static func decode(_ json: String?, handle: UInt64, input: AtlasObservedReaderInput,
                       request: AtlasObservedReaderSearchRequest) throws -> Self {
        let data = try AtlasObservedReaderWire.admit(json, handle: handle)
        do {
            let decoder = JSONDecoder(), w = try decoder.decode(Wire.self, from: data)
            try decoder.decode(AtlasObservedReaderModeEvidence.self, from: data).validate(request.mode)
            let identity = try decoder.decode(AtlasObservedReaderWire.Header.self, from: data)
                .identity(handle: handle, input: input, command: "find")
            let observation = try w.observation.value(request: request)
            let count = UInt64(w.hits.count)
            let encoded = request.originalHex
            let countFieldsPresent = w.count_all_matches != nil || w.match_count_complete != nil
            let matchCountComplete: Bool
            if countFieldsPresent {
                guard w.count_all_matches == request.countAllMatches,
                      w.match_count_complete == observation.searchComplete else {
                    throw AtlasObservedReaderError.invalidResponse
                }
                matchCountComplete = w.match_count_complete!
            } else {
                guard !request.countAllMatches else { throw AtlasObservedReaderError.invalidResponse }
                matchCountComplete = observation.searchComplete
            }
            guard w.mode == request.wireMode, w.case_sensitive == request.caseSensitive,
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
                  w.truncated == (w.matches_seen.value > count),
                  count <= w.matches_seen.value, w.matches_seen.value <= w.scanned_input_bytes.value,
                  !w.truncated || count == request.maximumMatches,
                  !observation.inputComplete || (w.scanned_input_bytes.value == observation.length
                    && w.payload_bytes_read.value == observation.length),
                  w.observation.state != "byte-limit" || w.payload_bytes_read.value == request.maximumScanBytes,
                  w.observation.state != "read-call-limit" || w.read_calls.value == AtlasObservedReaderSearchLimits.readCalls,
                  (w.observation.state == "unsupported-text") == (w.unsupported_at != nil),
                  w.observation.state != "unsupported-text" || count == 0,
                  w.unsupported_at.map({ $0.value < w.payload_bytes_read.value }) ?? true,
                  (count == 0 || !request.caseSensitive ? (w.literal_original_hex == nil && w.literal_sha256 == nil)
                    : (w.literal_original_hex == encoded
                      && w.literal_sha256.map { AtlasObservedReaderWire.validHex($0, bytes: 32) } == true)) else {
                throw AtlasObservedReaderError.invalidResponse
            }
            if request.countAllMatches {
                guard w.observation.state != "match-limit" else { throw AtlasObservedReaderError.invalidResponse }
            } else {
                guard w.truncated == (w.observation.state == "match-limit"),
                      w.matches_seen.value == count + (w.truncated ? 1 : 0) else {
                    throw AtlasObservedReaderError.invalidResponse
                }
            }
            var hits: [AtlasObservedReaderSearchHit] = []; hits.reserveCapacity(w.hits.count)
            var witnessBytes: UInt64 = 0
            for (index, hit) in w.hits.enumerated() {
                let digest = request.caseSensitive ? w.literal_sha256 : hit.witness_sha256
                guard hit.index.value == UInt64(index), hit.occurrence_id.value == UInt64(index) + 1,
                      AtlasObservedReaderSearchRequest.validWitnessRange(hit.original_range, needle: request.needle,
                        encoding: request.encoding, caseSensitive: request.caseSensitive),
                      hit.original_range.end <= w.scanned_input_bytes.value,
                      let digest, AtlasObservedReaderWire.validHex(digest, bytes: 32),
                      !request.caseSensitive || hit.witness_sha256 == nil || hit.witness_sha256 == digest,
                      hits.last.map({ previous in request.caseSensitive
                        ? previous.range.start < hit.original_range.start
                        : previous.range.start <= hit.original_range.start && previous.range.end <= hit.original_range.end }) ?? true else {
                    throw AtlasObservedReaderError.invalidResponse
                }
                if !request.caseSensitive || index == 0 { witnessBytes += hit.original_range.count }
                hits.append(.init(index: hit.index.value, occurrence: hit.occurrence_id.value,
                    range: hit.original_range, witnessSHA256: digest))
            }
            guard request.caseSensitive || w.retained_witness_bytes != nil,
                  w.retained_witness_bytes.map({ $0.value == witnessBytes }) ?? true else {
                throw AtlasObservedReaderError.invalidResponse
            }
            return Self(identity: identity, request: request, observation: observation.id,
                observedLength: observation.length, finalObservedLength: observation.finalLength,
                state: w.observation.state, consistency: w.observation.consistency,
                inputComplete: observation.inputComplete, searchComplete: observation.searchComplete,
                matchCountComplete: matchCountComplete,
                payloadBytesRead: w.payload_bytes_read.value, scannedInputBytes: w.scanned_input_bytes.value,
                readCalls: w.read_calls.value, peakInputBufferBytes: w.peak_input_buffer_bytes.value,
                matchesSeen: w.matches_seen.value, unsupportedAt: w.unsupported_at?.value,
                originalHex: w.literal_original_hex, witnessSHA256: w.literal_sha256,
                retainedWitnessBytes: witnessBytes, hits: hits)
        } catch { throw AtlasObservedReaderError.invalidResponse }
    }

    private struct Wire: Decodable {
        let observation: AtlasObservedReaderSearchWire.Observation
        let mode, literal_text: String
        let literal_original_hex, literal_sha256: String?
        let case_sensitive, truncated: Bool
        let count_all_matches, match_count_complete: Bool?
        let max_matches, max_scan_bytes, max_read_calls: AtlasObservedReaderWire.Number
        let payload_bytes_read, scanned_input_bytes, read_calls, peak_input_buffer_bytes: AtlasObservedReaderWire.Number
        let matches_seen, retained_hits, additional_source_bytes_read, additional_read_calls: AtlasObservedReaderWire.Number
        let unsupported_at: AtlasObservedReaderWire.Number?
        let retained_witness_bytes: AtlasObservedReaderWire.Number?
        let hits: [Hit]
        struct Hit: Decodable {
            let index, occurrence_id: AtlasObservedReaderWire.Number
            let original_range: AtlasObservedByteRange
            let witness_sha256: String?
        }
        init(from decoder: Decoder) throws {
            observation = try .init(from: decoder)
            let c = try decoder.container(keyedBy: CodingKeys.self)
            mode = try c.decode(String.self, forKey: .mode); literal_text = try c.decode(String.self, forKey: .literal_text)
            literal_original_hex = try c.decodeIfPresent(String.self, forKey: .literal_original_hex)
            literal_sha256 = try c.decodeIfPresent(String.self, forKey: .literal_sha256)
            case_sensitive = try c.decode(Bool.self, forKey: .case_sensitive); truncated = try c.decode(Bool.self, forKey: .truncated)
            count_all_matches = try c.decodeIfPresent(Bool.self, forKey: .count_all_matches)
            match_count_complete = try c.decodeIfPresent(Bool.self, forKey: .match_count_complete)
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
            retained_witness_bytes = try c.decodeIfPresent(AtlasObservedReaderWire.Number.self, forKey: .retained_witness_bytes)
            hits = try c.decode([Hit].self, forKey: .hits)
        }
        private enum CodingKeys: CodingKey {
            case mode, literal_text, literal_original_hex, literal_sha256, case_sensitive, truncated
            case count_all_matches, match_count_complete
            case max_matches, max_scan_bytes, max_read_calls, payload_bytes_read, scanned_input_bytes, read_calls
            case peak_input_buffer_bytes, matches_seen, retained_hits, additional_source_bytes_read, additional_read_calls
            case unsupported_at, retained_witness_bytes, hits
        }
    }
}

struct AtlasObservedReaderSearchWitness: Sendable {
    let target: AtlasObservedReaderSearchTarget
    let encoding: AtlasObservedReaderEncoding
    let text, originalHex, sha256: String
    var mode: AtlasObservedReaderSearchMode = .text

    static func decode(_ json: String?, handle: UInt64, input: AtlasObservedReaderInput,
                       report: AtlasObservedReaderSearchReport, target: AtlasObservedReaderSearchTarget) throws -> Self {
        let data = try AtlasObservedReaderWire.admit(json, handle: handle)
        do {
            let decoder = JSONDecoder(), w = try decoder.decode(Wire.self, from: data)
            try decoder.decode(AtlasObservedReaderModeEvidence.self, from: data).validate(report.request.mode)
            let identity = try decoder.decode(AtlasObservedReaderWire.Header.self, from: data)
                .identity(handle: handle, input: input, command: "search-hit")
            let common = try decoder.decode(AtlasObservedReaderSearchWire.Observation.self, from: data)
            let observation = try common.value(request: report.request)
            if w.mode != nil || w.case_sensitive != nil || !report.request.caseSensitive || report.request.mode == .occurrences {
                guard w.mode == report.request.wireMode, w.case_sensitive == report.request.caseSensitive else {
                    throw AtlasObservedReaderError.invalidResponse
                }
            }
            guard report.admits(target), identity == report.identity, observation.id == report.observation,
                  observation.length == report.observedLength, observation.finalLength == report.finalObservedLength,
                  observation.inputComplete == report.inputComplete, observation.searchComplete == report.searchComplete,
                  common.state == report.state, common.consistency == report.consistency,
                  w.index.value == target.hit.index, w.occurrence_id.value == target.hit.occurrence,
                  w.original_range == target.hit.range,
                  AtlasObservedReaderSearchRequest.validWitnessText(w.text, hex: w.original_hex, range: target.hit.range,
                    needle: report.request.needle, encoding: report.request.encoding, caseSensitive: report.request.caseSensitive),
                  w.witness_sha256 == target.hit.witnessSHA256,
                  w.additional_source_bytes_read.value == 0, w.additional_read_calls.value == 0 else {
                throw AtlasObservedReaderError.invalidResponse
            }
            return .init(target: target, encoding: report.request.encoding, text: w.text,
                originalHex: w.original_hex, sha256: w.witness_sha256, mode: report.request.mode)
        } catch { throw AtlasObservedReaderError.invalidResponse }
    }
    private struct Wire: Decodable {
        let index, occurrence_id, additional_source_bytes_read, additional_read_calls: AtlasObservedReaderWire.Number
        let original_range: AtlasObservedByteRange
        let text, original_hex, witness_sha256: String
        let mode: String?
        let case_sensitive: Bool?
    }

    /// New context is a separate observation. It may only receive a selection
    /// after its own original bytes verify the retained literal at this range.
    func verify(context page: AtlasObservedReaderPage) -> AtlasObservedReaderSearchContext {
        if page.findHit != nil {
            do {
                try validateVerifiedContext(page)
                return .verified(selection: page.verifiedSearchSelection)
            } catch { return .unavailable }
        }
        guard page.observation.identity == target.identity, page.observation.id > target.observation,
              page.observation.request.encoding == encoding,
              page.visibleRange.contains(target.hit.range) else { return .unavailable }
        let result = AtlasObservedReaderExternalMatch.verifyLiteral(context: page, range: target.hit.range,
            encoding: encoding, text: text, originalHex: originalHex)
        // An ordinary current window verifies bytes only. Identifier token
        // boundaries require the shared engine's explicit observe-hit action.
        if mode == .occurrences, case .verified = result { return .tokenBoundariesUnverified }
        return result
    }

    /// The shared engine identifies the selected scalar range even when other
    /// context contains replacement characters or the match splits a grapheme.
    /// Validate both byte domains; do not infer this mapping from nearby text.
    func validateVerifiedContext(_ page: AtlasObservedReaderPage) throws {
        guard let selected = page.findHit,
              page.observation.identity == target.identity, page.observation.id > target.observation,
              page.observation.request.encoding == encoding,
              selected.generation == target.generation, selected.searchObservation == target.observation,
              selected.index == target.hit.index, selected.occurrence == target.hit.occurrence,
              selected.originalRange == target.hit.range, page.visibleRange.contains(target.hit.range),
              selected.decodedRange.end <= UInt64(page.text.utf8.count),
              page.verifiedSearchSelection != nil,
              (mode != .occurrences || (selected.mode == "whole-token-occurrences"
                && selected.caseSensitive == true && selected.tokenBoundariesVerified == true)),
              (mode != .text || selected.mode != "whole-token-occurrences") else { throw AtlasObservedReaderError.invalidResponse }
        let start = Int(target.hit.range.start - page.visibleRange.start) * 2
        let count = Int(target.hit.range.count) * 2
        let raw = Array(page.originalHex.utf8), decoded = Array(page.text.utf8)
        guard start <= raw.count, count <= raw.count - start,
              raw[start..<(start + count)].elementsEqual(originalHex.utf8),
              decoded[Int(selected.decodedRange.start)..<Int(selected.decodedRange.end)].elementsEqual(text.utf8) else {
            throw AtlasObservedReaderError.invalidResponse
        }
    }
}

/// An earlier literal witness requests a new live observation, never an alias
/// of the earlier owner or an immutable whole-file source. The caller supplies
/// current project authority separately and revokes it with its search surface.
struct AtlasObservedReaderExternalMatch: Sendable {
    let input: AtlasObservedReaderInput
    let range: AtlasObservedByteRange
    let encoding: AtlasObservedReaderEncoding
    let text, originalHex: String
    init(input: AtlasObservedReaderInput, range: AtlasObservedByteRange,
         encoding: AtlasObservedReaderEncoding, text: String, originalHex: String) throws {
        guard !text.isEmpty, text.utf8.count <= AtlasObservedReaderSearchLimits.maximumWitnessBytes * 2,
              range.count <= UInt64(AtlasObservedReaderSearchLimits.maximumWitnessBytes) else {
            throw AtlasObservedReaderError.invalidInput
        }
        let bytes = AtlasObservedReaderSearchRequest.originalBytes(text, encoding: encoding)
        guard range.count == UInt64(bytes.count), range.count > 0,
              originalHex == AtlasObservedReaderWire.hex(bytes),
              encoding == .utf8 || range.start % 2 == 0 else { throw AtlasObservedReaderError.invalidInput }
        self.input = input; self.range = range; self.encoding = encoding
        self.text = text; self.originalHex = originalHex
        _ = try request()
    }
    func request() throws -> AtlasObservedReaderRequest {
        try .init(offset: range.start - min(range.start, 1024), encoding: encoding)
    }
    func verify(context page: AtlasObservedReaderPage) -> AtlasObservedReaderSearchContext {
        guard page.observation.identity.path == input.absolutePath, page.findHit == nil,
              page.observation.request.encoding == encoding else { return .unavailable }
        return Self.verifyLiteral(context: page, range: range, encoding: encoding, text: text, originalHex: originalHex)
    }
    static func verifyLiteral(context page: AtlasObservedReaderPage, range: AtlasObservedByteRange,
                              encoding: AtlasObservedReaderEncoding, text: String,
                              originalHex: String) -> AtlasObservedReaderSearchContext {
        guard page.visibleRange.contains(range) else { return .unavailable }
        let start = Int(range.start - page.visibleRange.start)
        let count = Int(range.count), raw = Array(page.originalHex.utf8)
        guard start * 2 <= raw.count, count * 2 <= raw.count - start * 2 else { return .unavailable }
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
    case verified(selection: NSRange?), changed, unavailable, tokenBoundariesUnverified
    var selection: NSRange? { if case .verified(let range) = self { return range }; return nil }
    var message: String {
        switch self {
        case .verified(let selection):
            return selection == nil
                ? "The matching bytes still agree in this new observation. An exact native text selection could not be verified."
                : "Matching bytes verified in this new observation and selected. Surrounding context was read now; it was not retained by the earlier search."
        case .tokenBoundariesUnverified: return "Matching bytes still agree, but whole-token boundaries were not rechecked. Use Go to verified match to verify this identifier in current context."
        case .changed: return "The bytes at this result changed. Current context is shown without an old-match highlight; the retained literal remains available."
        case .unavailable: return "The result range is unavailable in this new observation. No old-match highlight was applied."
        }
    }
}

/// Additive proof fields are mandatory for Occurrences. A literal response
/// cannot be relabeled as a whole-token result, even when the text happens to
/// match. Legacy literal receipts remain compatible.
private struct AtlasObservedReaderModeEvidence: Decodable {
    let requested_mode, realized_mode, evidence_level: String?
    let includes_comments_and_literals, compiler_resolved, semantic_complete: Bool?
    func validate(_ mode: AtlasObservedReaderSearchMode) throws {
        if mode == .occurrences {
            guard requested_mode == "occurrences", realized_mode == "whole-token-occurrences",
                  evidence_level == "whole-token-text-candidate", includes_comments_and_literals == true,
                  compiler_resolved == false, semantic_complete == false else {
                throw AtlasObservedReaderError.invalidResponse
            }
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
