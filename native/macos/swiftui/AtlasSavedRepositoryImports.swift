import Foundation

/// Saved imports are lexical statements in verified archive members. The
/// parser and closed-catalog resolver remain in the host; Swift verifies their
/// bounded receipts and never treats a displayed specifier as a pathname.
enum AtlasSavedImportWire {
    static let maximumLinks: UInt64 = 4096
    static let maximumSourceBytes: UInt64 = 32 * 1024 * 1024
    static let maximumMemberBytes: UInt64 = 128 * 1024
    // Verified snapshot reads count interrupted attempts as well as bytes.
    static let maximumMemberReadCalls: UInt64 = 1_048_576

    static func validateContract(_ data: Data) throws {
        struct Contract: Decodable {
            let projection, relationship_kind, evidence_precision, resolution_domain: String
            let extractor, extraction_policy, resolver: String
            let compiler_resolved, semantic_complete: Bool
        }
        let w = try JSONDecoder().decode(Contract.self, from: data)
        guard w.projection == "lexical-source-imports", w.relationship_kind == "lexical-import",
              w.evidence_precision == "exact-import-source-range",
              w.resolution_domain == "declared-catalog-candidates",
              w.extractor == "franken-markdown-d6fbd1d5/import-statements-v3",
              w.extraction_policy == "rust-use-tree-js-static-and-python-import-v3",
              w.resolver == "fcb-closed-catalog-imports-v3",
              !w.compiler_resolved, !w.semantic_complete else { throw AtlasSavedError.invalidResponse }
    }

    static func clear(_ json: String?, identity: AtlasSavedIdentity, generation: UInt64) throws {
        let data = try AtlasSavedWire.admit(json, handle: identity.owner, command: "imports-clear", identity: identity)
        guard generation > 0,
              let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              object["accepted_import_generation"] is NSNull,
              (object["last_import_attempt"] as? String).flatMap(AtlasReaderWire.integer) == generation,
              object["retained_links"] as? String == "0" else { throw AtlasSavedError.invalidResponse }
    }
}

struct AtlasSavedImportRange: Decodable, Sendable, Equatable {
    let start, end: UInt64
    var contextStart: UInt64 { start > 128 ? start - 128 : 0 }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        start = try c.decode(AtlasSavedNumber.self, forKey: .start).value
        end = try c.decode(AtlasSavedNumber.self, forKey: .end).value
        guard start < end, end <= AtlasSavedImportWire.maximumMemberBytes else { throw AtlasSavedError.invalidResponse }
    }
    enum CodingKeys: CodingKey { case start, end }
}

struct AtlasSavedImport: Decodable, Sendable, Identifiable {
    let id, sourceMember, sourceFile, sourceRevision, sourceBytes, sourceOccurrence: UInt64
    let sourceDigest: String
    let sourcePath: AtlasSavedPath
    let evidence, specifier: AtlasSavedImportRange
    let language, kind, destination, resolution, resolutionBasis: String
    let targetMember, targetFile, targetRevision, targetBytes: UInt64?
    let targetDigest: String?
    let targetPath: AtlasSavedPath?

    var canOpenTarget: Bool { resolution == "captured" }
    var label: String {
        switch kind {
        case "rust-module": return "Rust module declaration"
        case "rust-use": return "Rust use statement"
        case "python-import": return "Python import statement"
        case "python-from": return "Python from-import statement"
        case "javascript-export": return "Re-export statement"
        default: return "Import statement"
        }
    }
    var resolutionLabel: String {
        let modulePrefix = resolutionBasis == "rust-use-module-prefix-candidate"
        switch resolution {
        case "captured": return modulePrefix ? "Captured containing-module candidate" : "Captured catalog target candidate"
        case "unavailable": return modulePrefix ? "Containing-module candidate recorded without source bytes"
            : "Target recorded without source bytes"
        case "missing": return "No target in the saved manifest"
        case "ambiguous": return resolutionBasis == "ambiguous-rust-crate-root"
            ? "Several crate roots are possible; no target was selected" : "Several catalog targets are possible"
        case "external": return "External specifier"
        default: return "No supported catalog target established"
        }
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        func number(_ key: CodingKeys) throws -> UInt64 { try c.decode(AtlasSavedNumber.self, forKey: key).value }
        func optionalNumber(_ key: CodingKeys) throws -> UInt64? {
            guard c.contains(key) else { throw AtlasSavedError.invalidResponse }
            return try c.decodeIfPresent(AtlasSavedNumber.self, forKey: key)?.value
        }
        id = try number(.link_id); sourceMember = try number(.source_member)
        sourceFile = try number(.source_file_id); sourceRevision = try number(.source_revision)
        sourceBytes = try number(.source_bytes); sourceOccurrence = try number(.source_link_id)
        sourceDigest = try c.decode(String.self, forKey: .source_digest)
        sourcePath = try c.decode(AtlasSavedPath.self, forKey: .source_path)
        evidence = try c.decode(AtlasSavedImportRange.self, forKey: .evidence_range)
        specifier = try c.decode(AtlasSavedImportRange.self, forKey: .specifier_range)
        language = try c.decode(String.self, forKey: .language)
        kind = try c.decode(String.self, forKey: .lexical_import_kind)
        destination = try c.decode(String.self, forKey: .destination)
        resolution = try c.decode(String.self, forKey: .resolution)
        resolutionBasis = try c.decode(String.self, forKey: .resolution_basis)
        targetMember = try optionalNumber(.target_member); targetFile = try optionalNumber(.target_file_id)
        targetRevision = try optionalNumber(.target_revision); targetBytes = try optionalNumber(.target_bytes)
        guard c.contains(.target_digest), c.contains(.target_path) else { throw AtlasSavedError.invalidResponse }
        targetDigest = try c.decodeIfPresent(String.self, forKey: .target_digest)
        targetPath = try c.decodeIfPresent(AtlasSavedPath.self, forKey: .target_path)
        let kinds: [String]
        switch language {
        case "rust": kinds = ["rust-module", "rust-use"]
        case "python": kinds = ["python-import", "python-from"]
        case "javascript", "typescript", "jsx", "tsx": kinds = ["javascript-import", "javascript-export"]
        default: throw AtlasSavedError.invalidResponse
        }
        guard id > 0, id <= AtlasSavedImportWire.maximumLinks, sourceMember < 65_536,
              sourceFile == sourceMember + 1, sourceRevision == sourceFile, sourceOccurrence > 0,
              sourcePath.isMember, AtlasSavedWire.digest(sourceDigest),
              sourceBytes <= AtlasSavedImportWire.maximumMemberBytes, evidence.end <= sourceBytes,
              specifier.start >= evidence.start, specifier.end <= evidence.end,
              !destination.isEmpty, destination.utf8.count <= 1024, kinds.contains(kind),
              ["captured", "unavailable", "missing", "ambiguous", "external", "unresolved"].contains(resolution),
              ["exact-relative-path", "unique-relative-module-candidate", "rust-module-file-candidate",
               "rust-use-module-file-candidate", "rust-use-module-prefix-candidate", "ambiguous-rust-crate-root",
               "python-relative-module-candidate", "unresolved-rust-use", "external-specifier",
               "unsupported-specifier", "no-catalog-target", "ambiguous-catalog-target"].contains(resolutionBasis) else {
            throw AtlasSavedError.invalidResponse
        }
        if ["rust-use-module-file-candidate", "rust-use-module-prefix-candidate", "ambiguous-rust-crate-root"]
            .contains(resolutionBasis) && (language != "rust" || kind != "rust-use") {
            throw AtlasSavedError.invalidResponse
        }
        if resolutionBasis == "python-relative-module-candidate" && (language != "python" || kind != "python-from") {
            throw AtlasSavedError.invalidResponse
        }
        if language == "python" && !["python-relative-module-candidate", "external-specifier",
            "unsupported-specifier", "no-catalog-target", "ambiguous-catalog-target"].contains(resolutionBasis) {
            throw AtlasSavedError.invalidResponse
        }
        let acceptedBases: [String]
        switch resolution {
        case "captured", "unavailable": acceptedBases = ["exact-relative-path", "unique-relative-module-candidate",
            "rust-module-file-candidate", "rust-use-module-file-candidate", "rust-use-module-prefix-candidate",
            "python-relative-module-candidate"]
        case "missing": acceptedBases = ["no-catalog-target"]
        case "ambiguous": acceptedBases = ["ambiguous-catalog-target", "ambiguous-rust-crate-root"]
        case "external": acceptedBases = ["external-specifier"]
        default: acceptedBases = ["unsupported-specifier", "unresolved-rust-use"]
        }
        guard acceptedBases.contains(resolutionBasis) else { throw AtlasSavedError.invalidResponse }
        if resolution == "captured" || resolution == "unavailable" {
            guard let targetMember, targetMember < 65_536, targetFile == targetMember + 1,
                  targetPath?.isMember == true else { throw AtlasSavedError.invalidResponse }
        } else {
            guard targetMember == nil, targetFile == nil, targetPath == nil else { throw AtlasSavedError.invalidResponse }
        }
        if resolution == "captured" {
            guard targetRevision == targetFile, let targetDigest, AtlasSavedWire.digest(targetDigest),
                  let targetBytes, targetBytes <= AtlasSavedWire.memberBytes else { throw AtlasSavedError.invalidResponse }
        } else {
            guard targetRevision == nil, targetDigest == nil, targetBytes == nil else { throw AtlasSavedError.invalidResponse }
        }
    }
    func matches(_ other: Self) -> Bool {
        id == other.id && sourceMember == other.sourceMember && sourceFile == other.sourceFile
            && sourceRevision == other.sourceRevision && sourceDigest == other.sourceDigest
            && sourcePath.bytes == other.sourcePath.bytes && sourceBytes == other.sourceBytes
            && sourceOccurrence == other.sourceOccurrence && evidence == other.evidence && specifier == other.specifier
            && language == other.language && kind == other.kind && destination.utf8.elementsEqual(other.destination.utf8)
            && resolution == other.resolution && resolutionBasis == other.resolutionBasis
            && targetMember == other.targetMember && targetFile == other.targetFile
            && targetPath?.bytes == other.targetPath?.bytes && targetRevision == other.targetRevision
            && targetDigest == other.targetDigest && targetBytes == other.targetBytes
    }
    func matches(_ member: AtlasSavedMember, side: AtlasSavedLinkSide) -> Bool {
        guard member.captured else { return false }
        if side == .evidence {
            return member.id == sourceMember && member.file == sourceFile && member.digest == sourceDigest
                && member.path.bytes == sourcePath.bytes && member.observedBytes == sourceBytes
        }
        return canOpenTarget && member.id == targetMember && member.file == targetFile
            && member.digest == targetDigest && member.path.bytes == targetPath?.bytes && member.observedBytes == targetBytes
    }
    enum CodingKeys: CodingKey {
        case link_id, source_member, source_file_id, source_revision, source_digest, source_path, source_bytes,
             source_link_id, evidence_range, specifier_range, language, lexical_import_kind, destination,
             resolution, resolution_basis, target_member, target_file_id, target_revision, target_digest, target_path, target_bytes
    }
}

struct AtlasSavedImports: Sendable {
    let identity: AtlasSavedIdentity
    let generation: UInt64
    let direction: AtlasSavedDirection
    let member: UInt64?
    let start, limit, matching: UInt64
    let links: [AtlasSavedImport]
    let next: UInt64?
    let complete, truncated: Bool
    let sources, scanned, unavailable, unsupported, sourceLimited, diagnosticSources, diagnostics, retained, seen: UInt64
    let sourceBytes, readCalls, peakSourceBytes, maxLinks, maxSourceBytes: UInt64

    func matchesInventory(_ other: Self) -> Bool {
        identity == other.identity && generation == other.generation && direction == other.direction && member == other.member
            && complete == other.complete && truncated == other.truncated && matching == other.matching
            && sources == other.sources && scanned == other.scanned && unavailable == other.unavailable
            && unsupported == other.unsupported && sourceLimited == other.sourceLimited
            && diagnosticSources == other.diagnosticSources && diagnostics == other.diagnostics
            && retained == other.retained && seen == other.seen && sourceBytes == other.sourceBytes
            && readCalls == other.readCalls && peakSourceBytes == other.peakSourceBytes
            && maxLinks == other.maxLinks && maxSourceBytes == other.maxSourceBytes
    }
    static func decode(_ json: String?, identity: AtlasSavedIdentity, generation: UInt64,
        direction: AtlasSavedDirection, member: UInt64?, start: UInt64, limit: UInt64, preparing: Bool = false) throws -> Self {
        let data = try AtlasSavedWire.admit(json, handle: identity.owner,
            command: preparing ? "imports-prepare" : "imports", identity: identity)
        do {
            try AtlasSavedImportWire.validateContract(data)
            let w = try JSONDecoder().decode(Wire.self, from: data)
            let sources = w.source_members.value, scanned = w.scanned_source_members.value
            let unavailable = w.unavailable_source_members.value, unsupported = w.unsupported_source_members.value
            let limited = w.source_limited_members.value, diagnostic = w.diagnostic_source_members.value
            guard generation > 0, w.import_generation.value == generation, w.direction == direction.rawValue,
                  w.completeness_scope == "parsed-source-imports-in-saved-manifest",
                  w.source_observation == "verified-saved-members",
                  w.filter_member?.value == member, (direction == .all) == (member == nil),
                  member == nil || member! < identity.members, (1...128).contains(limit),
                  w.offset.value == start, w.limit.value == limit,
                  sources <= identity.members, scanned <= min(sources, identity.captured),
                  unavailable <= sources, unsupported <= sources, limited <= sources, diagnostic <= scanned,
                  scanned + unavailable + unsupported + limited == sources,
                  w.parser_diagnostics.value >= diagnostic, (w.parser_diagnostics.value == 0) == (diagnostic == 0),
                  (1...AtlasSavedImportWire.maximumLinks).contains(w.max_links.value),
                  w.max_source_bytes.value <= AtlasSavedImportWire.maximumSourceBytes,
                  w.max_member_source_bytes.value == AtlasSavedImportWire.maximumMemberBytes,
                  w.retained_links.value <= w.max_links.value, w.links_seen.value >= w.retained_links.value,
                  w.matching_links.value <= w.retained_links.value, w.retained_source_payload_bytes.value == 0,
                  w.member_bytes_read.value <= w.max_source_bytes.value,
                  w.peak_source_bytes.value <= min(w.member_bytes_read.value, AtlasSavedImportWire.maximumMemberBytes),
                  w.member_read_calls.value <= (scanned + unsupported) * AtlasSavedImportWire.maximumMemberReadCalls,
                  w.imports_complete == (identity.discoveryComplete && scanned == sources && diagnostic == 0 && !w.truncated),
                  start <= w.matching_links.value,
                  UInt64(w.links.count) <= min(limit, w.matching_links.value - start),
                  !w.links.isEmpty || start == w.matching_links.value else { throw AtlasSavedError.invalidResponse }
            var lastID: UInt64 = 0
            for (position, link) in w.links.enumerated() {
                guard link.id > lastID, link.id <= w.retained_links.value, link.sourceMember < identity.members,
                      link.targetMember == nil || link.targetMember! < identity.members,
                      direction != .all || link.id == start + UInt64(position) + 1,
                      direction != .outgoing || link.sourceMember == member,
                      direction != .incoming || link.targetMember == member else { throw AtlasSavedError.invalidResponse }
                lastID = link.id
            }
            let end = start + UInt64(w.links.count)
            let next = end < w.matching_links.value ? end : nil
            guard w.next_offset?.value == next else { throw AtlasSavedError.invalidResponse }
            return Self(identity: identity, generation: generation, direction: direction, member: member,
                start: start, limit: limit, matching: w.matching_links.value, links: w.links, next: next,
                complete: w.imports_complete, truncated: w.truncated, sources: sources, scanned: scanned,
                unavailable: unavailable, unsupported: unsupported, sourceLimited: limited, diagnosticSources: diagnostic,
                diagnostics: w.parser_diagnostics.value, retained: w.retained_links.value, seen: w.links_seen.value,
                sourceBytes: w.member_bytes_read.value, readCalls: w.member_read_calls.value,
                peakSourceBytes: w.peak_source_bytes.value, maxLinks: w.max_links.value, maxSourceBytes: w.max_source_bytes.value)
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }
    private struct Wire: Decodable {
        let import_generation, offset, limit, matching_links: AtlasSavedNumber
        let direction, completeness_scope, source_observation: String
        let filter_member, next_offset: AtlasSavedNumber?
        let imports_complete, truncated: Bool
        let source_members, scanned_source_members, unavailable_source_members, unsupported_source_members: AtlasSavedNumber
        let source_limited_members, diagnostic_source_members, parser_diagnostics, retained_links, links_seen: AtlasSavedNumber
        let member_bytes_read, member_read_calls, peak_source_bytes, retained_source_payload_bytes: AtlasSavedNumber
        let max_links, max_source_bytes, max_member_source_bytes: AtlasSavedNumber
        let links: [AtlasSavedImport]
    }
}
