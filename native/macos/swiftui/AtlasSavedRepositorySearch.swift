import Foundation

enum AtlasSavedSearchMode: String, CaseIterable, Identifiable, Sendable {
    case literal, expression
    var id: String { rawValue }
    var label: String { self == .literal ? "Literal" : "Expression" }
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
                  start <= retained, UInt64(w.hits.count) == min(limit, retained - start) else {
                throw AtlasSavedError.invalidResponse
            }
            // These are query-time index counters, not a partition of source
            // coverage and not a claim about the currently attached index.
            _ = w.posting_entries_visited.value
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
                indexGeneration: indexGeneration, indexPin: indexPin)
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
