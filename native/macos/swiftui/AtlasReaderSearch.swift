import Foundation

/// Optional transport capability, separate from source-page-only hosts. Native
/// builds supply the existing Rust find/hit ABI, not another matcher or decoder.
struct AtlasReaderSearchTransport: Sendable {
    let find: @Sendable (UInt64, UInt64, String, UInt64, UInt64) -> String?
    let hit: @Sendable (UInt64, UInt64, UInt64, UInt64) -> String?
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
    let identity: AtlasReaderIdentity
    let generation: UInt64
    let needle: String
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
    }

    static func validateNeedle(_ needle: String) throws {
        guard !needle.isEmpty, needle.utf8.count <= 1024, !needle.utf8.contains(0) else {
            throw AtlasReaderError.invalidInput
        }
    }
    func target(at index: Int) -> AtlasReaderHitTarget? {
        guard hits.indices.contains(index), let originalHex else { return nil }
        let hit = hits[index]
        return AtlasReaderHitTarget(generation: generation, index: hit.index, start: hit.start,
            end: hit.end, needle: needle, originalHex: originalHex)
    }
    var summary: String {
        if complete { return "\(hits.count) exact matches in the retained file." }
        let reason: String
        switch state {
        case "match-limit": reason = "match limit reached"
        case "byte-limit": reason = "scan byte limit reached"
        case "read-call-limit": reason = "read-call limit reached"
        case "short-read": reason = "source observation incomplete"
        case "unsupported-text": reason = "unsupported text encountered"
        default: reason = "coverage incomplete"
        }
        return "Partial file search: \(hits.count) matches retained; \(reason). More matches may exist."
    }

    static func decode(_ json: String?, info: AtlasReaderInfo, generation: UInt64, needle: String) throws -> Self {
        try validateNeedle(needle)
        let data = try AtlasReaderWire.admit(json, handle: info.identity.owner)
        do {
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            let identity = try wire.header.identity(handle: info.identity.owner)
            guard identity.matches(info.identity), wire.header.command == "find", generation > 0,
                  AtlasReaderWire.integer(wire.query_generation) == generation,
                  wire.mode == "exact-decoded-literal", wire.needle.utf8.elementsEqual(needle.utf8),
                  let seen = AtlasReaderWire.integer(wire.matches_seen),
                  let retained = AtlasReaderWire.integer(wire.retained_hits), retained <= maxHits,
                  retained == UInt64(wire.hits.count), seen >= retained,
                  let scanned = AtlasReaderWire.integer(wire.scanned_bytes), scanned <= identity.capturedBytes,
                  ["complete-observed-input", "match-limit", "byte-limit", "read-call-limit", "short-read", "unsupported-text"].contains(wire.state),
                  wire.search_complete == (wire.state == "complete-observed-input") else {
                throw AtlasReaderError.invalidResponse
            }
            if wire.search_complete {
                guard seen == retained, wire.unsupported_at == nil else { throw AtlasReaderError.invalidResponse }
            }
            if let unsupported = wire.unsupported_at {
                guard wire.state == "unsupported-text", let offset = AtlasReaderWire.integer(unsupported),
                      offset <= identity.capturedBytes else { throw AtlasReaderError.invalidResponse }
            }
            let witnessBytes: UInt64
            if let hex = wire.literal_original_hex {
                // Exact UTF-16 can use twice the UTF-8 query byte count. This is
                // a marshaling bound, not a second source-encoding algorithm.
                witnessBytes = UInt64(hex.utf8.count / 2)
                guard retained > 0, witnessBytes > 0, witnessBytes <= 2048,
                      AtlasReaderWire.validHex(hex, bytes: witnessBytes) else { throw AtlasReaderError.invalidResponse }
            } else {
                guard retained == 0 else { throw AtlasReaderError.invalidResponse }
                witnessBytes = 0
            }
            var occurrences: Set<UInt64> = []
            let hits = try wire.hits.enumerated().map { index, hit in
                let range = try hit.original_range.values()
                guard AtlasReaderWire.integer(hit.hit_index) == UInt64(index),
                      let occurrence = AtlasReaderWire.integer(hit.occurrence_id),
                      occurrences.insert(occurrence).inserted,
                      range.start < range.end, range.end <= identity.capturedBytes,
                      range.end - range.start == witnessBytes else { throw AtlasReaderError.invalidResponse }
                return Hit(index: UInt64(index), occurrence: occurrence, start: range.start, end: range.end)
            }
            return Self(identity: identity, generation: generation, needle: needle,
                complete: wire.search_complete, state: wire.state, matchesSeen: seen,
                hits: hits, originalHex: wire.literal_original_hex)
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }

    private struct Wire: Decodable {
        let header: AtlasReaderWire.Header
        let query_generation, mode, needle, state, scanned_bytes, matches_seen, retained_hits: String
        let search_complete: Bool
        let literal_original_hex, unsupported_at: String?
        let hits: [HitWire]
        enum CodingKeys: CodingKey {
            case query_generation, mode, needle, state, scanned_bytes, matches_seen, retained_hits,
                 search_complete, literal_original_hex, unsupported_at, hits
        }
        struct HitWire: Decodable {
            let hit_index, occurrence_id: String
            let original_range: AtlasReaderWire.Range
        }
        init(from decoder: Decoder) throws {
            header = try .init(from: decoder)
            let c = try decoder.container(keyedBy: CodingKeys.self)
            query_generation = try c.decode(String.self, forKey: .query_generation)
            mode = try c.decode(String.self, forKey: .mode)
            needle = try c.decode(String.self, forKey: .needle)
            state = try c.decode(String.self, forKey: .state)
            scanned_bytes = try c.decode(String.self, forKey: .scanned_bytes)
            matches_seen = try c.decode(String.self, forKey: .matches_seen)
            retained_hits = try c.decode(String.self, forKey: .retained_hits)
            search_complete = try c.decode(Bool.self, forKey: .search_complete)
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
}
