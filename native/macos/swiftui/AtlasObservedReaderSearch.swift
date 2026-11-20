import Foundation

enum AtlasObservedReaderSearchLimits {
    static let needleBytes = 4096
    static let expressionBytes = 1024
    static let maximumWitnessBytes = needleBytes * 12
    static let matches: UInt64 = 256
    static let scanBytes: UInt64 = 256 * 1024 * 1024
    static let maximumScanBytes: UInt64 = 4 * 1024 * 1024 * 1024
    static let readCalls: UInt64 = 1_048_576
    static let inputBufferBytes: UInt64 = 16 * 1024
}

/// A bounded excerpt from the very same source observation as its match.
/// Original bytes remain authoritative. Reverse encoding only correlates the
/// engine's text/range receipt; no source decoder or matcher is duplicated here.
struct AtlasRetainedSearchContext: Equatable, Sendable {
    static let beforeBytes: UInt64 = 160
    static let afterBytes: UInt64 = 160
    let requestedRange, originalRange: AtlasObservedByteRange
    let complete: Bool
    let originalHex: String
    let text: String?
    let textRange, matchedTextRange: AtlasObservedByteRange?
    let hasReplacements: Bool?
    let unavailableReason: String?

    static func == (a: Self, b: Self) -> Bool {
        a.requestedRange == b.requestedRange && a.originalRange == b.originalRange
            && a.complete == b.complete && a.originalHex == b.originalHex
            && a.text.map({ Array($0.utf8) }) == b.text.map({ Array($0.utf8) })
            && a.textRange == b.textRange && a.matchedTextRange == b.matchedTextRange
            && a.hasReplacements == b.hasReplacements && a.unavailableReason == b.unavailableReason
    }

    /// Text with replacements remains available as original hex. Only a
    /// losslessly correlated decoded span can supply a native match highlight.
    var displayText: String? { hasReplacements == false ? text : nil }
    var parts: (before: String, match: String, after: String)? {
        guard let text = displayText, let range = matchedTextRange else { return nil }
        return Self.split(text, at: range)
    }

    func containsWitness(_ hex: String, at range: AtlasObservedByteRange) -> Bool {
        guard originalRange.contains(range) else { return false }
        let start = Int(range.start - originalRange.start) * 2
        let end = Int(range.end - originalRange.start) * 2
        return Array(originalHex.utf8)[start..<end].elementsEqual(hex.utf8)
    }

    private static func split(_ text: String, at range: AtlasObservedByteRange)
        -> (before: String, match: String, after: String)? {
        guard range.end <= UInt64(text.utf8.count) else { return nil }
        let bytes = Array(text.utf8), start = Int(range.start), end = Int(range.end)
        guard let before = String(bytes: bytes[..<start], encoding: .utf8),
              let match = String(bytes: bytes[start..<end], encoding: .utf8),
              let after = String(bytes: bytes[end...], encoding: .utf8) else { return nil }
        return (before, match, after)
    }

    static func decode(_ w: Wire, matched: AtlasObservedByteRange,
        observedLength: UInt64, encoding: AtlasObservedReaderEncoding,
        caseSensitive: Bool, primary: String) throws -> Self {
        guard matched.count > 0, matched.count <= UInt64(AtlasObservedReaderSearchLimits.maximumWitnessBytes),
              matched.end <= observedLength else { throw AtlasObservedReaderError.invalidResponse }
        let requested = w.requested_range, original = w.original_range
        let before = min(matched.start, beforeBytes), after = min(observedLength - matched.end, afterBytes)
        guard w.state == "retained", w.match_range == matched,
              requested.start == matched.start - before, requested.end == matched.end + after,
              requested.contains(original), original.start == requested.start, original.contains(matched),
              w.complete == (requested == original),
              AtlasObservedReaderWire.validHex(w.original_hex, bytes: original.count) else {
            throw AtlasObservedReaderError.invalidResponse
        }
        let textRange = w.text_original_range, matchedText = w.match_decoded_range
        if let text = w.text {
            guard w.text_unavailable_reason == nil, let textRange, let matchedText, let replacements = w.text_has_replacements,
                  original.contains(textRange), textRange.contains(matched),
                  text.utf8.count <= Int(original.count) * 3,
                  let parts = split(text, at: matchedText), !parts.match.isEmpty else {
                throw AtlasObservedReaderError.invalidResponse
            }
            // Correlate the literal inside the excerpt even if malformed
            // surrounding text prevents a lossless whole-excerpt projection.
            let raw = Array(w.original_hex.utf8)
            let matchStart = Int(matched.start - original.start) * 2
            let matchEnd = Int(matched.end - original.start) * 2
            let matchHex = String(decoding: raw[matchStart..<matchEnd], as: UTF8.self)
            guard AtlasObservedReaderSearchRequest.validWitnessText(parts.match, hex: matchHex,
                range: matched, needle: primary, encoding: encoding, caseSensitive: caseSensitive) else {
                throw AtlasObservedReaderError.invalidResponse
            }
            if !replacements {
                let start = Int(textRange.start - original.start) * 2
                let end = Int(textRange.end - original.start) * 2
                guard AtlasObservedReaderWire.hex(AtlasObservedReaderSearchRequest.originalBytes(text,
                        encoding: encoding)).utf8.elementsEqual(raw[start..<end]),
                      UInt64(AtlasObservedReaderSearchRequest.originalBytes(parts.before,
                        encoding: encoding).count) == matched.start - textRange.start else {
                    throw AtlasObservedReaderError.invalidResponse
                }
            }
        } else {
            guard textRange == nil, matchedText == nil, w.text_has_replacements == nil,
                  w.text_unavailable_reason == "insufficient-scalar-context" || w.text_unavailable_reason == "match-boundary-unavailable"
                    || w.text_unavailable_reason == "encoding-unavailable" else {
                throw AtlasObservedReaderError.invalidResponse
            }
        }
        return Self(requestedRange: requested, originalRange: original, complete: w.complete,
            originalHex: w.original_hex, text: w.text, textRange: textRange, matchedTextRange: matchedText,
            hasReplacements: w.text_has_replacements, unavailableReason: w.text_unavailable_reason)
    }

    struct Wire: Decodable {
        let state, original_hex: String
        let requested_range, original_range, match_range: AtlasObservedByteRange
        let complete: Bool
        let text, text_unavailable_reason: String?
        let text_original_range, match_decoded_range: AtlasObservedByteRange?
        let text_has_replacements: Bool?

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            state = try c.decode(String.self, forKey: .state)
            original_hex = try c.decode(String.self, forKey: .original_hex)
            requested_range = try c.decode(AtlasObservedByteRange.self, forKey: .requested_range)
            original_range = try c.decode(AtlasObservedByteRange.self, forKey: .original_range)
            match_range = try c.decode(AtlasObservedByteRange.self, forKey: .match_range)
            complete = try c.decode(Bool.self, forKey: .complete)
            text = try c.decode(String?.self, forKey: .text)
            text_unavailable_reason = try c.decode(String?.self, forKey: .text_unavailable_reason)
            text_original_range = try c.decode(AtlasObservedByteRange?.self, forKey: .text_original_range)
            match_decoded_range = try c.decode(AtlasObservedByteRange?.self, forKey: .match_decoded_range)
            text_has_replacements = try c.decode(Bool?.self, forKey: .text_has_replacements)
        }
        private enum CodingKeys: CodingKey {
            case state, original_hex, requested_range, original_range, match_range, complete,
                 text, text_unavailable_reason, text_original_range, match_decoded_range, text_has_replacements
        }
    }
}

enum AtlasObservedReaderSearchMode: String, CaseIterable, Identifiable, Sendable {
    case text, occurrences, expression
    var id: String { rawValue }
    var label: String { switch self { case .text: "Text"; case .occurrences: "Occurrences"; case .expression: "Expression" } }
    var abi: UInt64 { switch self { case .text: 0; case .occurrences: 1; case .expression: 2 } }
    var prompt: String {
        switch self {
        case .text: "Find text in the opened file"
        case .occurrences: "Find exact identifier"
        case .expression: "Expression, e.g. \"request failed\" timeout -deprecated"
        }
    }
    var help: String {
        switch self {
        case .text: "Literal text with optional Unicode lowercase matching. Up to 4096 UTF-8 bytes; NUL is unsupported."
        case .occurrences: "One exact identifier, up to 1024 UTF-8 bytes. Includes comments and literals; compiler references are not established."
        case .expression: "Required terms, quoted phrases and -excluded terms in this opened file. Up to 1024 UTF-8 bytes. Path/language filters and regular expressions are unavailable."
        }
    }
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
    let pageStartAfter: UInt64

    init(generation: UInt64, needle: String, encoding: AtlasObservedReaderEncoding,
         maximumMatches: UInt64 = AtlasObservedReaderSearchLimits.matches,
         maximumScanBytes: UInt64 = AtlasObservedReaderSearchLimits.scanBytes,
         countAllMatches: Bool = false, caseSensitive: Bool = true,
         mode: AtlasObservedReaderSearchMode = .text, pageStartAfter: UInt64 = 0) throws {
        guard generation > 0, !needle.isEmpty,
              needle.utf8.count <= AtlasObservedReaderSearchLimits.needleBytes,
              !needle.utf8.contains(0),
              mode != .occurrences || (caseSensitive && needle.utf8.count <= 1024),
              mode != .expression || needle.utf8.count <= AtlasObservedReaderSearchLimits.expressionBytes,
              (1...AtlasObservedReaderSearchLimits.matches).contains(maximumMatches),
              (1...AtlasObservedReaderSearchLimits.maximumScanBytes).contains(maximumScanBytes) else {
            throw AtlasObservedReaderError.invalidInput
        }
        self.generation = generation; self.needle = needle; self.encoding = encoding
        self.maximumMatches = maximumMatches; self.maximumScanBytes = maximumScanBytes
        self.countAllMatches = countAllMatches
        self.caseSensitive = caseSensitive; self.mode = mode
        self.pageStartAfter = pageStartAfter
    }

    static func == (a: Self, b: Self) -> Bool {
        a.generation == b.generation && a.needle.utf8.elementsEqual(b.needle.utf8)
            && a.encoding == b.encoding && a.maximumMatches == b.maximumMatches
            && a.maximumScanBytes == b.maximumScanBytes && a.countAllMatches == b.countAllMatches
            && a.caseSensitive == b.caseSensitive && a.mode == b.mode && a.pageStartAfter == b.pageStartAfter
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
        switch mode {
        case .text: caseSensitive ? "exact-decoded-text" : "unicode-lowercase-text"
        case .occurrences: "whole-token-occurrences"
        case .expression: "decoded-text-expression"
        }
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

/// The engine's bounded expression receipt, not a second native query parser.
/// The primary literal owns all highlighted ranges and retained witnesses;
/// other predicates describe the original completed search observation.
struct AtlasObservedReaderExpression: Equatable, Sendable {
    let query, primaryNeedle: String
    let qualified: Bool?
    static func == (a: Self, b: Self) -> Bool {
        a.query.utf8.elementsEqual(b.query.utf8) && a.primaryNeedle.utf8.elementsEqual(b.primaryNeedle.utf8)
            && a.qualified == b.qualified
    }
}

struct AtlasObservedReaderSearchTransport: Sendable {
    let find: @Sendable (UInt64, AtlasObservedReaderSearchRequest, AtlasSearchCancellation) throws -> String?
    let hit: @Sendable (UInt64, UInt64, UInt64) throws -> String?
    var observeHit: (@Sendable (UInt64, AtlasObservedReaderSearchWitness, AtlasObservedReaderRequest,
        AtlasSearchCancellation) throws -> String?)? = nil
    /// Replays current bytes from zero under a new generation and observation.
    /// This does not continue an immutable capture of the previous result page.
    var findPage: (@Sendable (UInt64, AtlasObservedReaderSearchRequest,
        AtlasSearchCancellation) throws -> String?)? = nil
    var openExcerpt: (@Sendable (UInt64, AtlasObservedReaderSearchWitness,
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
    let pageStartAfter: UInt64
    let originalRange, decodedRange: AtlasObservedByteRange
    let mode: String?
    let caseSensitive, tokenBoundariesVerified: Bool?
    let expression: AtlasObservedReaderExpression?
    let verification: String
    var isCapturedExcerpt: Bool { verification == "retained-original-search-bytes" }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        generation = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .query_generation).value
        searchObservation = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .search_observation).value
        index = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .hit_index).value
        occurrence = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .occurrence_id).value
        if c.contains(.page_start_after) {
            pageStartAfter = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .page_start_after).value
        } else { pageStartAfter = 0 }
        originalRange = try c.decode(AtlasObservedByteRange.self, forKey: .original_range)
        decodedRange = try c.decode(AtlasObservedByteRange.self, forKey: .decoded_utf8_range)
        mode = try c.decodeIfPresent(String.self, forKey: .mode)
        caseSensitive = try c.decodeIfPresent(Bool.self, forKey: .case_sensitive)
        tokenBoundariesVerified = try c.decodeIfPresent(Bool.self, forKey: .token_boundaries_verified)
        if mode != nil || caseSensitive != nil || tokenBoundariesVerified != nil {
            guard let mode, let caseSensitive, let tokenBoundariesVerified,
                  ["exact-decoded-text", "unicode-lowercase-text", "whole-token-occurrences", "decoded-text-expression"].contains(mode),
                  mode == "decoded-text-expression" || caseSensitive == (mode != "unicode-lowercase-text"),
                  tokenBoundariesVerified == (mode == "whole-token-occurrences") else {
                throw AtlasObservedReaderError.invalidResponse
            }
        }
        if mode == "whole-token-occurrences" {
            try AtlasObservedReaderModeEvidence(from: decoder).validate(.occurrences)
        }
        if mode == "decoded-text-expression" {
            try AtlasObservedReaderModeEvidence(from: decoder).validate(.expression)
        }
        expression = try AtlasObservedReaderExpressionEvidence(from: decoder)
            .validate(mode: mode == "decoded-text-expression" ? .expression : .text,
                scope: "original-search-observation")
        if let expression {
            guard expression.qualified == true else { throw AtlasObservedReaderError.invalidResponse }
        }
        let (ordinal, overflow) = pageStartAfter.addingReportingOverflow(index)
        let (expectedOccurrence, occurrenceOverflow) = ordinal.addingReportingOverflow(1)
        verification = try c.decode(String.self, forKey: .verification)
        guard generation > 0, searchObservation > 0, index < AtlasObservedReaderSearchLimits.matches,
              !overflow, !occurrenceOverflow, occurrence == expectedOccurrence,
              originalRange.count > 0, decodedRange.count > 0,
              try c.decode(Bool.self, forKey: .witness_verified),
              ["equal-original-bytes-in-new-observation", "retained-original-search-bytes"].contains(verification) else {
            throw AtlasObservedReaderError.invalidResponse
        }
    }
    private enum CodingKeys: CodingKey {
        case query_generation, search_observation, hit_index, occurrence_id, page_start_after, original_range,
             decoded_utf8_range, witness_verified, verification, mode, case_sensitive, token_boundaries_verified
    }
}

struct AtlasObservedReaderRetainedContext: Equatable, Sendable, Decodable {
    let generation, searchObservation, pageStartAfter, index, occurrence: UInt64
    let matchRange, requestedRange, originalRange: AtlasObservedByteRange
    let complete: Bool

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        generation = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .query_generation).value
        searchObservation = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .search_observation).value
        pageStartAfter = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .page_start_after).value
        index = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .hit_index).value
        occurrence = try c.decode(AtlasObservedReaderWire.Number.self, forKey: .occurrence_id).value
        matchRange = try c.decode(AtlasObservedByteRange.self, forKey: .match_range)
        requestedRange = try c.decode(AtlasObservedByteRange.self, forKey: .requested_range)
        originalRange = try c.decode(AtlasObservedByteRange.self, forKey: .original_range)
        complete = try c.decode(Bool.self, forKey: .complete)
        let (ordinal, overflow) = pageStartAfter.addingReportingOverflow(index)
        let (expected, occurrenceOverflow) = ordinal.addingReportingOverflow(1)
        guard generation > 0, searchObservation > 0, index < AtlasObservedReaderSearchLimits.matches,
              !overflow, !occurrenceOverflow, occurrence == expected,
              matchRange.count > 0, matchRange.count <= UInt64(AtlasObservedReaderSearchLimits.maximumWitnessBytes),
              requestedRange.contains(originalRange), originalRange.contains(matchRange),
              originalRange.start == requestedRange.start, complete == (requestedRange == originalRange) else {
            throw AtlasObservedReaderError.invalidResponse
        }
    }

    func validate(page: UInt64, length: UInt64) throws {
        guard searchObservation < page, matchRange.end <= length,
              requestedRange.start == matchRange.start - min(matchRange.start, AtlasRetainedSearchContext.beforeBytes),
              requestedRange.end == matchRange.end + min(length - matchRange.end, AtlasRetainedSearchContext.afterBytes) else {
            throw AtlasObservedReaderError.invalidResponse
        }
    }
    func matches(_ hit: AtlasObservedReaderHitNavigation) -> Bool {
        hit.isCapturedExcerpt && generation == hit.generation && searchObservation == hit.searchObservation
            && pageStartAfter == hit.pageStartAfter && index == hit.index && occurrence == hit.occurrence
            && matchRange == hit.originalRange
    }
    func matches(_ witness: AtlasObservedReaderSearchWitness) -> Bool {
        guard let context = witness.context else { return false }
        return generation == witness.target.generation && searchObservation == witness.target.observation
            && index == witness.target.hit.index && occurrence == witness.target.hit.occurrence
            && matchRange == witness.target.hit.range && requestedRange == context.requestedRange
            && originalRange == context.originalRange && complete == context.complete
    }
    private enum CodingKeys: CodingKey {
        case query_generation, search_observation, page_start_after, hit_index, occurrence_id,
             match_range, requested_range, original_range, complete
    }
}

struct AtlasObservedReaderSearchReport: Sendable {
    let identity: AtlasObservedReaderIdentity
    let request: AtlasObservedReaderSearchRequest
    let primaryNeedle: String
    let expression: AtlasObservedReaderExpression?
    let observation, observedLength: UInt64
    let finalObservedLength: UInt64?
    let state, consistency: String
    let inputComplete, searchComplete, matchCountComplete: Bool
    let payloadBytesRead, scannedInputBytes, readCalls, peakInputBufferBytes, matchesSeen: UInt64
    let unsupportedAt: UInt64?
    let originalHex, witnessSHA256: String?
    let retainedWitnessBytes: UInt64
    let retainedContextBytes: UInt64?
    let hits: [AtlasObservedReaderSearchHit]
    let skippedMatches: UInt64
    let pageHasMoreMatches: Bool

    func target(for hit: AtlasObservedReaderSearchHit) -> AtlasObservedReaderSearchTarget {
        .init(identity: identity, generation: request.generation, observation: observation, hit: hit)
    }
    func admits(_ target: AtlasObservedReaderSearchTarget) -> Bool {
        target.identity == identity && target.generation == request.generation && target.observation == observation
            && target.hit.index < UInt64(hits.count) && hits[Int(target.hit.index)] == target.hit
    }
    var containsCompleteMatchInventory: Bool {
        request.pageStartAfter == 0 && matchCountComplete && matchesSeen == UInt64(hits.count)
    }
    var retainedLabel: String {
        guard let first = hits.first, let last = hits.last else {
            return request.pageStartAfter == 0 ? "0 retained"
                : "no matches retained after occurrence \(request.pageStartAfter)"
        }
        return request.pageStartAfter == 0 ? "first \(hits.count) retained"
            : "occurrences \(first.occurrence)–\(last.occurrence) retained"
    }
    var countLabel: String {
        if let expression, expression.qualified == nil {
            return "Expression not yet established · no qualifying matches retained"
        }
        if let expression, expression.qualified == false {
            return "No matches satisfy this expression in the observed input"
        }
        if matchCountComplete {
            let total = "\(matchesSeen) matches in the observed input"
            return request.pageStartAfter > 0 || matchesSeen > UInt64(hits.count)
                ? total + "; " + retainedLabel : total
        }
        if request.countAllMatches {
            return "At least \(matchesSeen) matches; \(retainedLabel) · partial count"
        }
        if state == "match-limit" { return "At least \(matchesSeen) matches; \(retainedLabel)" }
        return request.pageStartAfter == 0 ? "\(hits.count) retained matches · partial search"
            : "At least \(matchesSeen) matches; \(retainedLabel) · partial search"
    }
    var coverage: String {
        let reason: String
        switch state {
        case "complete-observed-input": reason = "Reached the length observed before the scan."
        case "match-limit": reason = "Stopped after verifying a match beyond this result page. The total match count is unknown."
        case "byte-limit": reason = "Stopped at the byte limit. Remaining bytes are unsearched."
        case "read-call-limit": reason = "Stopped at the read-call limit. Remaining bytes are unsearched."
        case "short-read": reason = "The opened file ended before its initially observed length."
        default: reason = "The selected encoding could not verify text near byte \(unsupportedAt ?? 0); no matches from this scan are retained."
        }
        let changed = consistency == "unchanged-metadata-not-atomic"
            ? " Metadata checks did not detect a change; this is not an atomic snapshot."
            : " Metadata or read consistency: \(consistency). The total match count is unknown."
        let retained = request.pageStartAfter > 0
            ? " This scan skipped \(skippedMatches) earlier matches; \(retainedLabel)."
            : request.countAllMatches && matchesSeen > UInt64(hits.count)
            ? " The result list retains the first \(hits.count) matches; the count includes later occurrences."
            : ""
        return "Scanned \(scannedInputBytes) of \(observedLength) initially observed bytes; read \(payloadBytesRead) bytes in \(readCalls) calls. "
            + reason + changed + retained
            + (retainedContextBytes != nil
                ? " Matches and bounded surrounding excerpts remain available from the original scan, including after edits."
                : " Only matching bytes are retained, without surrounding context.")
            + " Rescanning a result page reads current bytes from the beginning and creates a new observation; edits can change occurrence numbers."
            + (request.mode == .occurrences
                ? " Whole-token spelling matches include comments and literals; compiler references are not established." : "")
            + (expression.map { value in
                value.qualified == nil
                    ? " Required and excluded terms could not all be decided in the bounded observation; an observed primary literal alone is not a qualifying result."
                    : " Required and excluded terms were checked through the same observed input. Counts and highlights refer only to the primary literal; no compiler binding is inferred."
            } ?? "")
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
            let expression = try decoder.decode(AtlasObservedReaderExpressionEvidence.self, from: data)
                .validate(mode: request.mode, query: request.needle, primary: w.literal_text,
                    scope: "same-observed-stream", requireNeedle: true)
            let primary = expression?.primaryNeedle ?? request.needle
            let encoded = AtlasObservedReaderWire.hex(AtlasObservedReaderSearchRequest.originalBytes(primary, encoding: request.encoding))
            if let expression {
                guard observation.inputComplete == (expression.qualified != nil),
                      (expression.qualified == true ? w.matches_seen.value > 0 : (count == 0 && w.matches_seen.value == 0)) else {
                    throw AtlasObservedReaderError.invalidResponse
                }
            }
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
            let skippedMatches = min(request.pageStartAfter, w.matches_seen.value)
            let availableMatches = w.matches_seen.value - skippedMatches
            let pageHasMoreMatches = availableMatches > count
            try decoder.decode(AtlasObservedReaderSearchPageEvidence.self, from: data)
                .validate(startAfter: request.pageStartAfter, skipped: skippedMatches, hasMore: pageHasMoreMatches)
            guard w.mode == request.wireMode, w.case_sensitive == request.caseSensitive,
                  w.literal_text.utf8.elementsEqual(primary.utf8),
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
                  count == min(availableMatches, request.maximumMatches),
                  !observation.inputComplete || (w.scanned_input_bytes.value == observation.length
                    && w.payload_bytes_read.value == observation.length),
                  w.observation.state != "byte-limit" || w.payload_bytes_read.value == request.maximumScanBytes,
                  w.observation.state != "read-call-limit" || w.read_calls.value == AtlasObservedReaderSearchLimits.readCalls,
                  (w.observation.state == "unsupported-text") == (w.unsupported_at != nil),
                  w.observation.state != "unsupported-text" || (count == 0 && w.matches_seen.value == 0),
                  w.unsupported_at.map({ $0.value < w.payload_bytes_read.value }) ?? true,
                  (count == 0 || !request.caseSensitive ? (w.literal_original_hex == nil && w.literal_sha256 == nil)
                    : (w.literal_original_hex == encoded
                      && w.literal_sha256.map { AtlasObservedReaderWire.validHex($0, bytes: 32) } == true)) else {
                throw AtlasObservedReaderError.invalidResponse
            }
            if request.countAllMatches || request.mode == .expression {
                guard w.observation.state != "match-limit" else { throw AtlasObservedReaderError.invalidResponse }
            } else {
                guard pageHasMoreMatches == (w.observation.state == "match-limit"),
                      availableMatches == count + (pageHasMoreMatches ? 1 : 0) else {
                    throw AtlasObservedReaderError.invalidResponse
                }
            }
            var hits: [AtlasObservedReaderSearchHit] = []; hits.reserveCapacity(w.hits.count)
            var witnessBytes: UInt64 = 0
            for (index, hit) in w.hits.enumerated() {
                let digest = request.caseSensitive ? w.literal_sha256 : hit.witness_sha256
                let (ordinal, overflow) = request.pageStartAfter.addingReportingOverflow(UInt64(index))
                let (occurrence, occurrenceOverflow) = ordinal.addingReportingOverflow(1)
                guard !overflow, !occurrenceOverflow,
                      hit.index.value == UInt64(index), hit.occurrence_id.value == occurrence,
                      AtlasObservedReaderSearchRequest.validWitnessRange(hit.original_range, needle: primary,
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
            if let contextBytes = observation.contextBytes {
                let minimum = hits.reduce(UInt64(0)) { $0 + $1.range.count }
                let maximum = hits.reduce(UInt64(0)) {
                    $0 + min($1.range.start, AtlasRetainedSearchContext.beforeBytes) + $1.range.count
                        + min(observation.length - $1.range.end, AtlasRetainedSearchContext.afterBytes)
                }
                guard contextBytes >= minimum, contextBytes <= maximum else {
                    throw AtlasObservedReaderError.invalidResponse
                }
            }
            return Self(identity: identity, request: request, primaryNeedle: primary, expression: expression,
                observation: observation.id,
                observedLength: observation.length, finalObservedLength: observation.finalLength,
                state: w.observation.state, consistency: w.observation.consistency,
                inputComplete: observation.inputComplete, searchComplete: observation.searchComplete,
                matchCountComplete: matchCountComplete,
                payloadBytesRead: w.payload_bytes_read.value, scannedInputBytes: w.scanned_input_bytes.value,
                readCalls: w.read_calls.value, peakInputBufferBytes: w.peak_input_buffer_bytes.value,
                matchesSeen: w.matches_seen.value, unsupportedAt: w.unsupported_at?.value,
                originalHex: w.literal_original_hex, witnessSHA256: w.literal_sha256,
                retainedWitnessBytes: witnessBytes, retainedContextBytes: observation.contextBytes, hits: hits,
                skippedMatches: skippedMatches, pageHasMoreMatches: pageHasMoreMatches)
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
    var caseSensitive = true
    var expression: AtlasObservedReaderExpression? = nil
    var context: AtlasRetainedSearchContext? = nil

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
            let expression = try decoder.decode(AtlasObservedReaderExpressionEvidence.self, from: data)
                .validate(mode: report.request.mode, query: report.request.needle,
                    primary: report.primaryNeedle, scope: "same-observed-stream")
            guard expression == report.expression,
                  expression == nil || expression?.qualified == true else { throw AtlasObservedReaderError.invalidResponse }
            try decoder.decode(AtlasObservedReaderSearchPageEvidence.self, from: data)
                .validate(startAfter: report.request.pageStartAfter, skipped: report.skippedMatches,
                    hasMore: report.pageHasMoreMatches)
            if w.mode != nil || w.case_sensitive != nil || !report.request.caseSensitive || report.request.mode != .text {
                guard w.mode == report.request.wireMode, w.case_sensitive == report.request.caseSensitive else {
                    throw AtlasObservedReaderError.invalidResponse
                }
            }
            guard report.admits(target), identity == report.identity, observation.id == report.observation,
                  observation.length == report.observedLength, observation.finalLength == report.finalObservedLength,
                  observation.inputComplete == report.inputComplete, observation.searchComplete == report.searchComplete,
                  observation.contextBytes == report.retainedContextBytes,
                  common.state == report.state, common.consistency == report.consistency,
                  w.index.value == target.hit.index, w.occurrence_id.value == target.hit.occurrence,
                  w.original_range == target.hit.range,
                  AtlasObservedReaderSearchRequest.validWitnessText(w.text, hex: w.original_hex, range: target.hit.range,
                    needle: report.primaryNeedle, encoding: report.request.encoding, caseSensitive: report.request.caseSensitive),
                  w.witness_sha256 == target.hit.witnessSHA256,
                  w.additional_source_bytes_read.value == 0, w.additional_read_calls.value == 0 else {
                throw AtlasObservedReaderError.invalidResponse
            }
            let context: AtlasRetainedSearchContext?
            if let retainedBytes = report.retainedContextBytes {
                guard let wire = w.context else { throw AtlasObservedReaderError.invalidResponse }
                let retained = try AtlasRetainedSearchContext.decode(wire, matched: target.hit.range,
                    observedLength: report.observedLength, encoding: report.request.encoding,
                    caseSensitive: report.request.caseSensitive, primary: report.primaryNeedle)
                guard retained.containsWitness(w.original_hex, at: target.hit.range),
                      retained.originalRange.end <= report.payloadBytesRead,
                      retained.originalRange.count <= retainedBytes,
                      report.hits.count != 1 || retained.originalRange.count == retainedBytes else {
                    throw AtlasObservedReaderError.invalidResponse
                }
                context = retained
            } else {
                guard w.context == nil else { throw AtlasObservedReaderError.invalidResponse }
                context = nil
            }
            return .init(target: target, encoding: report.request.encoding, text: w.text,
                originalHex: w.original_hex, sha256: w.witness_sha256, mode: report.request.mode,
                caseSensitive: report.request.caseSensitive, expression: expression, context: context)
        } catch { throw AtlasObservedReaderError.invalidResponse }
    }
    private struct Wire: Decodable {
        let index, occurrence_id, additional_source_bytes_read, additional_read_calls: AtlasObservedReaderWire.Number
        let original_range: AtlasObservedByteRange
        let text, original_hex, witness_sha256: String
        let mode: String?
        let case_sensitive: Bool?
        let context: AtlasRetainedSearchContext.Wire?
    }

    /// New context is a separate observation. It may only receive a selection
    /// after its own original bytes verify the retained literal at this range.
    func verify(context page: AtlasObservedReaderPage) -> AtlasObservedReaderSearchContext {
        if page.findHit != nil {
            do {
                try validateVerifiedContext(page)
                if page.findHit?.isCapturedExcerpt == true { return .captured(selection: page.verifiedSearchSelection) }
                return mode == .expression ? .expressionPrimaryVerified(selection: page.verifiedSearchSelection)
                    : .verified(selection: page.verifiedSearchSelection)
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
        if mode == .expression, case .verified(let selection) = result {
            return .expressionPrimaryVerified(selection: selection)
        }
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
              (mode != .expression || (selected.mode == "decoded-text-expression"
                && selected.caseSensitive == caseSensitive && selected.tokenBoundariesVerified == false
                && expression != nil && selected.expression == expression)),
              (mode != .text || (selected.mode != "whole-token-occurrences" && selected.expression == nil)) else {
            throw AtlasObservedReaderError.invalidResponse
        }
        let start = Int(target.hit.range.start - page.visibleRange.start) * 2
        let count = Int(target.hit.range.count) * 2
        let raw = Array(page.originalHex.utf8), decoded = Array(page.text.utf8)
        guard start <= raw.count, count <= raw.count - start,
              raw[start..<(start + count)].elementsEqual(originalHex.utf8),
              decoded[Int(selected.decodedRange.start)..<Int(selected.decodedRange.end)].elementsEqual(text.utf8) else {
            throw AtlasObservedReaderError.invalidResponse
        }
        if selected.isCapturedExcerpt {
            guard let origin = page.observation.retainedSearchContext, origin.matches(self), origin.matches(selected),
                  let context, let retainedText = context.text, page.visibleRange == context.textRange,
                  selected.decodedRange == context.matchedTextRange,
                  page.hasReplacements == context.hasReplacements,
                  page.text.utf8.elementsEqual(retainedText.utf8) else {
                throw AtlasObservedReaderError.invalidResponse
            }
            let from = Int(page.visibleRange.start - context.originalRange.start) * 2
            let end = Int(page.visibleRange.end - context.originalRange.start) * 2
            guard Array(context.originalHex.utf8)[from..<end].elementsEqual(page.originalHex.utf8) else {
                throw AtlasObservedReaderError.invalidResponse
            }
        } else if page.observation.retainedSearchContext != nil {
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
    case verified(selection: NSRange?), expressionPrimaryVerified(selection: NSRange?), captured(selection: NSRange?), changed, unavailable, tokenBoundariesUnverified
    var selection: NSRange? {
        switch self { case .verified(let range), .expressionPrimaryVerified(let range), .captured(let range): range; default: nil }
    }
    var message: String {
        switch self {
        case .captured:
            return "The original search excerpt is open and its exact match is selected. No current file bytes were read; this page remains available in history after later searches."
        case .verified(let selection):
            return selection == nil
                ? "The matching bytes still agree in this new observation. An exact native text selection could not be verified."
                : "Matching bytes verified in this new observation and selected. Surrounding context was read now as a separate observation."
        case .tokenBoundariesUnverified: return "Matching bytes still agree, but whole-token boundaries were not rechecked. Use Go to verified match to verify this identifier in current context."
        case .expressionPrimaryVerified(let selection):
            return "Primary literal bytes still agree in this new observation. Other required and excluded terms belong to the original search observation and were not rechecked."
                + (selection == nil ? " An exact native selection is unavailable." : " The primary literal is selected.")
        case .changed: return "The bytes at this result changed. Current context is shown without an old-match highlight; the retained literal remains available."
        case .unavailable: return "The result range is unavailable in this new observation. No old-match highlight was applied."
        }
    }
}

/// All page proof fields travel together. Earlier transports may omit them
/// only for a query beginning with the first match.
private struct AtlasObservedReaderSearchPageEvidence: Decodable {
    let present: Bool
    let startAfter, skipped: AtlasObservedReaderWire.Number?
    let hasMore: Bool?
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        present = c.contains(.page_start_after) || c.contains(.skipped_matches) || c.contains(.page_has_more_matches)
        startAfter = try c.decodeIfPresent(AtlasObservedReaderWire.Number.self, forKey: .page_start_after)
        skipped = try c.decodeIfPresent(AtlasObservedReaderWire.Number.self, forKey: .skipped_matches)
        hasMore = try c.decodeIfPresent(Bool.self, forKey: .page_has_more_matches)
    }
    func validate(startAfter: UInt64, skipped: UInt64, hasMore: Bool) throws {
        guard present ? (self.startAfter?.value == startAfter && self.skipped?.value == skipped && self.hasMore == hasMore)
            : startAfter == 0 else { throw AtlasObservedReaderError.invalidResponse }
    }
    private enum CodingKeys: CodingKey { case page_start_after, skipped_matches, page_has_more_matches }
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
        } else if mode == .expression {
            guard requested_mode == "expression", realized_mode == "decoded-text-expression",
                  compiler_resolved != true, semantic_complete != true else {
                throw AtlasObservedReaderError.invalidResponse
            }
        }
    }
}

/// Expression proof is atomic, including the explicitly nullable qualification.
/// Reject stray fields on literal/identifier routes so their witnesses cannot
/// acquire expression authority through a partially populated receipt.
private struct AtlasObservedReaderExpressionEvidence: Decodable {
    private let anyPresent, allPresent, revalidationPresent: Bool
    private let expression, primary_needle, query_syntax, predicate_scope: String?
    private let expression_qualified, expression_revalidated: Bool?
    private let needle: String?

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let required: [CodingKeys] = [.expression, .primary_needle, .query_syntax, .predicate_scope, .expression_qualified]
        anyPresent = required.contains { c.contains($0) } || c.contains(.expression_revalidated)
        allPresent = required.allSatisfy { c.contains($0) }
        revalidationPresent = c.contains(.expression_revalidated)
        expression = try c.decodeIfPresent(String.self, forKey: .expression)
        primary_needle = try c.decodeIfPresent(String.self, forKey: .primary_needle)
        query_syntax = try c.decodeIfPresent(String.self, forKey: .query_syntax)
        predicate_scope = try c.decodeIfPresent(String.self, forKey: .predicate_scope)
        expression_qualified = try c.decodeIfPresent(Bool.self, forKey: .expression_qualified)
        expression_revalidated = try c.decodeIfPresent(Bool.self, forKey: .expression_revalidated)
        needle = try c.decodeIfPresent(String.self, forKey: .needle)
    }

    func validate(mode: AtlasObservedReaderSearchMode, query: String? = nil, primary: String? = nil,
                  scope: String, requireNeedle: Bool = false) throws -> AtlasObservedReaderExpression? {
        guard mode == .expression else {
            guard !anyPresent else { throw AtlasObservedReaderError.invalidResponse }
            return nil
        }
        guard allPresent, let expression, let primary_needle,
              !expression.isEmpty, !primary_needle.isEmpty,
              expression.utf8.count <= AtlasObservedReaderSearchLimits.expressionBytes,
              primary_needle.utf8.count <= AtlasObservedReaderSearchLimits.expressionBytes,
              !expression.utf8.contains(0), !primary_needle.utf8.contains(0),
              query.map({ $0.utf8.elementsEqual(expression.utf8) }) ?? true,
              primary.map({ $0.utf8.elementsEqual(primary_needle.utf8) }) ?? true,
              query_syntax == "fcb-bounded-expression/1", predicate_scope == scope,
              !requireNeedle || needle?.utf8.elementsEqual(expression.utf8) == true,
              needle.map({ $0.utf8.elementsEqual(expression.utf8) }) ?? true,
              scope == "original-search-observation"
                ? (revalidationPresent && expression_revalidated == false) : !revalidationPresent else {
            throw AtlasObservedReaderError.invalidResponse
        }
        return .init(query: expression, primaryNeedle: primary_needle, qualified: expression_qualified)
    }
    private enum CodingKeys: CodingKey {
        case expression, primary_needle, query_syntax, predicate_scope, expression_qualified, expression_revalidated, needle
    }
}

private enum AtlasObservedReaderSearchWire {
    struct Observation: Decodable {
        let query_generation, search_observation, observed_length: AtlasObservedReaderWire.Number
        let final_observed_length: AtlasObservedReaderWire.Number?
        let encoding, encoding_basis, retention, state, consistency: String
        let input_complete, search_complete: Bool
        let context_before_bytes, context_after_bytes, retained_context_bytes: AtlasObservedReaderWire.Number?
        func value(request: AtlasObservedReaderSearchRequest) throws
            -> (id: UInt64, length: UInt64, finalLength: UInt64?, inputComplete: Bool, searchComplete: Bool, contextBytes: UInt64?) {
            guard query_generation.value == request.generation, search_observation.value > 0,
                  encoding == request.encoding.rawValue, encoding_basis == "explicit-for-this-search",
                  ["literal-witnesses-only", "literal-witnesses-and-bounded-context"].contains(retention),
                  ["complete-observed-input", "match-limit", "byte-limit", "read-call-limit", "short-read", "unsupported-text"].contains(state),
                  ["unchanged-metadata-not-atomic", "changed-during-read", "short-read", "metadata-unavailable"].contains(consistency),
                  input_complete == (state == "complete-observed-input"),
                  search_complete == (input_complete && consistency == "unchanged-metadata-not-atomic"),
                  consistency != "unchanged-metadata-not-atomic" || final_observed_length?.value == observed_length.value,
                  state != "short-read" || !search_complete else { throw AtlasObservedReaderError.invalidResponse }
            if retention == "literal-witnesses-and-bounded-context" {
                guard context_before_bytes?.value == AtlasRetainedSearchContext.beforeBytes,
                      context_after_bytes?.value == AtlasRetainedSearchContext.afterBytes,
                      retained_context_bytes != nil else { throw AtlasObservedReaderError.invalidResponse }
            } else {
                guard context_before_bytes == nil, context_after_bytes == nil, retained_context_bytes == nil else {
                    throw AtlasObservedReaderError.invalidResponse
                }
            }
            return (search_observation.value, observed_length.value, final_observed_length?.value, input_complete,
                search_complete, retained_context_bytes?.value)
        }
    }
}
