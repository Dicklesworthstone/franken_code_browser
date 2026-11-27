import Foundation

enum AtlasSavedSearchMode: String, CaseIterable, Identifiable, Sendable {
    case literal, expression, symbols, headings, occurrences
    var id: String { rawValue }
    var label: String {
        switch self {
        case .literal: return "Literal"
        case .expression: return "Expression"
        case .symbols: return "Symbols"
        case .headings: return "Headings"
        case .occurrences: return "Occurrences"
        }
    }
    var structure: AtlasSearchMode? {
        switch self {
        case .symbols: return .symbols
        case .headings: return .headings
        case .occurrences: return .occurrences
        case .literal, .expression: return nil
        }
    }
    var supportsNameMatching: Bool { structure?.supportsNameMatching == true }
    func effectiveNameMode(_ requested: AtlasSearchNameMode) -> AtlasSearchNameMode {
        self == .occurrences ? .exact : requested
    }
}

enum AtlasSavedSearchWire {
    static let maximumNeedleBytes = 1024
    static let maximumHits: UInt64 = 4096
    static let maximumSourceBytes: UInt64 = 64 * 1024 * 1024

    /// The counted Swift bytes are exactly the UTF-8 submitted through the
    /// existing C-string API. Whitespace is query data, never trimmed.
    static func validNeedle(_ needle: String) -> Bool {
        !needle.isEmpty && needle.utf8.count <= maximumNeedleBytes && !needle.utf8.contains(0)
    }
}

/// Raw saved membership shared by every search mode. The original type name
/// remains source-compatible with existing structural search integrations.
enum AtlasSavedStructureScope: Equatable, Sendable {
    case archive, directory([UInt8]), member([UInt8])

    var path: [UInt8] {
        switch self { case .archive: return []; case .directory(let path), .member(let path): return path }
    }
    var code: UInt64 {
        switch self { case .archive: return 0; case .directory: return 1; case .member: return 2 }
    }
    var label: String {
        switch self {
        case .archive: return "Whole archive"
        case .directory, .member:
            let name = path.map { byte in
                (32...126).contains(byte) && byte != 92 ? String(UnicodeScalar(byte)) : String(format: "\\x%02X", byte)
            }.joined()
            return self.code == 1 ? "Folder: " + name : "File: " + name
        }
    }
    func includes(_ candidate: [UInt8]) -> Bool {
        switch self {
        case .archive: return true
        case .member(let path): return candidate == path
        case .directory(let path): return candidate.count > path.count && candidate.starts(with: path) && candidate[path.count] == 47
        }
    }
    static func admit(_ label: String?, pathHex: String?) throws -> Self {
        if label == nil || label == "whole-saved-archive" {
            guard pathHex == nil || pathHex == "" else { throw AtlasSavedError.invalidResponse }
            return .archive
        }
        guard let pathHex, !pathHex.isEmpty, pathHex.utf8.count <= 4096 else { throw AtlasSavedError.invalidResponse }
        let path = try AtlasReaderWire.pathBytes(pathHex)
        guard !path.contains(0), !path.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
            $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
        }) else { throw AtlasSavedError.invalidResponse }
        switch label { case "saved-directory": return .directory(path); case "saved-member": return .member(path)
        default: throw AtlasSavedError.invalidResponse }
    }
}

/// Scope and coverage are part of the accepted query, independent of editable
/// controls and of the archive's broader discovery and capture coverage.
struct AtlasSavedSearchScopeReceipt: Equatable, Sendable {
    let scope: AtlasSavedStructureScope
    let members, captured: UInt64
    let complete: Bool

    static func decode(_ data: Data, identity: AtlasSavedIdentity,
        expected: AtlasSavedStructureScope? = nil) throws -> Self {
        struct Wire: Decodable {
            let scope, scope_path_hex: String?
            let scope_known_members, scope_captured_members: AtlasSavedNumber?
            let scope_membership_complete: Bool?
        }
        let w = try JSONDecoder().decode(Wire.self, from: data)
        let hasReceipt = w.scope != nil || w.scope_path_hex != nil || w.scope_known_members != nil
            || w.scope_captured_members != nil || w.scope_membership_complete != nil
        if hasReceipt {
            guard w.scope != nil, w.scope_path_hex != nil, w.scope_known_members != nil, w.scope_captured_members != nil,
                  w.scope_membership_complete != nil else { throw AtlasSavedError.invalidResponse }
        }
        let scope = try AtlasSavedStructureScope.admit(w.scope, pathHex: w.scope_path_hex)
        let members = w.scope_known_members?.value ?? identity.members
        let captured = w.scope_captured_members?.value ?? identity.captured
        let complete: Bool
        switch scope {
        case .archive:
            guard members == identity.members, captured == identity.captured else {
                throw AtlasSavedError.invalidResponse
            }
            complete = identity.discoveryComplete
        case .directory:
            guard members <= identity.members else { throw AtlasSavedError.invalidResponse }
            complete = identity.discoveryComplete
        case .member:
            guard members <= min(1, identity.members) else { throw AtlasSavedError.invalidResponse }
            complete = members == 1
        }
        guard expected == nil || scope == expected!, captured <= members,
              identity.captured <= identity.members, captured <= identity.captured,
              members - captured <= identity.members - identity.captured,
              (w.scope_membership_complete ?? complete) == complete else {
            throw AtlasSavedError.invalidResponse
        }
        return Self(scope: scope, members: members, captured: captured, complete: complete)
    }
}

struct AtlasSavedStructurePolicy: Equatable, Sendable {
    let caseSensitive, countAllMatches: Bool
    var scope: AtlasSavedStructureScope = .archive
    static let standard = Self(caseSensitive: true, countAllMatches: false)

    /// Older pages carry only name_case; older reader receipts omit it.
    static func admit(caseSensitive: Bool?, countAll: Bool?, nameCase: String?, mode: AtlasSavedSearchMode, scope: String? = nil, pathHex: String? = nil) throws -> Self {
        let policy = try Self(caseSensitive: caseSensitive ?? true, countAllMatches: countAll ?? false,
            scope: AtlasSavedStructureScope.admit(scope, pathHex: pathHex))
        guard mode.structure != nil, (caseSensitive == nil) == (countAll == nil) else {
            throw AtlasSavedError.invalidResponse
        }
        if caseSensitive != nil || nameCase != nil {
            guard nameCase == (policy.caseSensitive ? "sensitive" : "unicode-lowercase") else {
                throw AtlasSavedError.invalidResponse
            }
        }
        return policy
    }
}

/// Exhaustive occurrence totals and retained navigation rows are independent.
/// New receipts must carry their entire count policy; legacy receipts retain
/// the old one-omitted-match lower bound and cannot satisfy a count request.
struct AtlasSavedSearchCount: Equatable, Sendable {
    let all, exact: Bool
    let maximum: UInt64
    let matchingFiles, retainedMatchingFiles: UInt64?

    static func admit(all: Bool?, exact: Bool?, matchingFiles: UInt64?, retainedFiles: UInt64?,
        expected: Bool?, complete: Bool, truncated: Bool, retained: UInt64, seen: UInt64,
        maximum: UInt64, availableFiles: UInt64, scannedBytes: UInt64, requiresReceipt: Bool = false,
        skipped: UInt64 = 0, hasMore: Bool? = nil) throws -> Self {
        let hasReceipt = requiresReceipt || all != nil || matchingFiles != nil || retainedFiles != nil
        let counted = all ?? false
        let later = hasMore ?? truncated
        guard skipped <= seen, retained <= seen - skipped,
              later == (seen - skipped > retained),
              expected == nil || counted == expected!, maximum <= AtlasSavedSearchWire.maximumHits,
              retained <= maximum, seen >= retained, truncated == (seen > retained),
              (exact ?? complete) == complete else { throw AtlasSavedError.invalidResponse }
        if hasReceipt {
            guard all != nil, exact != nil, let matchingFiles, let retainedFiles,
                  matchingFiles <= availableFiles, matchingFiles <= seen,
                  retainedFiles <= matchingFiles, retainedFiles <= retained,
                  (matchingFiles == 0) == (seen == 0), (retainedFiles == 0) == (retained == 0) else {
                throw AtlasSavedError.invalidResponse
            }
        }
        if counted {
            guard hasReceipt, seen <= scannedBytes,
                  !later || retained == maximum else { throw AtlasSavedError.invalidResponse }
        } else {
            guard seen - skipped == retained + (later ? 1 : 0), !complete || !later else {
                throw AtlasSavedError.invalidResponse
            }
        }
        return Self(all: counted, exact: exact ?? complete, maximum: maximum,
            matchingFiles: matchingFiles, retainedMatchingFiles: retainedFiles)
    }

    func validatePage(_ hits: [AtlasSavedHit], retained: UInt64) throws {
        if let retainedMatchingFiles {
            let files = UInt64(Set(hits.map(\.member)).count)
            guard files <= retainedMatchingFiles,
                  UInt64(hits.count) != retained || files == retainedMatchingFiles else {
                throw AtlasSavedError.invalidResponse
            }
        }
    }

    func summary(seen: UInt64, retained: UInt64, skipped: UInt64 = 0) -> String {
        let rows = skipped == 0 ? "first \(retained) retained" : "\(retained) retained after \(skipped) matches"
        if exact {
            let files = matchingFiles.map { " in \($0) captured files" } ?? ""
            return "\(seen) matches\(files)" + (seen > retained ? "; \(rows)" : "")
        }
        return "At least \(seen) matches; \(rows) · partial search"
    }
}

/// A fresh query owns each bounded result window. The offset is a count of
/// qualified matches, never a member ordinal or a retained metadata-page offset.
/// Legacy responses describe only the first window; an explicit window request
/// requires all four receipt fields, including a present nullable continuation.
struct AtlasSavedResultWindow: Equatable, Sendable {
    let offset, skipped: UInt64
    let hasMore: Bool
    let next: UInt64?

    static func decode(_ data: Data, expected: UInt64?, seen: UInt64, retained: UInt64,
        omitted: UInt64 = 0, legacyMore: Bool, permitsContinuation: Bool = true) throws -> Self {
        let raw = try JSONDecoder().decode(Wire.self, from: data)
        let offset = raw.offset ?? 0
        let skipped = raw.skipped ?? 0
        guard expected == nil || (raw.offset != nil && expected == offset),
              skipped == min(offset, seen), skipped <= seen,
              retained <= seen - skipped, omitted <= seen - skipped - retained else {
            throw AtlasSavedError.invalidResponse
        }
        let end = skipped + retained + omitted
        let more = raw.more ?? legacyMore
        // Literal/expression count-only queries retain omitted-match evidence
        // without an advancing cursor. Structural zero-row queries suppress
        // that evidence as well through permitsContinuation.
        let continuation: UInt64? = more && retained > 0 && end > offset ? end : nil
        let next = raw.offset == nil ? continuation : raw.next
        guard more == (permitsContinuation && seen > end), next == continuation else {
            throw AtlasSavedError.invalidResponse
        }
        return Self(offset: offset, skipped: skipped, hasMore: more, next: next)
    }

    var earlier: UInt64? { offset == 0 ? nil : offset > AtlasSavedSearchWire.maximumHits
        ? offset - AtlasSavedSearchWire.maximumHits : 0 }

    func summary(retained: UInt64) -> String {
        "Window after \(skipped) matches · \(retained) retained"
    }

    private struct Wire: Decodable {
        let offset, skipped, next: UInt64?
        let more: Bool?
        enum CodingKeys: CodingKey { case result_offset, skipped_matches, has_more_matches, next_match_offset }
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            let keys: [CodingKeys] = [.result_offset, .skipped_matches, .has_more_matches, .next_match_offset]
            if keys.contains(where: { c.contains($0) }) {
                guard keys.allSatisfy({ c.contains($0) }) else { throw AtlasSavedError.invalidResponse }
                offset = try c.decode(AtlasSavedNumber.self, forKey: .result_offset).value
                skipped = try c.decode(AtlasSavedNumber.self, forKey: .skipped_matches).value
                more = try c.decode(Bool.self, forKey: .has_more_matches)
                next = try c.decode(AtlasSavedSearchOptional<AtlasSavedNumber>.self, forKey: .next_match_offset).value?.value
            } else { offset = nil; skipped = nil; more = nil; next = nil }
        }
    }
}

/// An original-byte occurrence, qualified by its containing accepted page.
/// A UTF-16 match can occupy a different byte length from the UTF-8 needle.
struct AtlasSavedHit: Decodable, Sendable, Identifiable {
    let id: UInt64
    let member: UInt64
    let file: UInt64
    let revision: UInt64
    let digest: String
    let path: AtlasSavedPath
    let start: UInt64
    let end: UInt64

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(AtlasSavedNumber.self, forKey: .hit_id).value
        member = try c.decode(AtlasSavedNumber.self, forKey: .member).value
        file = try c.decode(AtlasSavedNumber.self, forKey: .file_id).value
        revision = try c.decode(AtlasSavedNumber.self, forKey: .source_revision).value
        digest = try c.decode(String.self, forKey: .source_digest)
        path = try c.decode(AtlasSavedPath.self, forKey: .path)
        let range = try c.decode(AtlasReaderWire.Range.self, forKey: .original_range).values()
        start = range.start; end = range.end
        guard (1...AtlasSavedSearchWire.maximumHits).contains(id), member < 65_536,
              file == member + 1, revision == file, AtlasSavedWire.digest(digest), path.isMember,
              start < end, end <= AtlasSavedWire.memberBytes else {
            throw AtlasSavedError.invalidResponse
        }
    }

    func matches(_ other: Self) -> Bool {
        id == other.id && member == other.member && file == other.file && revision == other.revision &&
            digest == other.digest && path.bytes == other.path.bytes && start == other.start && end == other.end
    }

    enum CodingKeys: CodingKey { case hit_id, member, file_id, source_revision, source_digest, path, original_range }
}

/// Required nullable wire fields must be present even when they contain null.
/// This avoids silently treating an omitted accepted-generation or continuation
/// field as an authoritative empty result.
struct AtlasSavedSearchOptional<Value: Decodable>: Decodable {
    let value: Value?
    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { value = nil }
        else { value = try c.decode(Value.self) }
    }
}

/// The shared extractors own names, slugs and source coordinates. Native code
/// only admits bounded, mode-qualified metadata from the accepted archive.
struct AtlasSavedStructureWitness: Decodable, Sendable {
    let value: AtlasSearchStructure
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let name = try c.decode(String.self, forKey: .name)
        let kind = try c.decode(String.self, forKey: .kind)
        let language = try c.decode(String.self, forKey: .language)
        let evidence = try c.decode(String.self, forKey: .evidence_level)
        let candidate = try c.decode(AtlasSavedNumber.self, forKey: .candidate_id).value
        let slug = try c.decode(AtlasSavedSearchOptional<String>.self, forKey: .heading_slug).value
        let range = try c.decode(AtlasReaderWire.Range.self, forKey: .evidence_range).values()
        guard !name.isEmpty, name.utf8.count <= 4096, !name.utf8.contains(0),
              !kind.isEmpty, kind.utf8.count <= 64, !kind.utf8.contains(0),
              !language.isEmpty, language.utf8.count <= 64, !language.utf8.contains(0),
              (1...4096).contains(candidate), range.start < range.end, range.end <= AtlasSavedWire.memberBytes,
              slug == nil || (!slug!.isEmpty && slug!.utf8.count <= 4096 && !slug!.utf8.contains(0)) else {
            throw AtlasSavedError.invalidResponse
        }
        value = AtlasSearchStructure(name: name, kind: kind, language: language, evidence: evidence,
            candidate: candidate, headingSlug: slug, evidenceStart: range.start, evidenceEnd: range.end)
    }
    enum CodingKeys: CodingKey { case name, kind, language, evidence_level, candidate_id, heading_slug, evidence_range }
}

struct AtlasSavedStructureHit: Decodable, Sendable, Identifiable {
    let hit: AtlasSavedHit
    let structure: AtlasSearchStructure
    var id: UInt64 { hit.id }
    init(from decoder: Decoder) throws {
        hit = try AtlasSavedHit(from: decoder)
        let c = try decoder.container(keyedBy: CodingKeys.self)
        structure = try c.decode(AtlasSavedStructureWitness.self, forKey: .structure).value
        guard structure.evidenceStart <= hit.start, hit.end <= structure.evidenceEnd else {
            throw AtlasSavedError.invalidResponse
        }
    }
    func validate(mode: AtlasSavedSearchMode, needle: String, caseSensitive: Bool = true) throws {
        guard let expected = mode.structure, structure.mode == expected, structure.evidence == expected.evidence,
              structure.evidenceEnd <= expected.structureFileByteLimit else { throw AtlasSavedError.invalidResponse }
        switch mode {
        case .symbols:
            guard structure.headingSlug == nil,
                  AtlasOutlineLanguage(rawValue: structure.language)?.engineName != nil else {
                throw AtlasSavedError.invalidResponse
            }
        case .headings:
            guard structure.headingSlug != nil, structure.kind == "heading", structure.language == "markdown",
                  structure.evidenceStart == hit.start, structure.evidenceEnd == hit.end else {
                throw AtlasSavedError.invalidResponse
            }
        case .occurrences:
            guard structure.headingSlug == nil, structure.kind == "identifier-occurrence", structure.language == "text",
                  AtlasSearchOccurrencePolicy.matches(source: structure.name, query: needle, caseSensitive: caseSensitive),
                  structure.evidenceStart == hit.start, structure.evidenceEnd == hit.end,
                  hit.end - hit.start == UInt64(structure.name.utf8.count)
                    || hit.end - hit.start == UInt64(structure.name.utf16.count * 2) else {
                throw AtlasSavedError.invalidResponse
            }
        case .literal, .expression: throw AtlasSavedError.invalidResponse
        }
    }
    func matches(_ other: Self) -> Bool { hit.matches(other.hit) && structure == other.structure }
    enum CodingKeys: CodingKey { case structure }
}

/// Completeness describes admitted extraction processing. It never upgrades
/// heuristic declarations or whole-token occurrences into semantic references.
struct AtlasSavedStructurePage: Sendable {
    let identity: AtlasSavedIdentity
    let generation: UInt64
    let needle: String
    let mode: AtlasSavedSearchMode
    let nameMode: AtlasSearchNameMode
    let hits: [AtlasSavedStructureHit]
    let start: UInt64
    let next: UInt64?
    let state: String
    let complete, truncated, matchesSeenExact: Bool
    let policy: AtlasSavedStructurePolicy
    let scopeMembers: UInt64
    let maximum: UInt64
    let retained, seen, visited, loaded, searched, unavailable, unsupported: UInt64
    let sourceLimited, extractionLimited, metadataOmitted, sourceBytes, sourceLimit, metadataBytes: UInt64
    let peakSourceBytes, memberReadCalls: UInt64
    let window: AtlasSavedResultWindow

    /// Metadata pagination belongs to one immutable accepted extraction. A new
    /// page may replace its rows, never its limits, coverage or source-I/O facts.
    func hasSameSummary(as other: Self) -> Bool {
        identity == other.identity && generation == other.generation
            && needle.utf8.elementsEqual(other.needle.utf8) && mode == other.mode && nameMode == other.nameMode
            && policy == other.policy && scopeMembers == other.scopeMembers && maximum == other.maximum
            && window == other.window && state == other.state && complete == other.complete
            && truncated == other.truncated && matchesSeenExact == other.matchesSeenExact
            && retained == other.retained && seen == other.seen
            && visited == other.visited && loaded == other.loaded && searched == other.searched
            && unavailable == other.unavailable && unsupported == other.unsupported
            && sourceLimited == other.sourceLimited && extractionLimited == other.extractionLimited
            && metadataOmitted == other.metadataOmitted && sourceBytes == other.sourceBytes
            && sourceLimit == other.sourceLimit && metadataBytes == other.metadataBytes
            && peakSourceBytes == other.peakSourceBytes && memberReadCalls == other.memberReadCalls
    }

    static func decode(_ json: String?, identity: AtlasSavedIdentity, generation: UInt64,
        mode expectedMode: AtlasSavedSearchMode?, nameMode expectedName: AtlasSearchNameMode?,
        needle expectedNeedle: String?, start: UInt64, limit: UInt64,
        resultOffset expectedOffset: UInt64? = nil, policy expectedPolicy: AtlasSavedStructurePolicy? = nil) throws -> Self {
        let data = try AtlasSavedWire.admit(json, handle: identity.owner, command: "structure", identity: identity)
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            guard let mode = AtlasSavedSearchMode(rawValue: w.requested_mode), let engine = mode.structure,
                  let nameMode = AtlasSearchNameMode(rawValue: w.name_mode),
                  expectedMode == nil || mode == expectedMode, expectedName == nil || nameMode == expectedName,
                  mode != .occurrences || nameMode == .exact,
                  generation > 0, w.structure_generation.value == generation,
                  w.selection_namespace == "saved-structure", w.mode == engine.wireMode,
                  w.realized_mode == engine.evidence, w.evidence_level == engine.evidence,
                  !w.semantic_complete,
                  w.completeness_basis == "admitted-extraction-processing", !w.index_used,
                  w.index_policy == "direct-verified-member-extraction",
                  AtlasSavedSearchWire.validNeedle(w.needle),
                  expectedNeedle == nil || w.needle.utf8.elementsEqual(expectedNeedle!.utf8) else {
                throw AtlasSavedError.invalidResponse
            }
            let policy = try AtlasSavedStructurePolicy.admit(caseSensitive: w.case_sensitive, countAll: w.count_all_matches,
                nameCase: w.name_case, mode: mode, scope: w.scope, pathHex: w.scope_path_hex)
            guard expectedPolicy.map({ $0 == policy }) ?? true else { throw AtlasSavedError.invalidResponse }
            if mode == .occurrences {
                guard w.includes_comments_and_literals == true, w.compiler_resolved == false,
                      w.token_policy == "ascii-alphanumeric-underscore-dollar-plus-nonascii-nonwhitespace-noncontrol-v1",
                      w.name_syntax == "single-token-no-leading-ascii-digit", w.unicode_normalization == "none" else {
                    throw AtlasSavedError.invalidResponse
                }
            }
            let retained = w.retained_hits.value, omitted = w.metadata_omitted.value
            let window = try AtlasSavedResultWindow.decode(data, expected: expectedOffset,
                seen: w.matches_seen.value, retained: retained, omitted: omitted, legacyMore: w.truncated && w.max_hits.value > 0,
                permitsContinuation: w.max_hits.value > 0)
            let visited = w.members_visited.value, loaded = w.members_loaded.value
            let searched = w.files_searched.value, unavailable = w.unavailable_files.value
            let unsupported = w.unsupported_files.value, limited = w.source_limited_files.value
            let extractionLimited = w.extraction_limited_files.value
            let bytes = w.loaded_bytes.value, calls = w.member_read_calls.value
            let scopeMembers = w.scope_known_members?.value ?? identity.members
            let closed: Bool
            switch policy.scope {
            case .archive:
                guard scopeMembers == identity.members else { throw AtlasSavedError.invalidResponse }
                closed = identity.discoveryComplete
            case .directory:
                guard w.scope_known_members != nil, scopeMembers <= identity.members else { throw AtlasSavedError.invalidResponse }
                closed = identity.discoveryComplete
            case .member:
                guard w.scope_known_members != nil, scopeMembers <= min(1, identity.members) else { throw AtlasSavedError.invalidResponse }
                closed = scopeMembers == 1
            }
            guard w.scope_membership_complete == closed || (policy.scope == .archive && w.scope_membership_complete == nil)
                else { throw AtlasSavedError.invalidResponse }
            let coverage = w.state == "complete" && closed && unavailable == 0 &&
                unsupported == 0 && limited == 0 && extractionLimited == 0
            // Legacy producers treated omitted row metadata as an inexact count.
            let expectedExact = w.case_sensitive == nil ? coverage && omitted == 0 : coverage
            guard ["complete", "truncated", "work-limit"].contains(w.state),
                  w.max_hits.value <= AtlasSavedSearchWire.maximumHits, retained <= w.max_hits.value,
                  w.max_source_bytes.value <= AtlasSavedSearchWire.maximumSourceBytes, bytes <= w.max_source_bytes.value,
                  omitted <= identity.members * 4096,
                  w.truncated == (w.matches_seen.value - window.skipped > retained + omitted),
                  !w.truncated || retained == w.max_hits.value,
                  (w.state == "truncated") == (w.truncated && !policy.countAllMatches),
                  policy.countAllMatches || w.matches_seen.value - window.skipped == retained + omitted + (w.truncated ? 1 : 0),
                  !policy.countAllMatches || w.matches_seen.value <= bytes,
                  visited <= scopeMembers, loaded <= visited, loaded <= identity.captured,
                  searched <= loaded, unavailable <= visited, unavailable <= identity.members - identity.captured,
                  unsupported <= visited - unavailable, limited <= visited - unavailable - unsupported,
                  searched <= visited - unavailable - unsupported - limited,
                  extractionLimited <= loaded,
                  w.state != "complete" || visited == scopeMembers,
                  w.matches_seen_exact == expectedExact, w.complete == (coverage && omitted == 0),
                  w.member_bytes_read.value == bytes,
                  w.peak_source_bytes.value <= engine.structureFileByteLimit, w.peak_source_bytes.value <= bytes,
                  (bytes == 0 && calls == 0) || (bytes > 0 && calls > 0), calls <= loaded * 1_048_576,
                  w.metadata_bytes.value <= 8 * 1024 * 1024, w.retained_source_payload_bytes.value == 0,
                  (1...128).contains(limit), start <= retained,
                  UInt64(w.hits.values.count) == min(limit, retained - start) else { throw AtlasSavedError.invalidResponse }
            var previous: AtlasSavedStructureHit?
            var pageMetadata: UInt64 = 0
            for (position, item) in w.hits.values.enumerated() {
                let hit = item.hit
                guard hit.id == start + UInt64(position) + 1, hit.member < identity.members, policy.scope.includes(hit.path.bytes) else {
                    throw AtlasSavedError.invalidResponse
                }
                try item.validate(mode: mode, needle: w.needle, caseSensitive: policy.caseSensitive)
                if let previous {
                    guard hit.member >= previous.hit.member else { throw AtlasSavedError.invalidResponse }
                    if hit.member == previous.hit.member {
                        guard item.structure.candidate > previous.structure.candidate,
                              hit.path.bytes == previous.hit.path.bytes, hit.digest == previous.hit.digest else {
                            throw AtlasSavedError.invalidResponse
                        }
                    }
                }
                pageMetadata += UInt64(item.structure.name.utf8.count + (item.structure.headingSlug?.utf8.count ?? 0))
                previous = item
            }
            let end = start + UInt64(w.hits.values.count), next = w.next_offset.value?.value
            guard pageMetadata <= w.metadata_bytes.value, next == (end < retained ? end : nil) else {
                throw AtlasSavedError.invalidResponse
            }
            return Self(identity: identity, generation: generation, needle: w.needle, mode: mode, nameMode: nameMode,
                hits: w.hits.values, start: start, next: next, state: w.state, complete: w.complete, truncated: w.truncated,
                matchesSeenExact: w.matches_seen_exact, policy: policy, scopeMembers: scopeMembers, maximum: w.max_hits.value,
                retained: retained, seen: w.matches_seen.value, visited: visited, loaded: loaded, searched: searched,
                unavailable: unavailable, unsupported: unsupported, sourceLimited: limited, extractionLimited: extractionLimited,
                metadataOmitted: omitted, sourceBytes: bytes, sourceLimit: w.max_source_bytes.value, metadataBytes: w.metadata_bytes.value,
                peakSourceBytes: w.peak_source_bytes.value, memberReadCalls: calls, window: window)
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }
    static func decodeClear(_ json: String?, identity: AtlasSavedIdentity, generation: UInt64) throws {
        let data = try AtlasSavedWire.admit(json, handle: identity.owner, command: "structure-clear", identity: identity)
        struct Clear: Decodable {
            let selection_namespace: String
            let structure_generation: AtlasSavedNumber
            let accepted_structure_generation: AtlasSavedSearchOptional<AtlasSavedNumber>
            let retained_hits: AtlasSavedNumber
        }
        do {
            let w = try JSONDecoder().decode(Clear.self, from: data)
            guard generation > 0, w.structure_generation.value == generation, w.selection_namespace == "saved-structure",
                  w.accepted_structure_generation.value == nil, w.retained_hits.value == 0 else {
                throw AtlasSavedError.invalidResponse
            }
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }
    private struct BoundedHits: Decodable {
        let values: [AtlasSavedStructureHit]
        init(from decoder: Decoder) throws {
            var c = try decoder.unkeyedContainer()
            guard c.count.map({ $0 <= 128 }) ?? true else { throw AtlasSavedError.invalidResponse }
            var values: [AtlasSavedStructureHit] = []
            values.reserveCapacity(c.count ?? 0)
            while !c.isAtEnd {
                guard values.count < 128 else { throw AtlasSavedError.invalidResponse }
                values.append(try c.decode(AtlasSavedStructureHit.self))
            }
            self.values = values
        }
    }
    private struct Wire: Decodable {
        let selection_namespace, needle, mode, requested_mode, realized_mode, evidence_level, name_case, name_mode: String
        let scope_path_hex: String?
        let scope_known_members: AtlasSavedNumber?
        let scope_membership_complete: Bool?
        let scope, completeness_basis, state, index_policy: String
        let case_sensitive, count_all_matches: Bool?
        let includes_comments_and_literals, compiler_resolved: Bool?
        let token_policy, name_syntax, unicode_normalization: String?
        let semantic_complete, index_used, complete, matches_seen_exact, truncated: Bool
        let structure_generation, max_hits, max_source_bytes, retained_hits, matches_seen: AtlasSavedNumber
        let members_visited, members_loaded, files_searched, unsupported_files, unavailable_files: AtlasSavedNumber
        let source_limited_files, extraction_limited_files, metadata_omitted: AtlasSavedNumber
        let loaded_bytes, peak_source_bytes, metadata_bytes, member_bytes_read, member_read_calls: AtlasSavedNumber
        let retained_source_payload_bytes: AtlasSavedNumber
        let hits: BoundedHits
        let next_offset: AtlasSavedSearchOptional<AtlasSavedNumber>
    }
}

/// The engine owns parsing and whole-member predicates. Native code validates
/// the accepted syntax and witness namespace without interpreting query text.
struct AtlasSavedExpressionPage: Sendable {
    let identity: AtlasSavedIdentity
    let scope: AtlasSavedSearchScopeReceipt
    let generation: UInt64
    let expression: String
    let primaryNeedle: String
    let caseSensitive: Bool
    var matchingDescription: String { caseSensitive ? "Match case" : "Unicode lowercase matching" }
    let hits: [AtlasSavedHit]
    let start: UInt64
    let next: UInt64?
    let state: String
    let complete: Bool
    let truncated: Bool
    let retained, seen, required, excluded, scopeFiles, filteredFiles, unavailable, unsupported: UInt64
    let sourceBytes, scannedBytes, scanLimit, predicateRejections: UInt64
    let counting: AtlasSavedSearchCount
    let window: AtlasSavedResultWindow

    func hasSameSummary(as other: Self) -> Bool {
        identity == other.identity && scope == other.scope && generation == other.generation
            && expression.utf8.elementsEqual(other.expression.utf8)
            && primaryNeedle.utf8.elementsEqual(other.primaryNeedle.utf8)
            && caseSensitive == other.caseSensitive
            && counting == other.counting && window == other.window && state == other.state && complete == other.complete
            && truncated == other.truncated && retained == other.retained && seen == other.seen
            && scopeFiles == other.scopeFiles && filteredFiles == other.filteredFiles
            && unavailable == other.unavailable && unsupported == other.unsupported
            && sourceBytes == other.sourceBytes && scannedBytes == other.scannedBytes && scanLimit == other.scanLimit
            && required == other.required && excluded == other.excluded && predicateRejections == other.predicateRejections
    }

    static func decode(_ json: String?, identity: AtlasSavedIdentity, generation: UInt64,
        expression expected: String?, start: UInt64, limit: UInt64,
        countAllMatches expectedCount: Bool? = nil, caseSensitive expectedCase: Bool? = nil,
        resultOffset expectedOffset: UInt64? = nil, scope expectedScope: AtlasSavedStructureScope? = nil) throws -> Self {
        let data = try AtlasSavedWire.admit(json, handle: identity.owner, command: "expression", identity: identity)
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            let membership = try AtlasSavedSearchScopeReceipt.decode(data, identity: identity, expected: expectedScope)
            let caseSensitive = w.case_sensitive
            if w.mode != nil || w.normalization != nil || !caseSensitive {
                guard w.mode == (caseSensitive ? "decoded-text-expression" : "unicode-lowercase-expression"),
                      w.normalization == "exact" else { throw AtlasSavedError.invalidResponse }
            }
            let retained = w.retained_hits.value, seen = w.matches_seen.value
            let window = try AtlasSavedResultWindow.decode(data, expected: expectedOffset,
                seen: seen, retained: retained, legacyMore: w.truncated)
            let required = w.required_terms.value, excluded = w.excluded_terms.value
            let scope = w.scope_files.value, unavailable = w.unavailable_files.value
            let loaded = w.members_loaded.value, unsupported = w.unsupported_files.value
            let sourceBytes = w.loaded_bytes.value, calls = w.member_read_calls.value
            let counting = try AtlasSavedSearchCount.admit(all: w.count_all_matches, exact: w.matches_seen_exact,
                matchingFiles: w.matching_files?.value, retainedFiles: w.retained_matching_files?.value,
                expected: expectedCount, complete: w.complete, truncated: w.truncated, retained: retained,
                seen: seen, maximum: w.max_hits.value, availableFiles: loaded, scannedBytes: w.scanned_bytes.value,
                skipped: window.skipped, hasMore: window.hasMore)
            guard generation > 0, w.expression_generation.value == generation,
                  w.selection_namespace == "saved-expression",
                  AtlasSavedSearchWire.validNeedle(w.expression), AtlasSavedSearchWire.validNeedle(w.primary_needle),
                  expected == nil || w.expression.utf8.elementsEqual(expected!.utf8),
                  w.predicate_scope == "whole-captured-member",
                  expectedCase == nil || caseSensitive == expectedCase,
                  !w.index_used, w.index_policy == "direct-verified-member-scan",
                  ["complete", "truncated", "work-limit"].contains(w.state),
                  required <= 63, excluded <= 63 - required,
                  w.max_hits.value <= AtlasSavedSearchWire.maximumHits, retained <= w.max_hits.value,
                  w.max_scan_bytes.value <= AtlasSavedSearchWire.maximumSourceBytes,
                  w.scanned_bytes.value <= w.max_scan_bytes.value,
                  counting.all ? w.state != "truncated" : window.hasMore == (w.state == "truncated"),
                  w.metadata_examined.value == membership.members,
                  scope <= membership.members, w.metadata_excluded.value == membership.members - scope,
                  unavailable <= scope, unavailable <= membership.members - membership.captured,
                  loaded <= scope - unavailable, unsupported <= loaded,
                  w.state != "complete" || loaded == scope - unavailable,
                  w.complete == (w.state == "complete" && membership.complete && unavailable == 0 && unsupported == 0),
                  w.matches_seen_exact == w.complete,
                  sourceBytes <= AtlasSavedSearchWire.maximumSourceBytes, w.member_bytes_read.value == sourceBytes,
                  w.peak_source_bytes.value <= AtlasSavedWire.memberBytes, w.peak_source_bytes.value <= sourceBytes,
                  (sourceBytes == 0 && calls == 0) || (sourceBytes > 0 && calls > 0),
                  calls <= loaded * 1_048_576,
                  w.predicate_scans.value <= loaded * (required + excluded),
                  w.predicate_rejections.value <= loaded, w.primary_scans.value <= loaded,
                  w.retained_source_payload_bytes.value == 0,
                  (1...128).contains(limit), start <= retained,
                  UInt64(w.hits.count) == min(limit, retained - start) else { throw AtlasSavedError.invalidResponse }
            var previous: AtlasSavedHit?
            for (position, hit) in w.hits.enumerated() {
                guard hit.id == start + UInt64(position) + 1, hit.member < identity.members,
                      membership.scope.includes(hit.path.bytes) else {
                    throw AtlasSavedError.invalidResponse
                }
                if let previous {
                    guard hit.member >= previous.member else { throw AtlasSavedError.invalidResponse }
                    if hit.member == previous.member {
                        guard (caseSensitive ? hit.start > previous.start :
                            hit.start >= previous.start && hit.end >= previous.end), hit.path.bytes == previous.path.bytes,
                              hit.digest == previous.digest else { throw AtlasSavedError.invalidResponse }
                    }
                }
                previous = hit
            }
            let end = start + UInt64(w.hits.count), next = w.next_offset.value?.value
            guard next == (end < retained ? end : nil) else { throw AtlasSavedError.invalidResponse }
            try counting.validatePage(w.hits, retained: retained)
            return Self(identity: identity, scope: membership, generation: generation, expression: w.expression, primaryNeedle: w.primary_needle,
                caseSensitive: caseSensitive,
                hits: w.hits, start: start, next: next, state: w.state, complete: w.complete, truncated: w.truncated,
                retained: retained, seen: seen, required: required, excluded: excluded, scopeFiles: scope,
                filteredFiles: w.metadata_excluded.value, unavailable: unavailable, unsupported: unsupported,
                sourceBytes: sourceBytes, scannedBytes: w.scanned_bytes.value, scanLimit: w.max_scan_bytes.value,
                predicateRejections: w.predicate_rejections.value, counting: counting, window: window)
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }

    static func decodeClear(_ json: String?, identity: AtlasSavedIdentity, generation: UInt64) throws {
        let data = try AtlasSavedWire.admit(json, handle: identity.owner, command: "expression-clear", identity: identity)
        struct Clear: Decodable {
            let selection_namespace: String
            let expression_generation: AtlasSavedNumber
            let accepted_expression_generation: AtlasSavedSearchOptional<AtlasSavedNumber>
            let retained_hits: AtlasSavedNumber
        }
        do {
            let w = try JSONDecoder().decode(Clear.self, from: data)
            guard generation > 0, w.expression_generation.value == generation, w.selection_namespace == "saved-expression",
                  w.accepted_expression_generation.value == nil, w.retained_hits.value == 0 else {
                throw AtlasSavedError.invalidResponse
            }
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }

    private struct BoundedHits: Decodable {
        let values: [AtlasSavedHit]
        init(from decoder: Decoder) throws {
            var c = try decoder.unkeyedContainer()
            guard c.count.map({ $0 <= 128 }) ?? true else { throw AtlasSavedError.invalidResponse }
            var hits: [AtlasSavedHit] = []
            hits.reserveCapacity(c.count ?? 0)
            while !c.isAtEnd {
                guard hits.count < 128 else { throw AtlasSavedError.invalidResponse }
                hits.append(try c.decode(AtlasSavedHit.self))
            }
            values = hits
        }
    }
    private struct Wire: Decodable {
        let selection_namespace, expression, primary_needle, predicate_scope, index_policy, state: String
        let mode, normalization: String?
        let case_sensitive, index_used, complete, matches_seen_exact, truncated: Bool
        let count_all_matches: Bool?
        let matching_files, retained_matching_files: AtlasSavedNumber?
        let expression_generation, required_terms, excluded_terms, max_hits, max_scan_bytes: AtlasSavedNumber
        let retained_hits, matches_seen, metadata_examined, scope_files, metadata_excluded: AtlasSavedNumber
        let unavailable_files, unsupported_files, members_loaded, member_bytes_read, member_read_calls: AtlasSavedNumber
        let loaded_bytes, peak_source_bytes, predicate_scans, predicate_rejections, primary_scans, scanned_bytes: AtlasSavedNumber
        let retained_source_payload_bytes: AtlasSavedNumber
        let boundedHits: BoundedHits
        var hits: [AtlasSavedHit] { boundedHits.values }
        let next_offset: AtlasSavedSearchOptional<AtlasSavedNumber>
        enum CodingKeys: String, CodingKey {
            case selection_namespace, expression, primary_needle, predicate_scope, index_policy, state
            case mode, normalization
            case case_sensitive, index_used, complete, matches_seen_exact, truncated
            case count_all_matches, matching_files, retained_matching_files
            case expression_generation, required_terms, excluded_terms, max_hits, max_scan_bytes
            case retained_hits, matches_seen, metadata_examined, scope_files, metadata_excluded
            case unavailable_files, unsupported_files, members_loaded, member_bytes_read, member_read_calls
            case loaded_bytes, peak_source_bytes, predicate_scans, predicate_rejections, primary_scans, scanned_bytes
            case retained_source_payload_bytes, next_offset
            case boundedHits = "hits"
        }
    }
}

struct AtlasSavedSearchPage: Sendable {
    let identity: AtlasSavedIdentity
    let scope: AtlasSavedSearchScopeReceipt
    let generation: UInt64
    let needle: String
    let caseSensitive: Bool
    var matchingDescription: String { caseSensitive ? "Match case" : "Unicode lowercase matching" }
    let hits: [AtlasSavedHit]
    let start: UInt64
    let next: UInt64?
    let complete: Bool
    let truncated: Bool
    let retained: UInt64
    /// This is exhaustive only when counting.exact establishes closed coverage.
    let seen: UInt64
    let counting: AtlasSavedSearchCount
    let unavailable: UInt64
    let unsupported: UInt64
    let sourceBytes: UInt64
    let scannedBytes: UInt64
    let peakSourceBytes: UInt64
    let indexGeneration: UInt64?
    let indexPin: String?
    let indexEliminatedFiles: UInt64
    let fallbackFiles: UInt64
    let postingEntriesVisited: UInt64
    let window: AtlasSavedResultWindow

    func hasSameSummary(as other: Self) -> Bool {
        identity == other.identity && scope == other.scope && generation == other.generation && needle.utf8.elementsEqual(other.needle.utf8)
            && caseSensitive == other.caseSensitive && counting == other.counting && window == other.window
            && complete == other.complete && truncated == other.truncated
            && retained == other.retained && seen == other.seen && unavailable == other.unavailable
            && unsupported == other.unsupported && sourceBytes == other.sourceBytes && scannedBytes == other.scannedBytes
            && peakSourceBytes == other.peakSourceBytes && indexGeneration == other.indexGeneration && indexPin == other.indexPin
            && indexEliminatedFiles == other.indexEliminatedFiles && fallbackFiles == other.fallbackFiles
            && postingEntriesVisited == other.postingEntriesVisited
    }

    /// A nil expected needle is reserved for recovering the currently accepted
    /// query through info/results. New searches always bind the exact submitted
    /// UTF-8, and every subsequent page retains the returned accepted needle.
    static func decode(_ json: String?, identity: AtlasSavedIdentity, generation: UInt64,
        needle expectedNeedle: String?, start: UInt64, limit: UInt64, searching: Bool = false,
        countAllMatches expectedCount: Bool? = nil, caseSensitive expectedCase: Bool? = nil,
        resultOffset expectedOffset: UInt64? = nil, scope expectedScope: AtlasSavedStructureScope? = nil) throws -> Self {
        let data = try AtlasSavedWire.admit(json, handle: identity.owner,
            command: searching ? "search" : "results", identity: identity)
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            let membership = try AtlasSavedSearchScopeReceipt.decode(data, identity: identity, expected: expectedScope)
            let retained = w.retained_hits.value, seen = w.matches_seen.value
            let window = try AtlasSavedResultWindow.decode(data, expected: expectedOffset,
                seen: seen, retained: retained, legacyMore: w.truncated)
            let unavailable = w.unavailable_files.value, unsupported = w.unsupported_files.value
            let loaded = w.member_bytes_read.value, calls = w.member_read_calls.value
            let indexGeneration = w.index_generation.value?.value
            let indexPin = w.index_pin.value
            let caseSensitive: Bool
            switch (w.mode, w.case_sensitive) {
            case ("exact-decoded-literal", nil), ("exact-decoded-literal", .some(true)): caseSensitive = true
            case ("unicode-lowercase-literal", .some(false)): caseSensitive = false
            default: throw AtlasSavedError.invalidResponse
            }
            // Older exact receipts omit these additive fields. A matching-policy
            // receipt is accepted only as a complete, internally consistent set.
            if w.case_sensitive != nil || w.index_used != nil || w.index_policy != nil {
                let indexPolicy = indexGeneration == nil ? "direct-verified-member-scan"
                    : caseSensitive ? "exact-compatible-postings" : "ascii-case-variants-with-source-fallback"
                guard w.case_sensitive == caseSensitive, w.index_used == (indexGeneration != nil),
                      w.index_policy == indexPolicy else { throw AtlasSavedError.invalidResponse }
            }
            let counting = try AtlasSavedSearchCount.admit(all: w.count_all_matches, exact: w.matches_seen_exact,
                matchingFiles: w.matching_files?.value, retainedFiles: w.retained_matching_files?.value,
                expected: expectedCount, complete: w.search_complete, truncated: w.truncated, retained: retained,
                seen: seen, maximum: w.max_matches?.value ?? AtlasSavedSearchWire.maximumHits,
                availableFiles: membership.captured, scannedBytes: w.scanned_bytes.value,
                requiresReceipt: w.max_matches != nil || w.matches_seen_exact != nil,
                skipped: window.skipped, hasMore: window.hasMore)
            guard w.count_all_matches == nil || w.max_matches != nil,
                  counting.all || counting.maximum > 0 else { throw AtlasSavedError.invalidResponse }
            guard generation > 0, w.query_generation.value == generation,
                  AtlasSavedSearchWire.validNeedle(w.needle),
                  expectedNeedle == nil || w.needle.utf8.elementsEqual(expectedNeedle!.utf8),
                  !searching || (expectedNeedle != nil && start == 0 && limit == 64),
                  expectedCase == nil || expectedCase == caseSensitive, (1...128).contains(limit),
                  identity.captured <= identity.members,
                  unavailable == membership.members - membership.captured, unsupported <= membership.captured,
                  retained <= AtlasSavedSearchWire.maximumHits,
                  w.search_complete == (membership.complete && unavailable == 0 && unsupported == 0
                    && (counting.all || !window.hasMore)),
                  loaded <= AtlasSavedSearchWire.maximumSourceBytes, w.scanned_bytes.value <= loaded,
                  w.peak_source_bytes.value <= AtlasSavedWire.memberBytes, w.peak_source_bytes.value <= loaded,
                  (loaded == 0 && calls == 0) || (loaded > 0 && calls > 0),
                  calls <= membership.captured * 1_048_576,
                  w.retained_source_payload_bytes.value == 0,
                  w.index_eliminated_files.value <= membership.captured, w.fallback_files.value <= membership.captured,
                  (indexGeneration == nil) == (indexPin == nil),
                  indexGeneration == nil || (indexGeneration! > 0 && AtlasSavedWire.digest(indexPin!)),
                  indexGeneration != nil || (w.index_eliminated_files.value == 0 && w.fallback_files.value == 0 &&
                    w.posting_entries_visited.value == 0),
                  start <= retained, UInt64(w.hits.count) == min(limit, retained - start) else {
                throw AtlasSavedError.invalidResponse
            }
            // These are query-time index counters, not a partition of source
            // coverage and not a claim about the currently attached index.
            var previous: AtlasSavedHit?
            for (position, hit) in w.hits.enumerated() {
                guard hit.id == start + UInt64(position) + 1, hit.member < identity.members,
                      membership.scope.includes(hit.path.bytes) else {
                    throw AtlasSavedError.invalidResponse
                }
                if let previous {
                    guard hit.member >= previous.member else { throw AtlasSavedError.invalidResponse }
                    if hit.member == previous.member {
                        guard (caseSensitive ? hit.start > previous.start :
                            hit.start >= previous.start && hit.end >= previous.end), hit.path.bytes == previous.path.bytes,
                              hit.digest == previous.digest else { throw AtlasSavedError.invalidResponse }
                    }
                }
                previous = hit
            }
            let end = start + UInt64(w.hits.count)
            let next = w.next_offset.value?.value
            guard next == (end < retained ? end : nil) else { throw AtlasSavedError.invalidResponse }
            try counting.validatePage(w.hits, retained: retained)
            return Self(identity: identity, scope: membership, generation: generation, needle: w.needle, caseSensitive: caseSensitive, hits: w.hits,
                start: start, next: next, complete: w.search_complete, truncated: w.truncated,
                retained: retained, seen: seen, counting: counting, unavailable: unavailable, unsupported: unsupported,
                sourceBytes: loaded, scannedBytes: w.scanned_bytes.value, peakSourceBytes: w.peak_source_bytes.value,
                indexGeneration: indexGeneration, indexPin: indexPin,
                indexEliminatedFiles: w.index_eliminated_files.value, fallbackFiles: w.fallback_files.value,
                postingEntriesVisited: w.posting_entries_visited.value, window: window)
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }

    static func decodeClear(_ json: String?, identity: AtlasSavedIdentity) throws {
        let data = try AtlasSavedWire.admit(json, handle: identity.owner, command: "clear-results", identity: identity)
        struct Clear: Decodable {
            let accepted_query_generation: AtlasSavedSearchOptional<AtlasSavedNumber>
            let retained_hits: AtlasSavedNumber
        }
        do {
            let w = try JSONDecoder().decode(Clear.self, from: data)
            guard w.accepted_query_generation.value == nil, w.retained_hits.value == 0 else {
                throw AtlasSavedError.invalidResponse
            }
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }

    private struct Wire: Decodable {
        let query_generation: AtlasSavedNumber
        let needle, mode: String
        let case_sensitive, index_used: Bool?
        let index_policy: String?
        let index_generation: AtlasSavedSearchOptional<AtlasSavedNumber>
        let index_pin: AtlasSavedSearchOptional<String>
        let search_complete, truncated: Bool
        let count_all_matches, matches_seen_exact: Bool?
        let max_matches, matching_files, retained_matching_files: AtlasSavedNumber?
        let retained_hits, matches_seen, unavailable_files, unsupported_files: AtlasSavedNumber
        let member_bytes_read, member_read_calls, scanned_bytes, peak_source_bytes: AtlasSavedNumber
        let index_eliminated_files, fallback_files, posting_entries_visited, retained_source_payload_bytes: AtlasSavedNumber
        let hits: [AtlasSavedHit]
        let next_offset: AtlasSavedSearchOptional<AtlasSavedNumber>
    }
}
