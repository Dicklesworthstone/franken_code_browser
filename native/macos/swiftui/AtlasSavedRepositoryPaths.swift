import Foundation

enum AtlasSavedPathMode: String, CaseIterable, Sendable, Identifiable {
    case exact, prefix, fuzzy
    var id: String { rawValue }
    var label: String { switch self { case .exact: return "Exact"; case .prefix: return "Prefix"; case .fuzzy: return "Fuzzy" } }
    var code: UInt64 { switch self { case .exact: return 0; case .prefix: return 1; case .fuzzy: return 2 } }
    var matchKinds: [String] {
        let exact = ["exact-filename", "exact-path", "exact-component"]
        if self == .exact { return exact }
        let prefixes = exact + ["filename-prefix", "component-prefix", "path-prefix"]
        return self == .prefix ? prefixes : prefixes + ["filename-subsequence", "path-subsequence"]
    }
}

enum AtlasSavedPathWire {
    static let maximumQueryBytes = 256
    static let maximumResults: UInt64 = 256
    static let defaultWork: UInt64 = 64 * 1024 * 1024
    static let maximumWork: UInt64 = 1 << 32
    static func validQuery(_ bytes: [UInt8]) -> Bool {
        !bytes.isEmpty && bytes.count <= maximumQueryBytes && !bytes.contains(0)
    }
    static func clear(_ json: String?, identity: AtlasSavedIdentity, generation: UInt64) throws {
        let data = try AtlasSavedWire.admit(json, handle: identity.owner, command: "paths-clear", identity: identity)
        guard generation > 0,
              let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              object["selection_namespace"] as? String == "saved-paths",
              (object["path_generation"] as? String).flatMap(AtlasReaderWire.integer) == generation,
              object["accepted_path_generation"] is NSNull,
              object["retained_hits"] as? String == "0" else { throw AtlasSavedError.invalidResponse }
    }
}

struct AtlasSavedPathHit: Decodable, Sendable, Identifiable {
    let id: UInt64
    let member: AtlasSavedMember
    let matchKind: String
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(AtlasSavedNumber.self, forKey: .hit_id).value
        matchKind = try c.decode(String.self, forKey: .match_kind)
        member = try AtlasSavedMember(from: decoder)
        guard (1...AtlasSavedPathWire.maximumResults).contains(id),
              AtlasSavedPathMode.fuzzy.matchKinds.contains(matchKind) else { throw AtlasSavedError.invalidResponse }
    }
    func matches(_ other: Self) -> Bool { id == other.id && member.matches(other.member) && matchKind == other.matchKind }
    enum CodingKeys: CodingKey { case hit_id, match_kind }
}

/// Metadata search is complete for the closed archive directory even when the
/// historical project discovery was incomplete. That separate fact remains in
/// identity.discoveryComplete; finding names never reads saved member payloads.
struct AtlasSavedPaths: Sendable {
    let identity: AtlasSavedIdentity
    let generation: UInt64
    let query: [UInt8]
    let mode: AtlasSavedPathMode
    let caseSensitive: Bool
    let maxResults, maxWorkUnits: UInt64
    let complete, truncated, workLimited: Bool
    let seen, retained, filesExamined, candidatesExamined, workUnits, metadataBytes: UInt64
    let start, limit: UInt64
    let hits: [AtlasSavedPathHit]
    let next: UInt64?
    var canSearchMore: Bool { workLimited && maxWorkUnits < AtlasSavedPathWire.maximumWork }
    var expandedWork: UInt64 { min(AtlasSavedPathWire.maximumWork, max(AtlasSavedPathWire.defaultWork, maxWorkUnits * 4)) }
    func matchesInventory(_ other: Self) -> Bool {
        identity == other.identity && generation == other.generation && query == other.query && mode == other.mode
            && caseSensitive == other.caseSensitive && maxResults == other.maxResults && maxWorkUnits == other.maxWorkUnits
            && complete == other.complete && truncated == other.truncated && workLimited == other.workLimited
            && seen == other.seen && retained == other.retained && filesExamined == other.filesExamined
            && candidatesExamined == other.candidatesExamined && workUnits == other.workUnits && metadataBytes == other.metadataBytes
    }
    static func decode(_ json: String?, identity: AtlasSavedIdentity, generation: UInt64,
        query: [UInt8]? = nil, mode: AtlasSavedPathMode? = nil, caseSensitive: Bool? = nil,
        maxResults: UInt64? = nil, maxWorkUnits: UInt64? = nil, start: UInt64, limit: UInt64) throws -> Self {
        let data = try AtlasSavedWire.admit(json, handle: identity.owner, command: "paths", identity: identity)
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            let acceptedQuery = try AtlasReaderWire.pathBytes(w.query_hex)
            guard generation > 0, w.path_generation.value == generation, w.selection_namespace == "saved-paths",
                  AtlasSavedPathWire.validQuery(acceptedQuery), query == nil || query == acceptedQuery,
                  let acceptedMode = AtlasSavedPathMode(rawValue: w.mode), mode == nil || mode == acceptedMode,
                  ["sensitive", "unicode-lowercase"].contains(w.casePolicy),
                  caseSensitive == nil || caseSensitive == (w.casePolicy == "sensitive"),
                  w.key_version.value == 1, w.metadata_scope == "archive-directory", w.identity_scope == "retained-saved-query",
                  w.ordering == "ranked-archive-paths", w.unicode_normalization == "none",
                  !w.source_payload_read, !w.live_roots_accessed, !w.persistent_index_used,
                  w.retained_source_payload_bytes.value == 0, w.member_bytes_read.value == 0, w.member_read_calls.value == 0,
                  w.max_results.value <= AtlasSavedPathWire.maximumResults, w.max_work_units.value <= AtlasSavedPathWire.maximumWork,
                  maxResults == nil || maxResults == w.max_results.value, maxWorkUnits == nil || maxWorkUnits == w.max_work_units.value,
                  w.retained_hits.value == min(w.max_results.value, w.matches_seen.value),
                  w.matches_seen.value <= w.files_examined.value, w.files_examined.value <= identity.members,
                  w.candidates_examined.value >= w.files_examined.value, w.candidates_examined.value <= 4_000_000,
                  acceptedMode != .fuzzy || w.candidates_examined.value == w.files_examined.value,
                  w.work_units.value >= w.candidates_examined.value, w.work_units.value <= w.max_work_units.value,
                  w.metadata_path_bytes.value >= identity.members, w.metadata_path_bytes.value <= identity.members * 2048,
                  w.matches_seen_exact == w.search_complete, w.top_k_complete == w.search_complete,
                  w.work_limited == !w.search_complete, w.truncated == (w.matches_seen.value > w.retained_hits.value),
                  w.state == (w.search_complete ? "complete" : "work-limit"),
                  (1...128).contains(limit), w.offset.value == start, w.limit.value == limit, start <= w.retained_hits.value,
                  UInt64(w.hits.count) <= min(limit, w.retained_hits.value - start),
                  !w.hits.isEmpty || start == w.retained_hits.value else { throw AtlasSavedError.invalidResponse }
            var members: Set<UInt64> = []
            for (position, hit) in w.hits.enumerated() {
                guard hit.id == start + UInt64(position) + 1, hit.member.id < identity.members,
                      members.insert(hit.member.id).inserted, acceptedMode.matchKinds.contains(hit.matchKind) else {
                    throw AtlasSavedError.invalidResponse
                }
            }
            let end = start + UInt64(w.hits.count), next = end < w.retained_hits.value ? end : nil
            guard w.next_offset?.value == next else { throw AtlasSavedError.invalidResponse }
            return Self(identity: identity, generation: generation, query: acceptedQuery, mode: acceptedMode,
                caseSensitive: w.casePolicy == "sensitive", maxResults: w.max_results.value, maxWorkUnits: w.max_work_units.value,
                complete: w.search_complete, truncated: w.truncated, workLimited: w.work_limited,
                seen: w.matches_seen.value, retained: w.retained_hits.value, filesExamined: w.files_examined.value,
                candidatesExamined: w.candidates_examined.value, workUnits: w.work_units.value, metadataBytes: w.metadata_path_bytes.value,
                start: start, limit: limit, hits: w.hits, next: next)
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }
    private struct Wire: Decodable {
        let selection_namespace, query_hex, mode, casePolicy, metadata_scope, identity_scope, ordering, unicode_normalization, state: String
        let path_generation, key_version, retained_source_payload_bytes, member_bytes_read, member_read_calls: AtlasSavedNumber
        let max_results, max_work_units, matches_seen, retained_hits, files_examined, candidates_examined, work_units, metadata_path_bytes: AtlasSavedNumber
        let offset, limit: AtlasSavedNumber
        let next_offset: AtlasSavedNumber?
        let source_payload_read, live_roots_accessed, persistent_index_used, search_complete, matches_seen_exact, top_k_complete, truncated, work_limited: Bool
        let hits: [AtlasSavedPathHit]
        enum CodingKeys: String, CodingKey {
            case casePolicy = "case"
            case selection_namespace, query_hex, mode, metadata_scope, identity_scope, ordering, unicode_normalization, state,
                 path_generation, key_version, retained_source_payload_bytes, member_bytes_read, member_read_calls,
                 max_results, max_work_units, matches_seen, retained_hits, files_examined, candidates_examined, work_units, metadata_path_bytes,
                 offset, limit, next_offset, source_payload_read, live_roots_accessed, persistent_index_used, search_complete,
                 matches_seen_exact, top_k_complete, truncated, work_limited, hits
        }
    }
}
