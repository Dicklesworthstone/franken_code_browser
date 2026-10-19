import Foundation

enum AtlasSavedError: Error, Sendable {
    case unavailable, invalidInput, invalidResponse, canceled, engine(String)
    var message: String {
        switch self {
        case .unavailable: return "The saved repository could not admit this operation."
        case .invalidInput: return "Choose a valid saved archive, member or documentation link."
        case .invalidResponse: return "The saved repository response could not be verified. Reload its accepted metadata before continuing."
        case .canceled: return "Saved repository work was canceled. Entered work may still be draining."
        case .engine(let code): return "Saved repository operation unavailable (\(code))."
        }
    }
}

struct AtlasSavedNumber: Decodable, Sendable {
    let value: UInt64
    init(from decoder: Decoder) throws {
        let text = try decoder.singleValueContainer().decode(String.self)
        guard let number = AtlasReaderWire.integer(text) else { throw AtlasSavedError.invalidResponse }
        value = number
    }
}

struct AtlasSavedPath: Decodable, Sendable, Equatable {
    let bytes: [UInt8]
    let display: String
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        guard try c.decode(String.self, forKey: .encoding) == "unix-bytes" else { throw AtlasSavedError.invalidResponse }
        bytes = try AtlasReaderWire.pathBytes(c.decode(String.self, forKey: .hex))
        display = try c.decode(String.self, forKey: .display)
        guard !bytes.isEmpty, !bytes.contains(0), display.utf8.count <= 65_536 else { throw AtlasSavedError.invalidResponse }
    }
    var isMember: Bool {
        bytes.count <= 2048 && bytes.first != 47 &&
            !bytes.split(separator: 47, omittingEmptySubsequences: false).contains {
                $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
            }
    }
    enum CodingKeys: CodingKey { case encoding, hex, display }
}

struct AtlasSavedIdentity: Decodable, Sendable, Equatable {
    let owner: UInt64
    let digest: String
    let members: UInt64
    let captured: UInt64
    let discoveryComplete: Bool
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        owner = try c.decode(AtlasSavedNumber.self, forKey: .owner).value
        digest = try c.decode(String.self, forKey: .archive_digest)
        members = try c.decode(AtlasSavedNumber.self, forKey: .members_total).value
        captured = try c.decode(AtlasSavedNumber.self, forKey: .captured_members).value
        discoveryComplete = try c.decode(Bool.self, forKey: .discovery_complete)
        guard owner > 0, AtlasSavedWire.digest(digest), members <= 65_536, captured <= members,
              try !c.decode(Bool.self, forKey: .live_root_authority),
              try !c.decode(Bool.self, forKey: .native_presented) else { throw AtlasSavedError.invalidResponse }
    }
    enum CodingKeys: CodingKey {
        case owner, archive_digest, members_total, captured_members, discovery_complete, live_root_authority, native_presented
    }
}

enum AtlasSavedWire {
    static let maximumResponseBytes = 8 * 1024 * 1024
    static let memberBytes: UInt64 = 1024 * 1024
    static let documentBytes: UInt64 = 256 * 1024
    static let relationshipBytes: UInt64 = 32 * 1024 * 1024
    static let evidencePrecision = "whole-captured-markdown-document"
    static func digest(_ text: String) -> Bool {
        text.utf8.count == 64 && text.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
    static func admit(_ json: String?, handle: UInt64, command: String,
        identity: AtlasSavedIdentity? = nil) throws -> Data {
        guard let json, !json.isEmpty, json.utf8.count <= maximumResponseBytes else { throw AtlasSavedError.unavailable }
        let data = Data(json.utf8)
        struct Header: Decodable {
            let schema, status: String
            let owner: AtlasSavedNumber
            let command: String?
            let error: Failure?
        }
        struct Failure: Decodable { let code: String }
        do {
            let h = try JSONDecoder().decode(Header.self, from: data)
            guard handle > 0, h.schema == "fcb.saved-repository/1", h.owner.value == handle else {
                throw AtlasSavedError.invalidResponse
            }
            if h.status == "error" {
                guard let code = h.error?.code, !code.isEmpty, code.utf8.count <= 512,
                      code.unicodeScalars.allSatisfy({ $0.value >= 32 && $0.value != 127 }) else {
                    throw AtlasSavedError.invalidResponse
                }
                throw AtlasSavedError.engine(code)
            }
            guard h.status == "ok", h.error == nil, h.command == command else { throw AtlasSavedError.invalidResponse }
            let actual = try JSONDecoder().decode(AtlasSavedIdentity.self, from: data)
            guard identity == nil || actual == identity else { throw AtlasSavedError.invalidResponse }
            return data
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }
}

struct AtlasSavedInfo: Sendable {
    let identity: AtlasSavedIdentity
    let path: AtlasSavedPath
    let relationshipGeneration: UInt64?
    let queryGeneration: UInt64?
    let initialArchiveBytes: UInt64
    let memberBytesRead: UInt64
    let memberReadCalls: UInt64

    static func decode(_ json: String?, handle: UInt64, expectedPath: [UInt8],
        identity: AtlasSavedIdentity? = nil, fresh: Bool = false) throws -> Self {
        let data = try AtlasSavedWire.admit(json, handle: handle, command: "info", identity: identity)
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            guard w.path.bytes == expectedPath, w.path.bytes.first == 47,
                  w.initial.value <= 80 * 1024 * 1024,
                  !fresh || (w.relationship == nil && w.query == nil && w.index == nil &&
                    w.memberBytes.value == 0 && w.memberCalls.value == 0) else { throw AtlasSavedError.invalidResponse }
            return Self(identity: try JSONDecoder().decode(AtlasSavedIdentity.self, from: data),
                path: w.path, relationshipGeneration: w.relationship, queryGeneration: w.query, initialArchiveBytes: w.initial.value,
                memberBytesRead: w.memberBytes.value, memberReadCalls: w.memberCalls.value)
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }
    private struct Wire: Decodable {
        let path: AtlasSavedPath
        let relationship, query, index: UInt64?
        let initial, memberBytes, memberCalls: AtlasSavedNumber
        enum CodingKeys: CodingKey {
            case archive_path, accepted_relationship_generation, accepted_query_generation, accepted_index_generation,
                 initial_archive_bytes_read, member_bytes_read, member_read_calls
        }
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            func generation(_ key: CodingKeys) throws -> UInt64? {
                guard c.contains(key) else { throw AtlasSavedError.invalidResponse }
                let value = try c.decodeIfPresent(AtlasSavedNumber.self, forKey: key)?.value
                guard value == nil || value! > 0 else { throw AtlasSavedError.invalidResponse }
                return value
            }
            path = try c.decode(AtlasSavedPath.self, forKey: .archive_path)
            relationship = try generation(.accepted_relationship_generation)
            query = try generation(.accepted_query_generation); index = try generation(.accepted_index_generation)
            initial = try c.decode(AtlasSavedNumber.self, forKey: .initial_archive_bytes_read)
            memberBytes = try c.decode(AtlasSavedNumber.self, forKey: .member_bytes_read)
            memberCalls = try c.decode(AtlasSavedNumber.self, forKey: .member_read_calls)
        }
    }
}

struct AtlasSavedMember: Decodable, Sendable, Identifiable {
    let id: UInt64
    let file: UInt64
    let path: AtlasSavedPath
    let observedBytes: UInt64
    let captured: Bool
    let digest: String?
    let reason: String?
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(AtlasSavedNumber.self, forKey: .member).value
        file = try c.decode(AtlasSavedNumber.self, forKey: .file_id).value
        path = try c.decode(AtlasSavedPath.self, forKey: .path)
        observedBytes = try c.decode(AtlasSavedNumber.self, forKey: .observed_bytes).value
        captured = try c.decode(Bool.self, forKey: .captured)
        digest = try c.decodeIfPresent(String.self, forKey: .source_digest)
        reason = try c.decodeIfPresent(String.self, forKey: .reason)
        guard id < 65_536, file == id + 1, path.isMember else { throw AtlasSavedError.invalidResponse }
        if captured {
            guard observedBytes <= AtlasSavedWire.memberBytes, let digest, AtlasSavedWire.digest(digest),
                  reason == nil else { throw AtlasSavedError.invalidResponse }
        } else {
            guard digest == nil, let reason, !reason.isEmpty, reason.utf8.count <= 96,
                  reason.utf8.allSatisfy({ (65...90).contains($0) || (48...57).contains($0) || $0 == 95 }) else {
                throw AtlasSavedError.invalidResponse
            }
        }
    }
    func matches(_ other: Self) -> Bool {
        id == other.id && file == other.file && path.bytes == other.path.bytes &&
            observedBytes == other.observedBytes && captured == other.captured && digest == other.digest && reason == other.reason
    }
    enum CodingKeys: CodingKey { case member, file_id, path, observed_bytes, captured, source_digest, reason }
}

struct AtlasSavedMembers: Sendable {
    let identity: AtlasSavedIdentity
    let start: UInt64
    let items: [AtlasSavedMember]
    let next: UInt64?
    static func decode(_ json: String?, identity: AtlasSavedIdentity, start: UInt64, limit: UInt64) throws -> Self {
        let data = try AtlasSavedWire.admit(json, handle: identity.owner, command: "members", identity: identity)
        struct Wire: Decodable { let members: [AtlasSavedMember]; let next_offset: AtlasSavedNumber? }
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            guard (1...128).contains(limit), start <= identity.members,
                  UInt64(w.members.count) == min(limit, identity.members - start) else { throw AtlasSavedError.invalidResponse }
            for (position, member) in w.members.enumerated() {
                guard member.id == start + UInt64(position) else { throw AtlasSavedError.invalidResponse }
            }
            let end = start + UInt64(w.members.count)
            guard w.next_offset?.value == (end < identity.members ? end : nil) else { throw AtlasSavedError.invalidResponse }
            return Self(identity: identity, start: start, items: w.members, next: w.next_offset?.value)
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }
}

enum AtlasSavedDirection: String, CaseIterable, Identifiable, Sendable {
    case all, outgoing, incoming
    var id: String { rawValue }
    var label: String { switch self { case .all: return "All links"; case .outgoing: return "Outgoing"; case .incoming: return "Backlinks" } }
    var code: UInt64 { switch self { case .all: return 0; case .outgoing: return 1; case .incoming: return 2 } }
}
enum AtlasSavedLinkSide: String, Sendable { case evidence, target; var code: UInt64 { self == .evidence ? 0 : 1 } }

struct AtlasSavedLink: Decodable, Sendable, Identifiable {
    let id: UInt64
    let sourceMember: UInt64
    let sourceFile: UInt64
    let sourceRevision: UInt64
    let sourceDigest: String
    let sourcePath: AtlasSavedPath
    let evidenceEnd: UInt64
    let destination: String
    let resolution: String
    let targetMember: UInt64?
    let targetPath: AtlasSavedPath?
    let suffix: String
    let fragmentUnresolved: Bool
    var canOpenTarget: Bool { resolution == "captured" }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(AtlasSavedNumber.self, forKey: .link_id).value
        sourceMember = try c.decode(AtlasSavedNumber.self, forKey: .source_member).value
        sourceFile = try c.decode(AtlasSavedNumber.self, forKey: .source_file_id).value
        sourceRevision = try c.decode(AtlasSavedNumber.self, forKey: .source_revision).value
        sourceDigest = try c.decode(String.self, forKey: .source_digest)
        sourcePath = try c.decode(AtlasSavedPath.self, forKey: .source_path)
        let range = try c.decode(AtlasReaderWire.Range.self, forKey: .evidence_range).values()
        evidenceEnd = range.end
        destination = try c.decode(String.self, forKey: .destination)
        resolution = try c.decode(String.self, forKey: .resolution)
        guard c.contains(.target_member), c.contains(.target_path) else { throw AtlasSavedError.invalidResponse }
        targetMember = try c.decodeIfPresent(AtlasSavedNumber.self, forKey: .target_member)?.value
        targetPath = try c.decodeIfPresent(AtlasSavedPath.self, forKey: .target_path)
        suffix = try c.decode(String.self, forKey: .target_suffix)
        let fragment = try c.decode(String.self, forKey: .fragment_resolution)
        fragmentUnresolved = fragment == "unresolved"
        let occurrence = try c.decode(AtlasSavedNumber.self, forKey: .source_link_id).value
        _ = try c.decode(AtlasSavedNumber.self, forKey: .block_index) // Parser ordering only, never a coordinate.
        guard (1...4096).contains(id), sourceMember < 65_536, sourceFile == sourceMember + 1,
              sourceRevision == sourceFile, AtlasSavedWire.digest(sourceDigest), sourcePath.isMember,
              range.start == 0, evidenceEnd > 0, evidenceEnd <= AtlasSavedWire.documentBytes,
              (1...131_072).contains(occurrence), destination.utf8.count <= 4096,
              suffix.utf8.count <= destination.utf8.count,
              ["none", "unresolved"].contains(fragment), fragmentUnresolved == suffix.contains("#"),
              ["captured", "unavailable", "missing", "external", "unresolved"].contains(resolution) else {
            throw AtlasSavedError.invalidResponse
        }
        if resolution == "captured" || resolution == "unavailable" {
            guard let targetMember, targetMember < 65_536, let targetPath, targetPath.isMember else {
                throw AtlasSavedError.invalidResponse
            }
            if targetMember == sourceMember {
                guard resolution == "captured", targetPath.bytes == sourcePath.bytes else { throw AtlasSavedError.invalidResponse }
            }
        } else {
            guard targetMember == nil, targetPath == nil else { throw AtlasSavedError.invalidResponse }
        }
    }
    func matches(_ other: Self) -> Bool {
        id == other.id && sourceMember == other.sourceMember && sourceDigest == other.sourceDigest &&
            sourcePath.bytes == other.sourcePath.bytes && evidenceEnd == other.evidenceEnd &&
            destination.utf8.elementsEqual(other.destination.utf8) && resolution == other.resolution &&
            targetMember == other.targetMember && targetPath?.bytes == other.targetPath?.bytes &&
            suffix.utf8.elementsEqual(other.suffix.utf8) && fragmentUnresolved == other.fragmentUnresolved
    }
    enum CodingKeys: CodingKey {
        case link_id, source_member, source_file_id, source_revision, source_digest, source_path, evidence_range,
             destination, resolution, target_member, target_path, target_suffix, fragment_resolution, source_link_id, block_index
    }
}

struct AtlasSavedRelationships: Sendable {
    let identity: AtlasSavedIdentity
    let generation: UInt64
    let direction: AtlasSavedDirection
    let member: UInt64?
    let start: UInt64
    let links: [AtlasSavedLink]
    let next: UInt64?
    let complete: Bool
    let truncated: Bool
    let markdown, scanned, unavailable, unsupported, sourceLimited, diagnostics, retained, seen, matching, sourceBytes: UInt64

    static func decode(_ json: String?, identity: AtlasSavedIdentity, generation: UInt64,
        direction: AtlasSavedDirection, member: UInt64?, start: UInt64, limit: UInt64, preparing: Bool = false) throws -> Self {
        let data = try AtlasSavedWire.admit(json, handle: identity.owner,
            command: preparing ? "relationships-prepare" : "relationships", identity: identity)
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            let markdown = w.markdown_members.value, scanned = w.scanned_markdown_members.value
            let unavailable = w.unavailable_markdown_members.value, unsupported = w.unsupported_markdown_members.value
            let limited = w.source_limited_members.value, diagnostics = w.diagnostic_markdown_members.value
            guard generation > 0, w.relationship_generation.value == generation,
                  w.projection == "markdown-document-links", w.evidence_precision == AtlasSavedWire.evidencePrecision,
                  w.resolver == "fmd-exact-member-v1", !w.extractor.isEmpty, w.extractor.utf8.count <= 256,
                  w.completeness_scope == "parsed-document-links-in-saved-manifest",
                  w.direction == direction.rawValue, w.filter_member?.value == member,
                  (direction == .all) == (member == nil), member == nil || member! < identity.members,
                  (1...128).contains(limit), markdown <= identity.members, scanned <= markdown,
                  unavailable <= markdown, unsupported <= markdown, limited <= markdown, diagnostics <= scanned,
                  scanned + unavailable + unsupported + limited == markdown,
                  w.retained_links.value <= 4096, w.links_seen.value >= w.retained_links.value,
                  w.matching_links.value <= w.retained_links.value,
                  w.retained_source_payload_bytes.value == 0, w.member_bytes_read.value <= AtlasSavedWire.relationshipBytes,
                  w.relationships_complete == (identity.discoveryComplete && scanned == markdown && diagnostics == 0 && !w.truncated),
                  start <= w.matching_links.value,
                  UInt64(w.links.count) == min(limit, w.matching_links.value - start) else { throw AtlasSavedError.invalidResponse }
            _ = w.member_read_calls.value
            var lastID: UInt64 = 0
            for (position, link) in w.links.enumerated() {
                guard link.id > lastID, link.id <= w.retained_links.value, link.sourceMember < identity.members,
                      link.targetMember == nil || link.targetMember! < identity.members else { throw AtlasSavedError.invalidResponse }
                switch direction {
                case .all: guard link.id == start + UInt64(position) + 1 else { throw AtlasSavedError.invalidResponse }
                case .outgoing: guard link.sourceMember == member else { throw AtlasSavedError.invalidResponse }
                case .incoming: guard link.targetMember == member else { throw AtlasSavedError.invalidResponse }
                }
                lastID = link.id
            }
            let end = start + UInt64(w.links.count)
            guard w.next_offset?.value == (end < w.matching_links.value ? end : nil) else { throw AtlasSavedError.invalidResponse }
            return Self(identity: identity, generation: generation, direction: direction, member: member,
                start: start, links: w.links, next: w.next_offset?.value, complete: w.relationships_complete,
                truncated: w.truncated, markdown: markdown, scanned: scanned, unavailable: unavailable,
                unsupported: unsupported, sourceLimited: limited, diagnostics: diagnostics,
                retained: w.retained_links.value, seen: w.links_seen.value, matching: w.matching_links.value,
                sourceBytes: w.member_bytes_read.value)
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }
    private struct Wire: Decodable {
        let relationship_generation: AtlasSavedNumber
        let projection, evidence_precision, resolver, extractor, completeness_scope, direction: String
        let relationships_complete, truncated: Bool
        let markdown_members, scanned_markdown_members, unavailable_markdown_members, unsupported_markdown_members: AtlasSavedNumber
        let source_limited_members, diagnostic_markdown_members, retained_links, links_seen, matching_links: AtlasSavedNumber
        let member_bytes_read, member_read_calls, retained_source_payload_bytes: AtlasSavedNumber
        let filter_member, next_offset: AtlasSavedNumber?
        let links: [AtlasSavedLink]
    }
}

enum AtlasSavedRequest: Sendable {
    case open(path: String), info
    case members(start: UInt64, limit: UInt64)
    case search(generation: UInt64, needle: String, matches: UInt64)
    case results(generation: UInt64, start: UInt64, limit: UInt64)
    case clearResults(generation: UInt64)
    case openHit(reader: UInt64, generation: UInt64, hit: UInt64)
    case prepareRelationships(generation: UInt64, links: UInt64, bytes: UInt64)
    case relationships(generation: UInt64, direction: AtlasSavedDirection, member: UInt64?, start: UInt64, limit: UInt64)
    case clearRelationships(generation: UInt64)
    case openMember(reader: UInt64, member: UInt64)
    case openRelationship(reader: UInt64, generation: UInt64, link: UInt64, side: AtlasSavedLinkSide)
}

struct AtlasSavedTransport: Sendable {
    let create: @Sendable () -> UInt64
    let run: @Sendable (UInt64, AtlasSavedRequest) -> String?
    let cancel: @Sendable (UInt64) -> Bool
    let close: @Sendable (UInt64) -> Bool
    let retirementFailed: @Sendable () -> Void
    let readerDomain: UUID?
}
