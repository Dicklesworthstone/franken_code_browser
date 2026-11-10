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
    func validate(mode: AtlasSavedSearchMode, needle: String) throws {
        guard let expected = mode.structure, structure.mode == expected, structure.evidence == expected.evidence,
              structure.evidenceEnd <= expected.structureFileByteLimit else { throw AtlasSavedError.invalidResponse }
        switch mode {
        case .symbols:
            guard structure.headingSlug == nil,
                  ["rust", "python", "javascript", "typescript", "go", "cpp"].contains(structure.language) else {
                throw AtlasSavedError.invalidResponse
            }
        case .headings:
            guard structure.headingSlug != nil, structure.kind == "heading", structure.language == "markdown",
                  structure.evidenceStart == hit.start, structure.evidenceEnd == hit.end else {
                throw AtlasSavedError.invalidResponse
            }
        case .occurrences:
            guard structure.headingSlug == nil, structure.kind == "identifier-occurrence", structure.language == "text",
                  structure.name.utf8.elementsEqual(needle.utf8),
                  structure.evidenceStart == hit.start, structure.evidenceEnd == hit.end,
                  hit.end - hit.start == UInt64(needle.utf8.count) || hit.end - hit.start == UInt64(needle.utf16.count * 2) else {
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
    let complete, truncated: Bool
    let retained, seen, visited, loaded, searched, unavailable, unsupported: UInt64
    let sourceLimited, extractionLimited, metadataOmitted, sourceBytes, sourceLimit, metadataBytes: UInt64

    static func decode(_ json: String?, identity: AtlasSavedIdentity, generation: UInt64,
        mode expectedMode: AtlasSavedSearchMode?, nameMode expectedName: AtlasSearchNameMode?,
        needle expectedNeedle: String?, start: UInt64, limit: UInt64) throws -> Self {
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
                  !w.semantic_complete, w.name_case == "sensitive", w.scope == "whole-saved-archive",
                  w.completeness_basis == "admitted-extraction-processing", !w.index_used,
                  w.index_policy == "direct-verified-member-extraction",
                  AtlasSavedSearchWire.validNeedle(w.needle),
                  expectedNeedle == nil || w.needle.utf8.elementsEqual(expectedNeedle!.utf8) else {
                throw AtlasSavedError.invalidResponse
            }
            if mode == .occurrences {
                guard w.includes_comments_and_literals == true, w.compiler_resolved == false,
                      w.token_policy == "ascii-alphanumeric-underscore-dollar-plus-nonascii-nonwhitespace-noncontrol-v1",
                      w.name_syntax == "single-token-no-leading-ascii-digit", w.unicode_normalization == "none" else {
                    throw AtlasSavedError.invalidResponse
                }
            }
            let retained = w.retained_hits.value, omitted = w.metadata_omitted.value
            let visited = w.members_visited.value, loaded = w.members_loaded.value
            let searched = w.files_searched.value, unavailable = w.unavailable_files.value
            let unsupported = w.unsupported_files.value, limited = w.source_limited_files.value
            let extractionLimited = w.extraction_limited_files.value
            let bytes = w.loaded_bytes.value, calls = w.member_read_calls.value
            guard ["complete", "truncated", "work-limit"].contains(w.state),
                  w.max_hits.value <= AtlasSavedSearchWire.maximumHits, retained <= w.max_hits.value,
                  w.max_source_bytes.value <= AtlasSavedSearchWire.maximumSourceBytes, bytes <= w.max_source_bytes.value,
                  w.truncated == (w.state == "truncated"), omitted <= identity.members * 4096,
                  w.matches_seen.value == retained + omitted + (w.truncated ? 1 : 0),
                  visited <= identity.members, loaded <= visited, loaded <= identity.captured,
                  searched <= loaded, unavailable <= visited, unavailable <= identity.members - identity.captured,
                  unsupported <= visited - unavailable, limited <= visited - unavailable - unsupported,
                  searched <= visited - unavailable - unsupported - limited,
                  extractionLimited <= loaded,
                  w.state != "complete" || visited == identity.members,
                  w.complete == (w.state == "complete" && identity.discoveryComplete && unavailable == 0 &&
                    unsupported == 0 && limited == 0 && extractionLimited == 0 && omitted == 0),
                  w.matches_seen_exact == w.complete,
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
                guard hit.id == start + UInt64(position) + 1, hit.member < identity.members else {
                    throw AtlasSavedError.invalidResponse
                }
                try item.validate(mode: mode, needle: w.needle)
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
                retained: retained, seen: w.matches_seen.value, visited: visited, loaded: loaded, searched: searched,
                unavailable: unavailable, unsupported: unsupported, sourceLimited: limited, extractionLimited: extractionLimited,
                metadataOmitted: omitted, sourceBytes: bytes, sourceLimit: w.max_source_bytes.value, metadataBytes: w.metadata_bytes.value)
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
        let scope, completeness_basis, state, index_policy: String
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
    let generation: UInt64
    let expression: String
    let primaryNeedle: String
    let hits: [AtlasSavedHit]
    let start: UInt64
    let next: UInt64?
    let state: String
    let complete: Bool
    let truncated: Bool
    let retained, seen, required, excluded, scopeFiles, filteredFiles, unavailable, unsupported: UInt64
    let sourceBytes, scannedBytes, scanLimit, predicateRejections: UInt64

    static func decode(_ json: String?, identity: AtlasSavedIdentity, generation: UInt64,
        expression expected: String?, start: UInt64, limit: UInt64) throws -> Self {
        let data = try AtlasSavedWire.admit(json, handle: identity.owner, command: "expression", identity: identity)
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            let retained = w.retained_hits.value, seen = w.matches_seen.value
            let required = w.required_terms.value, excluded = w.excluded_terms.value
            let scope = w.scope_files.value, unavailable = w.unavailable_files.value
            let loaded = w.members_loaded.value, unsupported = w.unsupported_files.value
            let sourceBytes = w.loaded_bytes.value, calls = w.member_read_calls.value
            guard generation > 0, w.expression_generation.value == generation,
                  w.selection_namespace == "saved-expression",
                  AtlasSavedSearchWire.validNeedle(w.expression), AtlasSavedSearchWire.validNeedle(w.primary_needle),
                  expected == nil || w.expression.utf8.elementsEqual(expected!.utf8),
                  w.predicate_scope == "whole-captured-member", w.case_sensitive,
                  !w.index_used, w.index_policy == "direct-verified-member-scan",
                  ["complete", "truncated", "work-limit"].contains(w.state),
                  required <= 63, excluded <= 63 - required,
                  w.max_hits.value <= AtlasSavedSearchWire.maximumHits, retained <= w.max_hits.value,
                  w.max_scan_bytes.value <= AtlasSavedSearchWire.maximumSourceBytes,
                  w.scanned_bytes.value <= w.max_scan_bytes.value,
                  w.truncated == (w.state == "truncated"), seen == retained + (w.truncated ? 1 : 0),
                  w.metadata_examined.value == identity.members,
                  scope <= identity.members, w.metadata_excluded.value == identity.members - scope,
                  unavailable <= scope, unavailable <= identity.members - identity.captured,
                  loaded <= scope - unavailable, unsupported <= loaded,
                  w.state != "complete" || loaded == scope - unavailable,
                  w.complete == (w.state == "complete" && identity.discoveryComplete && unavailable == 0 && unsupported == 0),
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
                guard hit.id == start + UInt64(position) + 1, hit.member < identity.members else {
                    throw AtlasSavedError.invalidResponse
                }
                if let previous {
                    guard hit.member >= previous.member else { throw AtlasSavedError.invalidResponse }
                    if hit.member == previous.member {
                        guard hit.start > previous.start, hit.path.bytes == previous.path.bytes,
                              hit.digest == previous.digest else { throw AtlasSavedError.invalidResponse }
                    }
                }
                previous = hit
            }
            let end = start + UInt64(w.hits.count), next = w.next_offset.value?.value
            guard next == (end < retained ? end : nil) else { throw AtlasSavedError.invalidResponse }
            return Self(identity: identity, generation: generation, expression: w.expression, primaryNeedle: w.primary_needle,
                hits: w.hits, start: start, next: next, state: w.state, complete: w.complete, truncated: w.truncated,
                retained: retained, seen: seen, required: required, excluded: excluded, scopeFiles: scope,
                filteredFiles: w.metadata_excluded.value, unavailable: unavailable, unsupported: unsupported,
                sourceBytes: sourceBytes, scannedBytes: w.scanned_bytes.value, scanLimit: w.max_scan_bytes.value,
                predicateRejections: w.predicate_rejections.value)
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
        let case_sensitive, index_used, complete, matches_seen_exact, truncated: Bool
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
            case case_sensitive, index_used, complete, matches_seen_exact, truncated
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
    let generation: UInt64
    let needle: String
    let hits: [AtlasSavedHit]
    let start: UInt64
    let next: UInt64?
    let complete: Bool
    let truncated: Bool
    let retained: UInt64
    /// The producer stops after proving one omitted occurrence. When truncated,
    /// this is a lower bound for the archive, not an exhaustive match count.
    let seen: UInt64
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

    /// A nil expected needle is reserved for recovering the currently accepted
    /// query through info/results. New searches always bind the exact submitted
    /// UTF-8, and every subsequent page retains the returned accepted needle.
    static func decode(_ json: String?, identity: AtlasSavedIdentity, generation: UInt64,
        needle expectedNeedle: String?, start: UInt64, limit: UInt64, searching: Bool = false) throws -> Self {
        let data = try AtlasSavedWire.admit(json, handle: identity.owner,
            command: searching ? "search" : "results", identity: identity)
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            let retained = w.retained_hits.value, seen = w.matches_seen.value
            let unavailable = w.unavailable_files.value, unsupported = w.unsupported_files.value
            let loaded = w.member_bytes_read.value, calls = w.member_read_calls.value
            let indexGeneration = w.index_generation.value?.value
            let indexPin = w.index_pin.value
            guard generation > 0, w.query_generation.value == generation,
                  AtlasSavedSearchWire.validNeedle(w.needle),
                  expectedNeedle == nil || w.needle.utf8.elementsEqual(expectedNeedle!.utf8),
                  !searching || (expectedNeedle != nil && start == 0 && limit == 64),
                  w.mode == "exact-decoded-literal", (1...128).contains(limit),
                  identity.captured <= identity.members,
                  unavailable == identity.members - identity.captured, unsupported <= identity.captured,
                  retained <= AtlasSavedSearchWire.maximumHits,
                  seen == retained + (w.truncated ? 1 : 0),
                  w.search_complete == (identity.discoveryComplete && unavailable == 0 && unsupported == 0 && !w.truncated),
                  loaded <= AtlasSavedSearchWire.maximumSourceBytes, w.scanned_bytes.value <= loaded,
                  w.peak_source_bytes.value <= AtlasSavedWire.memberBytes, w.peak_source_bytes.value <= loaded,
                  (loaded == 0 && calls == 0) || (loaded > 0 && calls > 0),
                  calls <= identity.captured * 1_048_576,
                  w.retained_source_payload_bytes.value == 0,
                  w.index_eliminated_files.value <= identity.members, w.fallback_files.value <= identity.members,
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
                guard hit.id == start + UInt64(position) + 1, hit.member < identity.members else {
                    throw AtlasSavedError.invalidResponse
                }
                if let previous {
                    guard hit.member >= previous.member else { throw AtlasSavedError.invalidResponse }
                    if hit.member == previous.member {
                        guard hit.start > previous.start, hit.path.bytes == previous.path.bytes,
                              hit.digest == previous.digest else { throw AtlasSavedError.invalidResponse }
                    }
                }
                previous = hit
            }
            let end = start + UInt64(w.hits.count)
            let next = w.next_offset.value?.value
            guard next == (end < retained ? end : nil) else { throw AtlasSavedError.invalidResponse }
            return Self(identity: identity, generation: generation, needle: w.needle, hits: w.hits,
                start: start, next: next, complete: w.search_complete, truncated: w.truncated,
                retained: retained, seen: seen, unavailable: unavailable, unsupported: unsupported,
                sourceBytes: loaded, scannedBytes: w.scanned_bytes.value, peakSourceBytes: w.peak_source_bytes.value,
                indexGeneration: indexGeneration, indexPin: indexPin,
                indexEliminatedFiles: w.index_eliminated_files.value, fallbackFiles: w.fallback_files.value,
                postingEntriesVisited: w.posting_entries_visited.value)
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
        let index_generation: AtlasSavedSearchOptional<AtlasSavedNumber>
        let index_pin: AtlasSavedSearchOptional<String>
        let search_complete, truncated: Bool
        let retained_hits, matches_seen, unavailable_files, unsupported_files: AtlasSavedNumber
        let member_bytes_read, member_read_calls, scanned_bytes, peak_source_bytes: AtlasSavedNumber
        let index_eliminated_files, fallback_files, posting_entries_visited, retained_source_payload_bytes: AtlasSavedNumber
        let hits: [AtlasSavedHit]
        let next_offset: AtlasSavedSearchOptional<AtlasSavedNumber>
    }
}
