import Foundation

/// Optional transport capability, separate from source-page-only hosts. Native
/// builds supply the existing Rust find/hit ABI, not another matcher or decoder.
struct AtlasReaderSearchTransport: Sendable {
    let find: @Sendable (UInt64, UInt64, String, UInt64, UInt64) -> String?
    let hit: @Sendable (UInt64, UInt64, UInt64, UInt64) -> String?
    var findWithOptions: (@Sendable (UInt64, UInt64, String, UInt64, UInt64, UInt64) -> String?)? = nil
    /// Ordinal pages replay the same immutable capture; they do not reopen disk.
    var findPage: (@Sendable (UInt64, UInt64, String, UInt64, UInt64, UInt64, UInt64) -> String?)? = nil
    /// A separate entry point preserves the literal meaning of older hosts.
    var findExpressionPage: (@Sendable (UInt64, UInt64, String, UInt64, UInt64, UInt64, UInt64) -> String?)? = nil

    func supportsPages(for mode: AtlasReaderFindMode) -> Bool {
        mode == .expression ? findExpressionPage != nil : findPage != nil
    }

    func performFind(_ handle: UInt64, _ generation: UInt64, _ needle: String,
                     _ matches: UInt64, _ bytes: UInt64, options: AtlasReaderFindOptions,
                     startAfter: UInt64 = 0) throws -> String? {
        if options.mode == .expression {
            guard let findExpressionPage else { throw AtlasReaderError.unavailable }
            return findExpressionPage(handle, generation, needle, startAfter, matches, bytes, options.flags)
        }
        if let findPage { return findPage(handle, generation, needle, startAfter, matches, bytes, options.flags) }
        guard startAfter == 0 else { throw AtlasReaderError.unavailable }
        if let findWithOptions { return findWithOptions(handle, generation, needle, matches, bytes, options.flags) }
        guard options == .init() else { throw AtlasReaderError.unavailable }
        return find(handle, generation, needle, matches, bytes)
    }
}

enum AtlasReaderFindMode: String, CaseIterable, Identifiable, Sendable {
    case literal = "Text", expression = "Expression"
    var id: Self { self }
    var help: String {
        switch self {
        case .literal: "Find literal text in this retained file (⌘F); move between matches with ⌘G and ⇧⌘G."
        case .expression: "Required terms, quoted phrases and -excluded terms across this retained file. For example: \"request failed\" timeout -deprecated. Up to 1024 UTF-8 bytes; use project search for path or language filters."
        }
    }
    var placeholder: String {
        self == .expression ? "Expression in retained file…" : "Find in retained file…"
    }
}

struct AtlasReaderFindOptions: Equatable, Sendable {
    var caseSensitive = true
    var countAllMatches = false
    var mode = AtlasReaderFindMode.literal
    /// Mode uses its own ABI entry point, never a reinterpretation of these bits.
    var flags: UInt64 { (caseSensitive ? 0 : 1) | (countAllMatches ? 2 : 0) }
}

/// The shared engine's query receipt. Swift validates its identity and bounds;
/// it does not parse predicates or decide whether a source qualifies.
struct AtlasReaderFindExpression: Equatable, Sendable {
    let query, primaryNeedle: String
    let qualified: Bool?
    static func == (a: Self, b: Self) -> Bool {
        a.query.utf8.elementsEqual(b.query.utf8)
            && a.primaryNeedle.utf8.elementsEqual(b.primaryNeedle.utf8) && a.qualified == b.qualified
    }
}

struct AtlasReaderHitTarget: Equatable, Sendable {
    let generation: UInt64
    let index: UInt64
    let start: UInt64
    let end: UInt64
    let needle: String
    let originalHex: String
    static let contextBytes: UInt64 = 2048

    /// Swift String equality normalizes canonically equivalent text. Exact
    /// queries and their witnesses instead use the encoded byte spelling.
    static func == (a: Self, b: Self) -> Bool {
        a.generation == b.generation && a.index == b.index && a.start == b.start && a.end == b.end
            && a.needle.utf8.elementsEqual(b.needle.utf8) && a.originalHex == b.originalHex
    }
}

struct AtlasReaderHitSelection: Sendable {
    let target: AtlasReaderHitTarget
    let utf8Start: UInt64
    let utf8End: UInt64
}

struct AtlasReaderFindReport: Sendable {
    static let maxHits: UInt64 = 4096
    /// Workspace matches can carry a longer original spelling after Unicode
    /// lowercase comparison. Find still verifies that spelling exactly.
    static let maximumNeedleBytes = 4096
    static let maximumExpressionBytes = 1024
    static let maximumWitnessBytes: UInt64 = 16_384
    static let maximumMatchedTextBytes = 16_384
    let identity: AtlasReaderIdentity
    let generation: UInt64
    let needle: String
    let options: AtlasReaderFindOptions
    let expression: AtlasReaderFindExpression?
    let retainedLimit: UInt64
    /// Nil identifies a legacy, single-table response without paging authority.
    let pageStartAfter: UInt64?
    let skippedMatches: UInt64
    let hasMoreMatches: Bool
    let complete: Bool
    let state: String
    let matchesSeen: UInt64
    let hits: [Hit]
    let originalHex: String?
    struct Hit: Sendable {
        let index: UInt64
        let occurrence: UInt64
        let start: UInt64
        let end: UInt64
        let matchedText: String
        let originalHex: String
    }

    static func validateNeedle(_ needle: String, mode: AtlasReaderFindMode = .literal) throws {
        let limit = mode == .expression ? maximumExpressionBytes : maximumNeedleBytes
        guard !needle.isEmpty, needle.utf8.count <= limit, !needle.utf8.contains(0) else {
            throw AtlasReaderError.invalidInput
        }
    }
    static func validateMatchedText(_ text: String) throws {
        guard !text.isEmpty, text.utf8.count <= maximumMatchedTextBytes, !text.utf8.contains(0) else {
            throw AtlasReaderError.invalidInput
        }
    }
    func target(at index: Int) -> AtlasReaderHitTarget? {
        guard hits.indices.contains(index) else { return nil }
        let hit = hits[index]
        return AtlasReaderHitTarget(generation: generation, index: hit.index, start: hit.start,
            end: hit.end, needle: hit.matchedText, originalHex: hit.originalHex)
    }
    var summary: String {
        if let expression {
            if expression.qualified == nil {
                return "Expression search incomplete: \(incompleteReason). No matches are available until the complete retained file satisfies every term."
            }
            if expression.qualified == false {
                return "No matches satisfy this expression in the retained file."
            }
        }
        let comparison = (options.caseSensitive ? "exact" : "case-insensitive")
            + (expression == nil ? "" : " primary-term")
        if pageStartAfter != nil, let first = hits.first, let last = hits.last {
            let count = complete ? "\(matchesSeen)" : "at least \(matchesSeen)"
            let more = hasMoreMatches ? " Next continues to later matches." : ""
            if complete || state == "match-limit" {
                return "Matches \(first.occurrence)–\(last.occurrence) of \(count) \(comparison) matches in the retained file.\(more)"
            }
        }
        if complete {
            if pageStartAfter != nil {
                return "\(matchesSeen) \(comparison) matches counted in the retained file; no matches on this page."
            }
            if matchesSeen > UInt64(hits.count) {
                return "\(matchesSeen) \(comparison) matches counted in the retained file; first \(hits.count) retained for navigation."
            }
            return "\(matchesSeen) \(comparison) matches in the retained file."
        }
        return "Partial file search: \(matchesSeen) matches counted, \(hits.count) retained; \(incompleteReason). More matches may exist."
    }
    private var incompleteReason: String {
        switch state {
        case "match-limit": "match limit reached"
        case "byte-limit": "scan byte limit reached"
        case "read-call-limit": "read-call limit reached"
        case "short-read": "source observation incomplete"
        case "unsupported-text": "unsupported text encountered"
        default: "coverage incomplete"
        }
    }

    static func decode(_ json: String?, info: AtlasReaderInfo, generation: UInt64, needle: String,
                       options: AtlasReaderFindOptions = .init(), startAfter: UInt64? = nil) throws -> Self {
        try validateNeedle(needle, mode: options.mode)
        let data = try AtlasReaderWire.admit(json, handle: info.identity.owner)
        do {
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            let identity = try wire.header.identity(handle: info.identity.owner)
            let expression = try wire.expression.admit(mode: options.mode, query: needle)
            let primaryNeedle = expression?.primaryNeedle ?? needle
            let expectedMode = options.mode == .expression ? "decoded-text-expression"
                : (options.caseSensitive ? "exact-decoded-literal" : "unicode-lowercase-literal")
            guard identity.matches(info.identity), wire.header.command == "find", generation > 0,
                  AtlasReaderWire.integer(wire.query_generation) == generation,
                  wire.mode == expectedMode,
                  wire.needle.utf8.elementsEqual(needle.utf8),
                  (wire.case_sensitive ?? true) == options.caseSensitive,
                  (wire.count_all_matches ?? false) == options.countAllMatches,
                  let seen = AtlasReaderWire.integer(wire.matches_seen),
                  // Exact and declared Unicode-lowercase occurrences cannot
                  // outnumber original input bytes, even when rows are omitted.
                  seen <= identity.capturedBytes,
                  let retained = AtlasReaderWire.integer(wire.retained_hits), retained <= maxHits,
                  let retainedLimit = AtlasReaderWire.integer(wire.retained_limit ?? String(maxHits)),
                  retained <= retainedLimit, retainedLimit <= maxHits,
                  retained == UInt64(wire.hits.count), seen >= retained,
                  let scanned = AtlasReaderWire.integer(wire.scanned_bytes), scanned <= identity.capturedBytes,
                  ["complete-observed-input", "match-limit", "byte-limit", "read-call-limit", "short-read", "unsupported-text"].contains(wire.state),
                  wire.search_complete == (wire.state == "complete-observed-input") else {
                throw AtlasReaderError.invalidResponse
            }
            if let expression {
                guard startAfter != nil, wire.state != "match-limit",
                      wire.search_complete == (expression.qualified != nil),
                      (expression.qualified == true ? seen > 0 : (seen == 0 && retained == 0)) else {
                    throw AtlasReaderError.invalidResponse
                }
            }
            let skipped: UInt64
            let more: Bool
            if let startAfter {
                guard startAfter <= identity.capturedBytes,
                      let cursor = wire.page_start_after, AtlasReaderWire.integer(cursor) == startAfter,
                      let skippedText = wire.skipped_matches,
                      let actualSkipped = AtlasReaderWire.integer(skippedText),
                      actualSkipped == min(startAfter, seen),
                      retained == min(retainedLimit, seen - actualSkipped),
                      let hasMore = wire.has_more_matches,
                      hasMore == (seen - actualSkipped > retained),
                      retainedLimit > 0 else { throw AtlasReaderError.invalidResponse }
                skipped = actualSkipped
                more = hasMore
            } else {
                guard wire.page_start_after == nil, wire.skipped_matches == nil,
                      wire.has_more_matches == nil else { throw AtlasReaderError.invalidResponse }
                skipped = 0
                more = seen > retained
            }
            if options != .init() || startAfter != nil {
                guard wire.case_sensitive != nil, wire.count_all_matches != nil,
                      wire.retained_limit != nil, wire.match_count_complete == wire.search_complete,
                      wire.rows_truncated == (seen > retained) else { throw AtlasReaderError.invalidResponse }
            }
            if wire.search_complete {
                guard (expression != nil || options.countAllMatches || seen - skipped == retained), wire.unsupported_at == nil,
                      scanned == identity.capturedBytes else { throw AtlasReaderError.invalidResponse }
            }
            if let unsupported = wire.unsupported_at {
                guard wire.state == "unsupported-text", let offset = AtlasReaderWire.integer(unsupported),
                      offset <= identity.capturedBytes else { throw AtlasReaderError.invalidResponse }
            }
            let witnessBytes: UInt64
            if options.caseSensitive, let hex = wire.literal_original_hex {
                // Exact UTF-16 can use twice the UTF-8 query byte count. This is
                // a marshaling bound, not a second source-encoding algorithm.
                witnessBytes = UInt64(hex.utf8.count / 2)
                guard retained > 0, witnessBytes > 0, witnessBytes <= maximumWitnessBytes,
                      AtlasReaderWire.validHex(hex, bytes: witnessBytes) else { throw AtlasReaderError.invalidResponse }
            } else {
                guard !options.caseSensitive || retained == 0,
                      wire.literal_original_hex == nil else { throw AtlasReaderError.invalidResponse }
                witnessBytes = 0
            }
            var occurrences: Set<UInt64> = []
            let hits = try wire.hits.enumerated().map { index, hit in
                let range = try hit.original_range.values()
                let text = options.caseSensitive ? primaryNeedle : hit.matched_text
                let hex = options.caseSensitive ? wire.literal_original_hex : hit.original_hex
                guard let text, let hex else { throw AtlasReaderError.invalidResponse }
                do { try validateMatchedText(text) }
                catch { throw AtlasReaderError.invalidResponse }
                let bytes = options.caseSensitive ? witnessBytes : UInt64(hex.utf8.count / 2)
                guard AtlasReaderWire.integer(hit.hit_index) == UInt64(index),
                      let occurrence = AtlasReaderWire.integer(hit.occurrence_id),
                      occurrence > 0, occurrence <= seen,
                      occurrences.insert(occurrence).inserted,
                      range.start < range.end, range.end <= identity.capturedBytes,
                      bytes > 0, bytes <= maximumWitnessBytes,
                      AtlasReaderWire.validHex(hex, bytes: bytes),
                      range.end - range.start == bytes else { throw AtlasReaderError.invalidResponse }
                if let startAfter {
                    let (expected, overflow) = startAfter.addingReportingOverflow(UInt64(index) + 1)
                    guard !overflow, occurrence == expected else { throw AtlasReaderError.invalidResponse }
                }
                return Hit(index: UInt64(index), occurrence: occurrence, start: range.start, end: range.end,
                    matchedText: text, originalHex: hex)
            }
            return Self(identity: identity, generation: generation, needle: needle, options: options,
                expression: expression, retainedLimit: retainedLimit,
                pageStartAfter: startAfter, skippedMatches: skipped, hasMoreMatches: more,
                complete: wire.search_complete, state: wire.state, matchesSeen: seen,
                hits: hits, originalHex: wire.literal_original_hex)
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }

    private struct Wire: Decodable {
        let header: AtlasReaderWire.Header
        let expression: ExpressionWire
        let query_generation, mode, needle, state, scanned_bytes, matches_seen, retained_hits: String
        let search_complete: Bool
        let case_sensitive, count_all_matches, match_count_complete, rows_truncated: Bool?
        let retained_limit: String?
        let page_start_after, skipped_matches: String?
        let has_more_matches: Bool?
        let literal_original_hex, unsupported_at: String?
        let hits: [HitWire]
        enum CodingKeys: CodingKey {
            case query_generation, mode, needle, state, scanned_bytes, matches_seen, retained_hits,
                 search_complete, literal_original_hex, unsupported_at, hits,
                 case_sensitive, count_all_matches, match_count_complete, rows_truncated, retained_limit,
                 page_start_after, skipped_matches, has_more_matches
        }
        struct HitWire: Decodable {
            let hit_index, occurrence_id: String
            let original_range: AtlasReaderWire.Range
            let matched_text, original_hex: String?
        }
        init(from decoder: Decoder) throws {
            header = try .init(from: decoder)
            expression = try .init(from: decoder)
            let c = try decoder.container(keyedBy: CodingKeys.self)
            query_generation = try c.decode(String.self, forKey: .query_generation)
            mode = try c.decode(String.self, forKey: .mode)
            needle = try c.decode(String.self, forKey: .needle)
            state = try c.decode(String.self, forKey: .state)
            scanned_bytes = try c.decode(String.self, forKey: .scanned_bytes)
            matches_seen = try c.decode(String.self, forKey: .matches_seen)
            retained_hits = try c.decode(String.self, forKey: .retained_hits)
            search_complete = try c.decode(Bool.self, forKey: .search_complete)
            case_sensitive = try c.decodeIfPresent(Bool.self, forKey: .case_sensitive)
            count_all_matches = try c.decodeIfPresent(Bool.self, forKey: .count_all_matches)
            match_count_complete = try c.decodeIfPresent(Bool.self, forKey: .match_count_complete)
            rows_truncated = try c.decodeIfPresent(Bool.self, forKey: .rows_truncated)
            retained_limit = try c.decodeIfPresent(String.self, forKey: .retained_limit)
            page_start_after = try c.decodeIfPresent(String.self, forKey: .page_start_after)
            skipped_matches = try c.decodeIfPresent(String.self, forKey: .skipped_matches)
            has_more_matches = try c.decodeIfPresent(Bool.self, forKey: .has_more_matches)
            literal_original_hex = try c.decodeIfPresent(String.self, forKey: .literal_original_hex)
            unsupported_at = try c.decodeIfPresent(String.self, forKey: .unsupported_at)
            var rows = try c.nestedUnkeyedContainer(forKey: .hits)
            if let count = rows.count, count > Int(AtlasReaderFindReport.maxHits) { throw AtlasReaderError.invalidResponse }
            var admitted: [HitWire] = []
            while !rows.isAtEnd {
                guard admitted.count < Int(AtlasReaderFindReport.maxHits) else { throw AtlasReaderError.invalidResponse }
                admitted.append(try rows.decode(HitWire.self))
            }
            hits = admitted
        }
    }

    /// Null qualification means the bounded scan could not decide the complete
    /// conjunction/exclusion predicate. An omitted key is a malformed receipt.
    private struct ExpressionWire: Decodable {
        let anyPresent, allPresent, revalidationPresent: Bool
        let requested_mode, realized_mode, expression, primary_needle, query_syntax, predicate_scope: String?
        let expression_qualified: Bool?
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            let required: [CodingKeys] = [.requested_mode, .realized_mode, .expression, .primary_needle,
                .query_syntax, .predicate_scope, .expression_qualified]
            revalidationPresent = c.contains(.expression_revalidated)
            anyPresent = required.contains { c.contains($0) } || revalidationPresent
            allPresent = required.allSatisfy { c.contains($0) }
            requested_mode = try c.decodeIfPresent(String.self, forKey: .requested_mode)
            realized_mode = try c.decodeIfPresent(String.self, forKey: .realized_mode)
            expression = try c.decodeIfPresent(String.self, forKey: .expression)
            primary_needle = try c.decodeIfPresent(String.self, forKey: .primary_needle)
            query_syntax = try c.decodeIfPresent(String.self, forKey: .query_syntax)
            predicate_scope = try c.decodeIfPresent(String.self, forKey: .predicate_scope)
            expression_qualified = try c.decodeIfPresent(Bool.self, forKey: .expression_qualified)
        }
        func admit(mode: AtlasReaderFindMode, query: String) throws -> AtlasReaderFindExpression? {
            guard mode == .expression else {
                guard !anyPresent else { throw AtlasReaderError.invalidResponse }
                return nil
            }
            guard allPresent, !revalidationPresent, requested_mode == "expression",
                  realized_mode == "decoded-text-expression", let expression, let primary_needle,
                  expression.utf8.elementsEqual(query.utf8), !primary_needle.isEmpty,
                  primary_needle.utf8.count <= AtlasReaderFindReport.maximumExpressionBytes, !primary_needle.utf8.contains(0),
                  query_syntax == "fcb-bounded-expression/1", predicate_scope == "retained-source-capture" else {
                throw AtlasReaderError.invalidResponse
            }
            return .init(query: expression, primaryNeedle: primary_needle, qualified: expression_qualified)
        }
        private enum CodingKeys: CodingKey {
            case requested_mode, realized_mode, expression, primary_needle, query_syntax, predicate_scope,
                 expression_qualified, expression_revalidated
        }
    }
}
