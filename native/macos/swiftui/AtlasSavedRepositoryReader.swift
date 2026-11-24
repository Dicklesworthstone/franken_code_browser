import Foundation

enum AtlasSavedReaderOrigin: Sendable {
    case member
    case trail(generation: UInt64, digest: String, entry: AtlasSavedTrailEntry)
    case pathHit(generation: UInt64, hit: AtlasSavedPathHit)
    case relationship(generation: UInt64, link: AtlasSavedLink, side: AtlasSavedLinkSide)
    case importLink(generation: UInt64, link: AtlasSavedImport, side: AtlasSavedLinkSide)
    case hit(generation: UInt64, needle: String, hit: AtlasSavedHit, caseSensitive: Bool = true)
    case expressionHit(generation: UInt64, expression: String, primaryNeedle: String, hit: AtlasSavedHit,
        caseSensitive: Bool = true)
    case structureHit(generation: UInt64, mode: AtlasSavedSearchMode, nameMode: AtlasSearchNameMode,
        needle: String, hit: AtlasSavedStructureHit, policy: AtlasSavedStructurePolicy = .standard)
    var matchingCaseSensitive: Bool {
        switch self {
        case .hit(_, _, _, let caseSensitive), .expressionHit(_, _, _, _, let caseSensitive): return caseSensitive
        default: return true
        }
    }
    var searchHit: AtlasSavedHit? {
        switch self {
        case .hit(_, _, let hit, _), .expressionHit(_, _, _, let hit, _): return hit
        case .structureHit(_, _, _, _, let item, _): return item.hit
        case .member, .trail, .relationship, .importLink, .pathHit: return nil
        }
    }
}

/// Original-byte evidence for one accepted saved-search occurrence. It does not
/// pretend to be a shaped text selection or a query in the independent reader.
struct AtlasSavedMatchEvidence: Sendable {
    let identity: AtlasReaderIdentity
    let start: UInt64
    let end: UInt64
    let originalHex: String
    var contextStart: UInt64 { start > 132 ? start - 132 : 0 }

    func validate(_ page: AtlasReaderPage) throws {
        guard page.identity.matches(identity), page.start <= start, start < end, end <= page.end,
              AtlasReaderWire.validHex(originalHex, bytes: end - start) else { throw AtlasSavedError.invalidResponse }
        let offset = Int(start - page.start) * 2
        let count = Int(end - start) * 2
        guard page.originalHex.utf8.dropFirst(offset).prefix(count).elementsEqual(originalHex.utf8) else {
            throw AtlasSavedError.invalidResponse
        }
    }

    /// Encoding the submitted literal is not source decoding or a geometry map.
    /// BOMs are never added or stripped from the needle: in-content U+FEFF can
    /// itself be a valid exact match. Swift canonical String equality is avoided.
    static func literalHex(_ needle: String, encoding: String) throws -> String {
        guard AtlasSavedSearchWire.validNeedle(needle) else { throw AtlasSavedError.invalidResponse }
        return try sourceHex(needle, encoding: encoding)
    }

    /// Encode the host's actual verified source spelling, including complete
    /// scalars contributing to a lowercase expansion. Matching stays in Rust.
    static func sourceHex(_ spelling: String, encoding: String) throws -> String {
        guard !spelling.isEmpty, spelling.utf8.count <= 12 * AtlasSavedSearchWire.maximumNeedleBytes,
              !spelling.utf8.contains(0) else { throw AtlasSavedError.invalidResponse }
        let needle = spelling
        let bytes: [UInt8]
        switch encoding {
        case "utf8": bytes = Array(needle.utf8)
        case "utf16le":
            bytes = needle.utf16.flatMap { [UInt8(truncatingIfNeeded: $0), UInt8(truncatingIfNeeded: $0 >> 8)] }
        case "utf16be":
            bytes = needle.utf16.flatMap { [UInt8(truncatingIfNeeded: $0 >> 8), UInt8(truncatingIfNeeded: $0)] }
        default: throw AtlasSavedError.invalidResponse
        }
        return bytes.map { String(format: "%02x", $0) }.joined()
    }
}

struct AtlasSavedReaderReceipt: Sendable {
    let readerJSON: String
    let window: AtlasReaderPage
    let match: AtlasSavedMatchEvidence?

    static func decode(_ json: String?, saved: AtlasSavedIdentity, member: AtlasSavedMember,
        origin: AtlasSavedReaderOrigin, destination: UInt64) throws -> Self {
        let command: String
        switch origin {
        case .member, .hit, .pathHit: command = "open-reader"
        case .trail: command = "trail-open-reader"
        case .relationship: command = "relationships-open-reader"
        case .importLink: command = "imports-open-reader"
        case .expressionHit: command = "expression-open-reader"
        case .structureHit: command = "structure-open-reader"
        }
        let data = try AtlasSavedWire.admit(json, handle: saved.owner, command: command, identity: saved)
        struct Common: Decodable {
            let member, file_id, source_revision: AtlasSavedNumber
            let source_digest, source_observation: String
            let live_source_reopened: Bool
        }
        do {
            let w = try JSONDecoder().decode(Common.self, from: data)
            guard member.captured, destination > 0, destination != saved.owner,
                  w.member.value == member.id, w.file_id.value == member.file, w.source_revision.value == member.file,
                  w.source_digest == member.digest, w.source_observation == "verified-saved-member",
                  !w.live_source_reopened,
                  let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let readerObject = object["reader"] as? [String: Any],
                  let windowObject = object["window"] as? [String: Any] else { throw AtlasSavedError.invalidResponse }
            func nested(_ object: [String: Any]) throws -> String {
                String(decoding: try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]), as: UTF8.self)
            }
            let readerJSON = try nested(readerObject)
            let info = try AtlasReaderInfo.decodeRetainedInitialization(readerJSON, handle: destination, pathBytes: member.path.bytes)
            guard info.identity.capturedBytes == member.observedBytes, info.captureSHA256 == member.digest else {
                throw AtlasSavedError.invalidResponse
            }
            let caseSensitive = origin.matchingCaseSensitive
            let request: AtlasReaderRequest
            let match: AtlasSavedMatchEvidence?
            let selectionGeneration: String
            if case .expressionHit(let generation, let expression, let primary, _, _) = origin {
                guard AtlasSavedSearchWire.validNeedle(expression),
                      object["selection_namespace"] as? String == "saved-expression",
                      (object["expression_generation"] as? String).flatMap(AtlasReaderWire.integer) == generation,
                      let acceptedPrimary = object["primary_needle"] as? String,
                      acceptedPrimary.utf8.elementsEqual(primary.utf8) else { throw AtlasSavedError.invalidResponse }
                if object["case_sensitive"] != nil || object["mode"] != nil || object["normalization"] != nil || !caseSensitive {
                    struct ExpressionPolicy: Decodable { let case_sensitive: Bool; let mode, normalization: String }
                    let policy = try JSONDecoder().decode(ExpressionPolicy.self, from: data)
                    guard policy.case_sensitive == caseSensitive,
                          policy.mode == (caseSensitive ? "decoded-text-expression" : "unicode-lowercase-expression"),
                          policy.normalization == "exact" else { throw AtlasSavedError.invalidResponse }
                }
                selectionGeneration = "expression_generation"
            } else { selectionGeneration = "query_generation" }
            switch origin {
            case .trail(let generation, let digest, let entry):
                struct TrailReceipt: Decodable {
                    let trail_generation, item, reader_owner, source_bytes, offset: AtlasSavedNumber
                    let trail_digest: String
                    let path: AtlasSavedPath
                    let entry: AtlasSavedTrailEntry
                    let selection: AtlasSavedTrailRange
                    let member_bytes_read, member_read_calls: AtlasSavedNumber
                }
                let accepted = try JSONDecoder().decode(TrailReceipt.self, from: data)
                try entry.validate(in: saved)
                guard generation > 0, AtlasSavedWire.digest(digest), entry.matches(member),
                      accepted.trail_generation.value == generation, accepted.trail_digest == digest,
                      accepted.item.value == entry.id, accepted.reader_owner.value == destination,
                      accepted.entry.matches(entry), accepted.path.bytes == member.path.bytes,
                      accepted.source_bytes.value == member.observedBytes,
                      accepted.selection == entry.range, accepted.offset.value == entry.range.start,
                      accepted.member_bytes_read.value == member.observedBytes,
                      (member.observedBytes == 0 && accepted.member_read_calls.value == 0) ||
                        (member.observedBytes > 0 && accepted.member_read_calls.value > 0 &&
                            accepted.member_read_calls.value <= 1_048_576) else { throw AtlasSavedError.invalidResponse }
                // A trail range may be a point, a whole file, or span many
                // reader pages. Its exact anchor does not require duplicating
                // the selected payload into a giant hexadecimal witness.
                request = .window(offset: entry.range.contextStart, bytes: 16 * 1024)
                match = nil
            case .member, .pathHit:
                if case .pathHit(let generation, let hit) = origin {
                    guard generation > 0, hit.member.matches(member) else { throw AtlasSavedError.invalidResponse }
                }
                guard object["selection"] is NSNull,
                      (object["reader_owner"] as? String).flatMap(AtlasReaderWire.integer) == destination else {
                    throw AtlasSavedError.invalidResponse
                }
                request = .window(offset: 0, bytes: 16 * 1024); match = nil
            case .relationship(let generation, let link, let side):
                guard (object["relationship_generation"] as? String).flatMap(AtlasReaderWire.integer) == generation,
                      (object["link_id"] as? String).flatMap(AtlasReaderWire.integer) == link.id,
                      object["side"] as? String == side.rawValue,
                      object["evidence_precision"] as? String == AtlasSavedWire.evidencePrecision,
                      object["fragment_resolution"] as? String == link.fragmentResolution.rawValue,
                      AtlasSavedFragmentRange.matches(object["target_range"], expected: link.targetRange),
                      (object["offset"] as? String).flatMap(AtlasReaderWire.integer) ==
                        (side == .target ? link.targetRange?.start ?? 0 : 0),
                      (object["member_bytes_read"] as? String).flatMap(AtlasReaderWire.integer) == member.observedBytes,
                      let callsText = object["member_read_calls"] as? String, let calls = AtlasReaderWire.integer(callsText),
                      (member.observedBytes == 0 && calls == 0) || (member.observedBytes > 0 && calls > 0 && calls <= 1_048_576) else {
                    throw AtlasSavedError.invalidResponse
                }
                if let slug = link.fragmentSlug {
                    guard let accepted = object["fragment_slug"] as? String,
                          accepted.utf8.elementsEqual(slug.utf8) else { throw AtlasSavedError.invalidResponse }
                } else if !(object["fragment_slug"] is NSNull) { throw AtlasSavedError.invalidResponse }
                if side == .evidence {
                    guard member.id == link.sourceMember, member.path.bytes == link.sourcePath.bytes,
                          member.digest == link.sourceDigest, member.observedBytes == link.evidenceEnd,
                          let selected = object["selection"] as? [String: Any],
                          selected["start"] as? String == "0",
                          (selected["end"] as? String).flatMap(AtlasReaderWire.integer) == member.observedBytes else {
                        throw AtlasSavedError.invalidResponse
                    }
                } else {
                    guard link.canOpenTarget, member.id == link.targetMember,
                          member.path.bytes == link.targetPath?.bytes,
                          link.targetRange == nil || link.targetRange!.end <= member.observedBytes,
                          AtlasSavedFragmentRange.matches(object["selection"], expected: link.targetRange) else {
                        throw AtlasSavedError.invalidResponse
                    }
                    if link.fragmentResolution == .resolved {
                        guard info.identity.encoding == "utf8", member.observedBytes <= AtlasSavedWire.documentBytes else {
                            throw AtlasSavedError.invalidResponse
                        }
                    } else if link.fragmentResolution == .resolvedLines {
                        guard let range = link.targetRange,
                              AtlasSourceLineFragment.admits(.init(start: range.start, end: range.end), in: info.identity) else {
                            throw AtlasSavedError.invalidResponse
                        }
                    } else if link.fragmentResolution == .missingLine {
                        guard AtlasSourceLineFragment.admitsIdentity(info.identity) else { throw AtlasSavedError.invalidResponse }
                    }
                }
                request = .window(offset: side == .target ? link.targetRange?.contextStart ?? 0 : 0, bytes: 16 * 1024)
                match = nil
            case .importLink(let generation, let link, let side):
                try AtlasSavedImportWire.validateContract(data)
                struct ImportReceipt: Decodable {
                    let import_generation, link_id, reader_owner, source_bytes, offset: AtlasSavedNumber
                    let member_bytes_read, member_read_calls: AtlasSavedNumber
                    let side: String
                    let path: AtlasSavedPath
                    let relationship: AtlasSavedImport
                }
                let accepted = try JSONDecoder().decode(ImportReceipt.self, from: data)
                guard generation > 0, accepted.import_generation.value == generation, accepted.link_id.value == link.id,
                      accepted.side == side.rawValue, accepted.reader_owner.value == destination,
                      accepted.path.bytes == member.path.bytes, accepted.source_bytes.value == member.observedBytes,
                      accepted.relationship.matches(link), link.matches(member, side: side),
                      accepted.offset.value == (side == .evidence ? link.evidence.start : 0),
                      accepted.member_bytes_read.value == member.observedBytes,
                      (member.observedBytes == 0 && accepted.member_read_calls.value == 0) ||
                        (member.observedBytes > 0 && accepted.member_read_calls.value > 0 &&
                            accepted.member_read_calls.value <= AtlasSavedImportWire.maximumMemberReadCalls) else {
                    throw AtlasSavedError.invalidResponse
                }
                if side == .evidence {
                    guard ["utf8", "utf16le", "utf16be"].contains(info.identity.encoding),
                          let selected = object["selection"] as? [String: Any] else {
                        throw AtlasSavedError.invalidResponse
                    }
                    if info.identity.encoding != "utf8" {
                        guard member.observedBytes % 2 == 0, link.evidence.start % 2 == 0, link.evidence.end % 2 == 0,
                              link.specifier.start % 2 == 0, link.specifier.end % 2 == 0 else { throw AtlasSavedError.invalidResponse }
                    }
                    let range = try JSONDecoder().decode(AtlasSavedImportRange.self,
                        from: JSONSerialization.data(withJSONObject: selected))
                    guard range == link.evidence else { throw AtlasSavedError.invalidResponse }
                } else if !(object["selection"] is NSNull) { throw AtlasSavedError.invalidResponse }
                request = .window(offset: side == .evidence ? link.evidence.contextStart : 0, bytes: 16 * 1024)
                match = nil
            case .structureHit(let generation, let mode, let nameMode, let needle, let item, let policy):
                let hit = item.hit
                try item.validate(mode: mode, needle: needle)
                struct Receipt: Decodable {
                    let case_sensitive, count_all_matches: Bool?
                    let name_case: String?
                }
                let receipt = try JSONDecoder().decode(Receipt.self, from: data)
                guard try AtlasSavedStructurePolicy.admit(caseSensitive: receipt.case_sensitive,
                        countAll: receipt.count_all_matches, nameCase: receipt.name_case, mode: mode) == policy,
                      generation > 0, AtlasSavedSearchWire.validNeedle(needle),
                      mode != .occurrences || nameMode == .exact,
                      object["selection_namespace"] as? String == "saved-structure",
                      (object["structure_generation"] as? String).flatMap(AtlasReaderWire.integer) == generation,
                      object["requested_mode"] as? String == mode.rawValue,
                      object["name_mode"] as? String == nameMode.rawValue,
                      let acceptedNeedle = object["needle"] as? String, acceptedNeedle.utf8.elementsEqual(needle.utf8),
                      member.id == hit.member, member.file == hit.file, member.file == hit.revision,
                      member.path.bytes == hit.path.bytes, member.digest == hit.digest,
                      hit.start < hit.end, hit.end <= member.observedBytes,
                      item.structure.evidenceEnd <= member.observedBytes,
                      (object["reader_owner"] as? String).flatMap(AtlasReaderWire.integer) == destination,
                      let witness = object["structure"] as? [String: Any],
                      let selected = object["selection"] as? [String: Any],
                      (selected["structure_generation"] as? String).flatMap(AtlasReaderWire.integer) == generation,
                      (selected["hit_id"] as? String).flatMap(AtlasReaderWire.integer) == hit.id,
                      selected["domain"] as? String == "original-source-bytes",
                      let range = selected["original_range"] as? [String: Any],
                      (range["start"] as? String).flatMap(AtlasReaderWire.integer) == hit.start,
                      (range["end"] as? String).flatMap(AtlasReaderWire.integer) == hit.end,
                      info.identity.encoding == "utf8" || (hit.start % 2 == 0 && hit.end % 2 == 0) else {
                    throw AtlasSavedError.invalidResponse
                }
                let acceptedWitness = try JSONDecoder().decode(AtlasSavedStructureWitness.self,
                    from: JSONSerialization.data(withJSONObject: witness)).value
                guard acceptedWitness == item.structure else { throw AtlasSavedError.invalidResponse }
                let contextStart = hit.start > 132 ? hit.start - 132 : 0
                if hit.end - hit.start <= 2048 {
                    guard let originalHex = selected["original_hex"] as? String,
                          AtlasReaderWire.validHex(originalHex, bytes: hit.end - hit.start) else {
                        throw AtlasSavedError.invalidResponse
                    }
                    if mode == .occurrences {
                        let literal = try AtlasSavedMatchEvidence.literalHex(needle, encoding: info.identity.encoding)
                        guard literal.utf8.elementsEqual(originalHex.utf8) else { throw AtlasSavedError.invalidResponse }
                    }
                    let evidence = AtlasSavedMatchEvidence(identity: info.identity, start: hit.start, end: hit.end,
                        originalHex: originalHex)
                    request = .window(offset: contextStart, bytes: max(4, min(member.observedBytes, hit.end + 132) - contextStart))
                    match = evidence
                } else {
                    guard selected["original_hex"] is NSNull else { throw AtlasSavedError.invalidResponse }
                    request = .window(offset: contextStart, bytes: 16 * 1024); match = nil
                }
            case .hit(let generation, let needle, let hit, _), .expressionHit(let generation, _, let needle, let hit, _):
                guard generation > 0, AtlasSavedSearchWire.validNeedle(needle),
                      member.id == hit.member, member.file == hit.file, member.file == hit.revision,
                      member.path.bytes == hit.path.bytes, member.digest == hit.digest,
                      hit.start < hit.end, hit.end <= member.observedBytes,
                      hit.end - hit.start <= (caseSensitive ? 2048 : 12 * UInt64(AtlasSavedSearchWire.maximumNeedleBytes)),
                      (object["reader_owner"] as? String).flatMap(AtlasReaderWire.integer) == destination,
                      let selected = object["selection"] as? [String: Any],
                      (selected[selectionGeneration] as? String).flatMap(AtlasReaderWire.integer) == generation,
                      (selected["hit_id"] as? String).flatMap(AtlasReaderWire.integer) == hit.id,
                      selected["domain"] as? String == "original-source-bytes",
                      let range = selected["original_range"] as? [String: Any],
                      (range["start"] as? String).flatMap(AtlasReaderWire.integer) == hit.start,
                      (range["end"] as? String).flatMap(AtlasReaderWire.integer) == hit.end,
                      let originalHex = selected["original_hex"] as? String,
                      AtlasReaderWire.validHex(originalHex, bytes: hit.end - hit.start) else {
                    throw AtlasSavedError.invalidResponse
                }
                let literal: String
                let expectedMode: String
                if case .expressionHit = origin {
                    expectedMode = caseSensitive ? "decoded-text-expression" : "unicode-lowercase-expression"
                } else {
                    expectedMode = caseSensitive ? "exact-decoded-literal" : "unicode-lowercase-literal"
                }
                if selected["case_sensitive"] != nil || selected["mode"] != nil || selected["text"] != nil
                    || selected["normalization"] != nil {
                    struct MatchingSelection: Decodable {
                        let case_sensitive: Bool
                        let mode, text: String
                        let normalization: String?
                    }
                    let accepted = try JSONDecoder().decode(MatchingSelection.self,
                        from: JSONSerialization.data(withJSONObject: selected))
                    guard accepted.case_sensitive == caseSensitive,
                          accepted.mode == expectedMode,
                          !caseSensitive || accepted.text.utf8.elementsEqual(needle.utf8) else {
                        throw AtlasSavedError.invalidResponse
                    }
                    if case .expressionHit = origin {
                        guard accepted.normalization == "exact" else { throw AtlasSavedError.invalidResponse }
                    }
                    literal = try AtlasSavedMatchEvidence.sourceHex(accepted.text, encoding: info.identity.encoding)
                } else {
                    // The legacy exact ABI did not return additive policy
                    // fields. Lowercase activation always requires them.
                    guard caseSensitive else { throw AtlasSavedError.invalidResponse }
                    literal = try AtlasSavedMatchEvidence.literalHex(needle, encoding: info.identity.encoding)
                }
                guard literal.utf8.elementsEqual(originalHex.utf8),
                      info.identity.encoding == "utf8" || (hit.start % 2 == 0 && hit.end % 2 == 0) else {
                    throw AtlasSavedError.invalidResponse
                }
                let evidence = AtlasSavedMatchEvidence(identity: info.identity, start: hit.start, end: hit.end, originalHex: originalHex)
                let contextEnd = min(member.observedBytes, hit.end + 132)
                request = .window(offset: evidence.contextStart, bytes: max(4, contextEnd - evidence.contextStart))
                match = evidence
            }
            let page = try AtlasReaderPage.decode(nested(windowObject), info: info, request: request)
            try match?.validate(page)
            return Self(readerJSON: readerJSON, window: page, match: match)
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }
    func validateInitialPage(_ page: AtlasReaderPage?) throws {
        guard let page, page.identity.matches(window.identity), page.start == window.start else {
            throw AtlasSavedError.invalidResponse
        }
        let bytes = min(page.end, window.end) - page.start
        guard page.originalHex.utf8.prefix(Int(bytes) * 2).elementsEqual(window.originalHex.utf8.prefix(Int(bytes) * 2)) else {
            throw AtlasSavedError.invalidResponse
        }
        try match?.validate(page)
    }
}

/// This capability identifies the captured supplemental source, while the
/// primary identity authorizes only navigation within the accepted trail.
/// It never supplies an archive-local document/image or authoring capability.
struct AtlasSavedTrailArchiveSource: Sendable {
    let primary: AtlasSavedIdentity
    let archiveDigest: String
    let path: String
    let sourceFile, sourceRevision: UInt64
}

struct AtlasSavedTrailArchiveReaderReceipt: Sendable {
    let member: AtlasSavedMember
    let reader: AtlasSavedReaderReceipt
    let source: AtlasSavedTrailArchiveSource
    static func decode(_ json: String?, request: AtlasSavedTrailArchiveReaderRequest, destination: UInt64) throws -> Self {
        guard let json, !json.isEmpty, json.utf8.count <= 1024 * 1024 else { throw AtlasSavedError.unavailable }
        let primary = request.trail.identity, entry = request.entry
        let data = try AtlasSavedWire.admit(json, handle: primary.owner, command: "trail-archive-open-reader", identity: primary)
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            let member = w.resolved_member, source = w.source_archive, validation = w.archive_validation
            guard destination > 0, destination != primary.owner,
                  w.trail_generation.value == request.trail.generation, w.trail_digest == request.trail.digest,
                  w.item.value == entry.id, w.entry.matches(entry), w.reader_owner.value == destination,
                  w.member.value == member.id, w.file_id.value > primary.members,
                  w.source_revision.value == w.file_id.value,
                  member.captured, member.id < source.members_total.value,
                  member.path.bytes == entry.path.bytes, member.digest == entry.sourceDigest,
                  member.observedBytes == entry.sourceBytes, w.source_digest == entry.sourceDigest,
                  w.path.bytes == entry.path.bytes, w.source_bytes.value == entry.sourceBytes,
                  w.selection == entry.range, w.offset.value == entry.range.start,
                  source.digest == entry.archiveDigest, source.path.bytes == Array(request.path.utf8),
                  (1...65_536).contains(source.members_total.value),
                  source.captured_members.value > 0, source.captured_members.value <= source.members_total.value,
                  w.source_observation == "verified-saved-member", w.archive_scope == "explicit-supplemental-archive",
                  !w.archive_path_reopened, !w.live_source_reopened, w.accepted_trail_unchanged, w.accepted_query_unchanged,
                  w.primary_member_bytes_read.value == 0, w.primary_member_read_calls.value == 0,
                  w.member_bytes_read.value == entry.sourceBytes,
                  (entry.sourceBytes == 0 && w.member_read_calls.value == 0) ||
                    (entry.sourceBytes > 0 && w.member_read_calls.value > 0 && w.member_read_calls.value <= 1_048_576),
                  validation.routing == "verified-archive-digest", validation.duplicate_policy == "first-verified-handle",
                  validation.validation_scope == "explicit-supplemental-archive-envelopes",
                  validation.validation_admission == "observed-file-length-plus-one-eof-probe",
                  validation.supplied_archives.value == 2, validation.validated_archives.value == 2,
                  validation.distinct_archives.value == 2, validation.duplicate_archives.value == 0,
                  validation.opened_archive_paths.value == 1, validation.validation_complete,
                  validation.max_archives.value == 8, validation.max_directory_bytes.value == 64 * 1024 * 1024,
                  validation.max_validation_bytes.value == request.validationBytes,
                  validation.validation_bytes_read.value > 0,
                  validation.validation_bytes_read.value <= validation.validation_bytes_admitted.value,
                  validation.validation_bytes_admitted.value <= request.validationBytes,
                  validation.validation_read_calls.value > 0,
                  validation.retained_directory_bytes.value <= validation.peak_directory_bytes.value,
                  validation.peak_directory_bytes.value <= validation.max_directory_bytes.value,
                  let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let readerObject = object["reader"] as? [String: Any],
                  let windowObject = object["window"] as? [String: Any] else { throw AtlasSavedError.invalidResponse }
            func nested(_ object: [String: Any]) throws -> String {
                String(decoding: try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]), as: UTF8.self)
            }
            let readerJSON = try nested(readerObject)
            let info = try AtlasReaderInfo.decodeRetainedInitialization(readerJSON, handle: destination, pathBytes: entry.path.bytes)
            guard info.identity.capturedBytes == entry.sourceBytes, info.captureSHA256 == entry.sourceDigest else {
                throw AtlasSavedError.invalidResponse
            }
            let window = try AtlasReaderPage.decode(nested(windowObject), info: info,
                request: .window(offset: entry.range.contextStart, bytes: 16 * 1024))
            return Self(member: member, reader: AtlasSavedReaderReceipt(readerJSON: readerJSON, window: window, match: nil),
                source: AtlasSavedTrailArchiveSource(primary: primary, archiveDigest: source.digest, path: request.path,
                    sourceFile: w.file_id.value, sourceRevision: w.source_revision.value))
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }
    private struct SourceArchive: Decodable {
        let path: AtlasSavedPath
        let digest: String
        let members_total, captured_members: AtlasSavedNumber
        let discovery_complete: Bool
    }
    private struct Validation: Decodable {
        let routing, duplicate_policy, validation_scope, validation_admission: String
        let supplied_archives, validated_archives, distinct_archives, duplicate_archives, opened_archive_paths: AtlasSavedNumber
        let max_archives, max_directory_bytes, max_validation_bytes: AtlasSavedNumber
        let validation_bytes_read, validation_bytes_admitted, validation_read_calls: AtlasSavedNumber
        let retained_directory_bytes, peak_directory_bytes: AtlasSavedNumber
        let validation_complete: Bool
    }
    private struct Wire: Decodable {
        let trail_generation, item, reader_owner, member, file_id, source_revision, source_bytes, offset: AtlasSavedNumber
        let member_bytes_read, member_read_calls, primary_member_bytes_read, primary_member_read_calls: AtlasSavedNumber
        let trail_digest, source_digest, source_observation, archive_scope: String
        let path: AtlasSavedPath
        let entry: AtlasSavedTrailEntry
        let selection: AtlasSavedTrailRange
        let resolved_member: AtlasSavedMember
        let source_archive: SourceArchive
        let archive_validation: Validation
        let archive_path_reopened, live_source_reopened, accepted_trail_unchanged, accepted_query_unchanged: Bool
    }
}

final class AtlasSavedTrailArchiveReaderReceiptBox: @unchecked Sendable {
    private let lock = NSLock()
    private var value: AtlasSavedTrailArchiveReaderReceipt?
    func set(_ receipt: AtlasSavedTrailArchiveReaderReceipt) throws {
        lock.lock(); defer { lock.unlock() }
        guard value == nil else { throw AtlasSavedError.invalidResponse }
        value = receipt
    }
    func accepted() throws -> AtlasSavedTrailArchiveReaderReceipt {
        lock.lock(); defer { lock.unlock() }
        guard let value else { throw AtlasSavedError.invalidResponse }
        return value
    }
}

/// The initializer runs on the reader worker. Its bounded receipt crosses back
/// under this lock; no worker mutates observable native state.
final class AtlasSavedReaderReceiptBox: @unchecked Sendable {
    private let lock = NSLock()
    private var value: AtlasSavedReaderReceipt?
    func set(_ receipt: AtlasSavedReaderReceipt) throws {
        lock.lock(); defer { lock.unlock() }
        guard value == nil else { throw AtlasSavedError.invalidResponse }
        value = receipt
    }
    func accepted() throws -> AtlasSavedReaderReceipt {
        lock.lock(); defer { lock.unlock() }
        guard let value else { throw AtlasSavedError.invalidResponse }
        return value
    }
}

@MainActor final class AtlasSavedReaderCapture: Identifiable {
    let id = UUID()
    let reader: AtlasPagedReaderModel
    let owner: UInt64
    /// Membership authority for onward links, not a live filesystem grant.
    /// The independent source reader remains usable when this archive closes.
    let archiveIdentity: AtlasSavedIdentity?
    let supplementalTrailSource: AtlasSavedTrailArchiveSource?
    var trailNavigationIdentity: AtlasSavedIdentity? {
        guard case .trail = origin else { return nil }
        return supplementalTrailSource?.primary ?? archiveIdentity
    }
    let member: AtlasSavedMember
    let origin: AtlasSavedReaderOrigin
    let match: AtlasSavedMatchEvidence?
    private let trailIdentity: AtlasReaderIdentity?
    init(reader: AtlasPagedReaderModel, owner: UInt64, member: AtlasSavedMember, origin: AtlasSavedReaderOrigin,
        match: AtlasSavedMatchEvidence? = nil, archiveIdentity: AtlasSavedIdentity? = nil,
        supplementalTrailSource: AtlasSavedTrailArchiveSource? = nil) {
        self.reader = reader; self.owner = owner; self.member = member; self.origin = origin; self.match = match
        self.archiveIdentity = supplementalTrailSource == nil ? archiveIdentity : nil
        self.supplementalTrailSource = supplementalTrailSource
        if case .trail = origin { trailIdentity = reader.page?.identity } else { trailIdentity = nil }
    }
    var evidenceLabel: String {
        switch origin {
        case .member: return "Exact saved member · \(member.observedBytes) original bytes"
        case .trail(_, _, let entry):
            return "Reading trail item \(entry.id + 1) · \(entry.range.label) · explicit user selection"
        case .pathHit(_, let hit):
            return "Saved filename result · \(hit.matchKind) · \(member.observedBytes) exact captured bytes"
        case .relationship(_, _, .evidence):
            return "Captured document evidence: original bytes 0..<\(member.observedBytes). Individual link positions are not established."
        case .relationship(_, let link, .target):
            return link.targetNavigationDescription
        case .importLink(_, let link, .evidence):
            return "\(link.label) · exact statement bytes \(link.evidence.start)..<\(link.evidence.end) · specifier bytes \(link.specifier.start)..<\(link.specifier.end)"
        case .importLink(_, let link, .target):
            if link.language == "java" {
                return "Saved Java source candidate for \(link.destination) · imported declaration location, classpath and compiler binding are not established"
            }
            if link.resolutionBasis == "rust-use-module-prefix-candidate" {
                return "Saved containing-module candidate for \(link.destination) · imported item location and compiler resolution are not established"
            }
            return "Saved catalog candidate for \(link.destination) · \(link.resolutionBasis) · lexical relationship, not compiler resolution"
        case .hit(let generation, _, let hit, let caseSensitive):
            return "Saved search \(generation), occurrence \(hit.id) · \(caseSensitive ? "match case" : "Unicode lowercase matching") · original bytes \(hit.start)..<\(hit.end)"
        case .expressionHit(let generation, _, _, let hit, let caseSensitive):
            return "Saved expression \(generation), occurrence \(hit.id) · \(caseSensitive ? "match case" : "Unicode lowercase matching") · original bytes \(hit.start)..<\(hit.end)"
        case .structureHit(_, _, _, _, let item, _):
            return "\(item.structure.label) · original bytes \(item.hit.start)..<\(item.hit.end)"
        }
    }
    var canUseMatch: Bool {
        guard let match, reader.canPin, let page = reader.page else { return false }
        return page.identity.matches(match.identity)
    }
    var canUseTrailRange: Bool {
        guard case .trail(_, _, let entry) = origin,
              reader.canPin, let identity = trailIdentity, let page = reader.page else { return false }
        if let source = supplementalTrailSource {
            guard archiveIdentity == nil, source.primary.owner == owner, source.archiveDigest == entry.archiveDigest,
                  source.sourceFile > source.primary.members, source.sourceRevision == source.sourceFile,
                  entry.readiness == .otherArchive, member.captured, member.path.bytes == entry.path.bytes,
                  member.digest == entry.sourceDigest, member.observedBytes == entry.sourceBytes,
                  reader.retainedCaptureSHA256 == entry.sourceDigest else { return false }
        } else if !entry.matches(member) { return false }
        return page.identity.matches(identity) && identity.capturedBytes == entry.sourceBytes &&
            identity.pathBytes == entry.path.bytes
    }
    func withTrailReader<Value: Sendable>(_ work: @escaping @Sendable (UInt64) throws -> Value) async throws -> Value {
        guard canUseTrailRange else { throw AtlasSavedError.unavailable }
        try Task.checkCancellation()
        return try await reader.withRetainedReader(work)
    }
    /// Recheck the exact independent reader after any Desk startup wait. The
    /// caller keeps this occurrence's frozen original range, not whichever page
    /// the user happens to be viewing by the time the import begins.
    func withMatchReader<Value: Sendable>(_ work: @escaping @Sendable (UInt64) throws -> Value) async throws -> Value {
        guard canUseMatch else { throw AtlasSavedError.unavailable }
        try Task.checkCancellation()
        return try await reader.withRetainedReader(work)
    }
}


/// At most four independent captures retain the reading journey. Back/Forward
/// changes only the selected capture; its byte page, Find and preview survive.
/// Discarded forward entries and the oldest entry retire through reader.close(),
/// whose coordinator drains work and destroys native source off the UI thread.
@MainActor struct AtlasSavedReaderHistory {
    static let capacity = 4
    private(set) var entries: [AtlasSavedReaderCapture]
    private(set) var position = 0
    private(set) var closed = false
    init(_ first: AtlasSavedReaderCapture) { entries = [first] }
    var current: AtlasSavedReaderCapture { entries[position] }
    var canBack: Bool { !closed && position > 0 }
    var canForward: Bool { !closed && position + 1 < entries.count }
    mutating func back() { if canBack { position -= 1 } }
    mutating func forward() { if canForward { position += 1 } }
    @discardableResult mutating func accept(_ candidate: AtlasSavedReaderCapture) -> Bool {
        let sameArchive = current.archiveIdentity.map {
            candidate.archiveIdentity == $0 && candidate.owner == $0.owner
        } ?? false
        let sameTrail: Bool
        if let primary = current.trailNavigationIdentity, candidate.trailNavigationIdentity == primary,
           case .trail(let generation, let digest, _) = current.origin,
           case .trail(let nextGeneration, let nextDigest, _) = candidate.origin {
            sameTrail = candidate.owner == primary.owner && current.owner == primary.owner &&
                generation == nextGeneration && digest == nextDigest &&
                current.reader.retainedRegistryDomain != nil &&
                current.reader.retainedRegistryDomain == candidate.reader.retainedRegistryDomain
        } else { sameTrail = false }
        guard !closed, sameArchive || sameTrail,
              !entries.contains(where: { $0.id == candidate.id }) else { return false }
        for retired in entries.dropFirst(position + 1) { retired.reader.close() }
        entries.removeSubrange((position + 1)..<entries.count)
        if entries.count == Self.capacity {
            entries.removeFirst().reader.close()
        }
        entries.append(candidate); position = entries.count - 1
        return true
    }
    mutating func close() {
        guard !closed else { return }
        closed = true
        for entry in entries { entry.reader.close() }
    }
}
