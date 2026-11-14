import Foundation

/// A trail is an ordered set of explicit byte anchors and notes. Its names
/// never grant filesystem access, including entries for a different archive.
enum AtlasSavedTrailReadiness: String, Decodable, Sendable {
    case ready
    case otherArchive = "other-archive", missingMember = "missing-member"
    case unavailableMember = "unavailable-member", changedSource = "changed-source", invalidRange = "invalid-range"
    var label: String {
        switch self {
        case .ready: return "Ready in this archive"
        case .otherArchive: return "Requires a different saved archive"
        case .missingMember: return "Member absent from this archive"
        case .unavailableMember: return "Source was not captured"
        case .changedSource: return "Captured source does not match this anchor"
        case .invalidRange: return "Range unavailable in this capture"
        }
    }
}

struct AtlasSavedTrailRange: Decodable, Sendable, Equatable {
    let start, end: UInt64
    var contextStart: UInt64 { start > 132 ? start - 132 : 0 }
    var label: String { start == end ? "Bookmark at byte \(start)" : "Original bytes \(start)..<\(end)" }
    init(start: UInt64, end: UInt64) throws {
        guard start <= end else { throw AtlasSavedError.invalidInput }
        self.start = start; self.end = end
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        start = try c.decode(AtlasSavedNumber.self, forKey: .start).value
        end = try c.decode(AtlasSavedNumber.self, forKey: .end).value
        guard start <= end else { throw AtlasSavedError.invalidResponse }
    }
    enum CodingKeys: CodingKey { case start, end }
}

struct AtlasSavedTrailEntry: Decodable, Sendable, Identifiable {
    let id: UInt64
    let archiveDigest, sourceDigest: String
    let sourceBytes: UInt64
    let path: AtlasSavedPath
    let range: AtlasSavedTrailRange
    let rationale: String
    let readiness: AtlasSavedTrailReadiness
    let member, file, revision: UInt64?
    var canOpen: Bool { readiness == .ready }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(AtlasSavedNumber.self, forKey: .item).value
        archiveDigest = try c.decode(String.self, forKey: .archive_digest)
        sourceDigest = try c.decode(String.self, forKey: .source_digest)
        sourceBytes = try c.decode(AtlasSavedNumber.self, forKey: .source_bytes).value
        path = try c.decode(AtlasSavedPath.self, forKey: .path)
        range = try c.decode(AtlasSavedTrailRange.self, forKey: .original_range)
        rationale = try c.decode(String.self, forKey: .rationale)
        readiness = try c.decode(AtlasSavedTrailReadiness.self, forKey: .readiness)
        func optional(_ key: CodingKeys) throws -> UInt64? {
            guard c.contains(key) else { throw AtlasSavedError.invalidResponse }
            return try c.decodeIfPresent(AtlasSavedNumber.self, forKey: key)?.value
        }
        member = try optional(.member); file = try optional(.file_id); revision = try optional(.source_revision)
        guard id < 1024, AtlasSavedWire.digest(archiveDigest), AtlasSavedWire.digest(sourceDigest),
              range.end <= sourceBytes, rationale.utf8.count <= 4096,
              path.bytes.count <= 16_384, path.bytes.first != 47,
              !path.bytes.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                  $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
              }), try c.decode(String.self, forKey: .evidence) == "explicit-user-selection" else {
            throw AtlasSavedError.invalidResponse
        }
        if canOpen {
            guard let member, member < 65_536, file == member + 1, revision == file,
                  sourceBytes <= AtlasSavedWire.memberBytes, path.isMember else { throw AtlasSavedError.invalidResponse }
        } else if member != nil || file != nil || revision != nil { throw AtlasSavedError.invalidResponse }
    }
    func validate(in identity: AtlasSavedIdentity) throws {
        guard (readiness == .otherArchive) == (archiveDigest != identity.digest),
              member == nil || member! < identity.members else { throw AtlasSavedError.invalidResponse }
    }
    func matches(_ other: Self) -> Bool {
        id == other.id && archiveDigest == other.archiveDigest && sourceDigest == other.sourceDigest &&
            sourceBytes == other.sourceBytes && path.bytes == other.path.bytes &&
            path.display.utf8.elementsEqual(other.path.display.utf8) && range == other.range &&
            rationale.utf8.elementsEqual(other.rationale.utf8) && readiness == other.readiness &&
            member == other.member && file == other.file && revision == other.revision
    }
    func matches(_ saved: AtlasSavedMember) -> Bool {
        canOpen && saved.captured && member == saved.id && file == saved.file && revision == saved.file &&
            path.bytes == saved.path.bytes && sourceDigest == saved.digest && sourceBytes == saved.observedBytes
    }
    enum CodingKeys: CodingKey {
        case item, archive_digest, source_digest, source_bytes, path, original_range, rationale, evidence, readiness,
             member, file_id, source_revision
    }
}

struct AtlasSavedTrailPage: Sendable {
    static let pageSize: UInt64 = 32
    let identity: AtlasSavedIdentity
    let generation, lastAttempt: UInt64
    let digest: String
    let path: AtlasSavedPath
    let title: String
    let parentDigest: String?
    let total, referenceBytes, retainedBytes, ready, unavailable, readCalls: UInt64
    let start, limit: UInt64
    let items: [AtlasSavedTrailEntry]
    let next: UInt64?

    func matchesAttachment(_ other: Self) -> Bool {
        identity == other.identity && generation == other.generation && digest == other.digest &&
            path.bytes == other.path.bytes && path.display.utf8.elementsEqual(other.path.display.utf8) &&
            title.utf8.elementsEqual(other.title.utf8) && parentDigest == other.parentDigest && total == other.total &&
            referenceBytes == other.referenceBytes && retainedBytes == other.retainedBytes &&
            ready == other.ready && unavailable == other.unavailable && readCalls == other.readCalls
    }

    static func decode(_ json: String?, identity: AtlasSavedIdentity, generation: UInt64,
        start: UInt64, limit: UInt64 = pageSize, attachingPath: [UInt8]? = nil) throws -> Self {
        let data = try AtlasSavedWire.admit(json, handle: identity.owner,
            command: attachingPath == nil ? "trail-page" : "trail-attach", identity: identity)
        struct Wire: Decodable {
            let trail_generation, accepted_trail_generation, last_trail_attempt: AtlasSavedNumber
            let trail_digest: String
            let trail_path: AtlasSavedPath
            let title: String
            let parent_digest: AtlasSavedSearchOptional<String>
            let total_items, reference_bytes, retained_trail_bytes, ready_items, unavailable_items: AtlasSavedNumber
            let trail_read_calls, max_trail_bytes, max_trail_items, max_page_items: AtlasSavedNumber
            let retained_source_payload_bytes, member_bytes_read, member_read_calls: AtlasSavedNumber
            let readiness_scope, source_observation: String
            let payload_verified, truncated: Bool
            let offset, limit: AtlasSavedNumber
            let items: [AtlasSavedTrailEntry]
            let next_offset: AtlasSavedSearchOptional<AtlasSavedNumber>
        }
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            let total = w.total_items.value, ready = w.ready_items.value
            guard generation > 0, w.trail_generation.value == generation,
                  w.accepted_trail_generation.value == generation, w.last_trail_attempt.value >= generation,
                  attachingPath == nil || w.last_trail_attempt.value == generation,
                  AtlasSavedWire.digest(w.trail_digest), w.title.utf8.count <= 256,
                  w.parent_digest.value.map(AtlasSavedWire.digest) != false,
                  w.trail_path.bytes.first == 47, w.trail_path.bytes.count <= 16_384,
                  attachingPath == nil || w.trail_path.bytes == attachingPath,
                  total <= 1024, ready <= total, w.unavailable_items.value == total - ready,
                  w.retained_trail_bytes.value > 0, w.retained_trail_bytes.value <= 4 * 1024 * 1024,
                  (1...131_072).contains(w.trail_read_calls.value),
                  w.max_trail_bytes.value == 4 * 1024 * 1024, w.max_trail_items.value == 1024,
                  w.max_page_items.value == pageSize, w.retained_source_payload_bytes.value == 0,
                  w.member_bytes_read.value == 0, w.member_read_calls.value == 0,
                  w.readiness_scope == "selected-saved-archive-metadata", !w.payload_verified,
                  w.source_observation == "saved-manifest-metadata", !w.truncated,
                  (1...pageSize).contains(limit), start <= total, w.offset.value == start, w.limit.value == limit,
                  UInt64(w.items.count) == min(limit, total - start) else { throw AtlasSavedError.invalidResponse }
            var pageReferenceBytes: UInt64 = 0
            for (position, entry) in w.items.enumerated() {
                guard entry.id == start + UInt64(position) else { throw AtlasSavedError.invalidResponse }
                try entry.validate(in: identity)
                let (sum, overflow) = pageReferenceBytes.addingReportingOverflow(entry.range.end - entry.range.start)
                guard !overflow else { throw AtlasSavedError.invalidResponse }
                pageReferenceBytes = sum
            }
            let end = start + UInt64(w.items.count)
            guard w.next_offset.value?.value == (end < total ? end : nil),
                  pageReferenceBytes <= w.reference_bytes.value,
                  start != 0 || end != total || pageReferenceBytes == w.reference_bytes.value,
                  UInt64(w.items.filter(\.canOpen).count) <= ready,
                  UInt64(w.items.filter { !$0.canOpen }.count) <= total - ready else { throw AtlasSavedError.invalidResponse }
            return Self(identity: identity, generation: generation, lastAttempt: w.last_trail_attempt.value,
                digest: w.trail_digest, path: w.trail_path, title: w.title, parentDigest: w.parent_digest.value,
                total: total, referenceBytes: w.reference_bytes.value, retainedBytes: w.retained_trail_bytes.value,
                ready: ready, unavailable: total - ready, readCalls: w.trail_read_calls.value,
                start: start, limit: limit, items: w.items, next: w.next_offset.value?.value)
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }
    static func decodeDetach(_ json: String?, identity: AtlasSavedIdentity, generation: UInt64) throws {
        let data = try AtlasSavedWire.admit(json, handle: identity.owner, command: "trail-detach", identity: identity)
        struct Wire: Decodable {
            let accepted_trail_generation: AtlasSavedSearchOptional<AtlasSavedNumber>
            let last_trail_attempt: AtlasSavedNumber
        }
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            guard generation > 0, w.accepted_trail_generation.value == nil,
                  w.last_trail_attempt.value == generation else { throw AtlasSavedError.invalidResponse }
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }
}

enum AtlasSavedTrailRequest: Sendable {
    case attach(generation: UInt64, path: String)
    case page(generation: UInt64, start: UInt64, limit: UInt64)
    case detach(generation: UInt64)
    case openReader(reader: UInt64, generation: UInt64, item: UInt64)
}

/// Optional companion to the existing archive transport. Cancellation and
/// lifetime still belong to that archive handle and its reader registry.
struct AtlasSavedTrailTransport: Sendable {
    let run: @Sendable (UInt64, AtlasSavedTrailRequest) -> String?
    var write: (@Sendable (UInt64, AtlasSavedTrailWriteRequest) -> String?)? = nil
    var writeReceipt: (@Sendable (UInt64, UInt64) -> String?)? = nil
}
