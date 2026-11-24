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
    var export: (@Sendable (UInt64, AtlasSavedTrailExportRequest) -> String?)? = nil
    var exportReceipt: (@Sendable (UInt64, UInt64) -> String?)? = nil
    var exportWithArchives: (@Sendable (UInt64, AtlasSavedTrailExportRequest, AtlasSearchCancellation) -> String?)? = nil
    var openArchiveReader: (@Sendable (UInt64, UInt64, AtlasSavedTrailArchiveReaderRequest, AtlasSearchCancellation) -> String?)? = nil
    var edit: (@Sendable (UInt64, AtlasSavedTrailEditRequest, AtlasSearchCancellation) -> String?)? = nil
}

/// A single explicit archive grant for one frozen other-archive trail visit.
/// The selected file must prove the entry's archive digest before source I/O.
struct AtlasSavedTrailArchiveReaderRequest: Sendable {
    static let maximumValidationBytes: UInt64 = 256 * 1024 * 1024
    let trail: AtlasSavedTrailParent
    let entry: AtlasSavedTrailEntry
    let path: String
    let validationBytes: UInt64
    init(trail: AtlasSavedTrailParent, entry: AtlasSavedTrailEntry, path: String,
        validationBytes: UInt64 = Self.maximumValidationBytes) throws {
        guard trail.generation > 0, AtlasSavedWire.digest(trail.digest), entry.id < trail.items,
              entry.readiness == .otherArchive, AtlasSavedIndexBuildRequest.validDestination(path),
              validationBytes <= Self.maximumValidationBytes else { throw AtlasSavedError.invalidInput }
        try entry.validate(in: trail.identity)
        self.trail = trail; self.entry = entry; self.path = path; self.validationBytes = validationBytes
    }
}

struct AtlasSavedTrailExportLimits: Sendable, Equatable {
    var entries: UInt64 = 1024
    var verificationBytes: UInt64 = 64 * 1024 * 1024
    var selectedBytes: UInt64 = 1024 * 1024
    var outputBytes: UInt64 = 16 * 1024 * 1024
    var outputScalars: UInt64 = 16 * 1024 * 1024
    /// Supplemental envelopes are validated before any selected source reads.
    /// The primary archive retains its already validated descriptor.
    var archiveValidationBytes: UInt64 = 256 * 1024 * 1024
    var isValid: Bool {
        entries <= 1024 && verificationBytes <= 64 * 1024 * 1024 && selectedBytes <= 64 * 1024 * 1024 &&
            (1...16 * 1024 * 1024).contains(outputBytes) && (1...16 * 1024 * 1024).contains(outputScalars) &&
            archiveValidationBytes <= 256 * 1024 * 1024
    }
}

/// Frozen accepted trail identity; the save panel supplies destination access
/// separately. Display labels never reopen an archive or a source pathname.
struct AtlasSavedTrailExportRequest: Sendable {
    static let maximumAdditionalArchives = 7
    let trail: AtlasSavedTrailParent
    let operation: UInt64
    let destination: String
    let limits: AtlasSavedTrailExportLimits
    /// Only explicit native file selections authorize these paths. Trail
    /// metadata never supplies them, and byte spelling is preserved.
    let additionalArchives: [String]
    var identity: AtlasSavedIdentity { trail.identity }
    init(trail: AtlasSavedTrailParent, operation: UInt64, destination: String,
         limits: AtlasSavedTrailExportLimits, additionalArchives: [String] = []) throws {
        guard operation > 0, trail.generation > 0, AtlasSavedWire.digest(trail.digest),
              trail.items <= 1024, AtlasSavedIndexBuildRequest.validDestination(destination), limits.isValid,
              additionalArchives.count <= Self.maximumAdditionalArchives,
              additionalArchives.allSatisfy({ AtlasSavedIndexBuildRequest.validDestination($0) &&
                  !$0.utf8.elementsEqual(destination.utf8) }) else {
            throw AtlasSavedError.invalidInput
        }
        self.trail = trail; self.operation = operation; self.destination = destination; self.limits = limits
        self.additionalArchives = additionalArchives
    }
}

struct AtlasSavedTrailArchiveValidation: Sendable {
    let supplied, validated, distinct, duplicates, openedPaths, bytesRead: UInt64
    let complete: Bool
}

struct AtlasSavedTrailExportReceipt: Sendable {
    let identity: AtlasSavedIdentity
    let operation: UInt64
    let effect: AtlasSavedIndexBuildEffect
    let succeeded, completeArtifact, coverageComplete: Bool
    let exportBytes, exportScalars, bytesWritten, verified, unavailable, omitted, selectedBytes: UInt64
    let digest, errorCode: String?
    let archiveValidation: AtlasSavedTrailArchiveValidation?
    var message: String {
        if succeeded {
            let archives = archiveValidation.map {
                "Validated \($0.validated) archives (\($0.distinct) distinct) before selecting sources. "
            } ?? ""
            return archives + "Created a source pack: \(verified) verified selections, \(unavailable) unavailable and \(omitted) omitted. \(exportBytes) UTF-8 JSON bytes; \(exportScalars) Unicode scalars. " +
                (coverageComplete ? "Every saved selection is included." : "Coverage is partial; all admitted notes and omission reasons remain in the pack.")
        }
        let state: String
        switch effect {
        case .none: state = "No source-pack destination was created."
        case .incomplete: state = "The destination contains an incomplete source pack."
        case .fileSyncUnconfirmed: state = "Complete source-pack bytes were written; file synchronization is unconfirmed."
        case .fileSyncRequested: state = "Complete source-pack bytes were written and file synchronization was requested."
        case .fileAndParentSyncRequested: state = "The complete source pack and parent directory were synchronized."
        }
        return "\(state) \(errorCode ?? "Unconfirmed export"). Inspect the destination before choosing a new filename."
    }
    static func decode(_ json: String?, request: AtlasSavedTrailExportRequest) throws -> Self {
        let maximumReceiptBytes = request.additionalArchives.isEmpty ? 256 * 1024 : 2 * 1024 * 1024
        guard let json, !json.isEmpty, json.utf8.count <= maximumReceiptBytes else { throw AtlasSavedError.unavailable }
        do {
            let data = Data(json.utf8), w = try JSONDecoder().decode(Wire.self, from: data)
            let identity = try JSONDecoder().decode(AtlasSavedIdentity.self, from: data)
            let b = w.budgets, c = w.coverage, l = request.limits
            guard identity == request.identity, w.schema == "fcb.saved-repository/1", w.command == "trail-export",
                  w.status == "ok" || w.status == "error", w.operation.value == request.operation,
                  w.last_trail_export_attempt.value == request.operation,
                  w.trail_generation.value == request.trail.generation, w.trail_digest == request.trail.digest,
                  w.destination.bytes == Array(request.destination.utf8), w.pack_schema == "fcb.trail-source-pack/1",
                  w.character_representation == "unicode-scalars-in-serialized-utf8-json",
                  w.archive_scope == (request.additionalArchives.isEmpty ? "retained-single-archive" :
                    "retained-primary-and-explicit-supplemental-archives"),
                  b.entries.value == l.entries, b.verification_source_bytes.value == l.verificationBytes,
                  b.selected_original_bytes.value == l.selectedBytes, b.export_bytes.value == l.outputBytes,
                  b.export_characters.value == l.outputScalars,
                  !w.archive_path_reopened, !w.live_source_reopened, w.accepted_trail_unchanged, w.accepted_query_unchanged,
                  w.source_derived_sensitive, !w.redacted, !w.encrypted, !w.overwritten, !w.power_loss_qualified,
                  c.total_entries.value == request.trail.items, c.entries_visited.value <= min(l.entries, request.trail.items),
                  c.verification_bytes_admitted.value <= l.verificationBytes, c.source_bytes_read.value <= c.verification_bytes_admitted.value,
                  c.selected_original_bytes.value <= l.selectedBytes,
                  c.selected_original_bytes.value <= c.source_bytes_read.value,
                  c.selected_original_bytes.value <= request.trail.referenceBytes,
                  w.member_bytes_read.value == c.source_bytes_read.value,
                  w.member_read_calls.value <= 1_048_576 * 1024,
                  w.export_bytes.value <= l.outputBytes, w.export_characters.value <= l.outputScalars,
                  w.export_characters.value <= w.export_bytes.value, w.bytes_written.value <= w.export_bytes.value else {
                throw AtlasSavedError.invalidResponse
            }
            let validation = try validateArchives(w, request: request)
            let verified = c.verified_entries.value, unavailable = c.unavailable_entries.value
            let sourceOmitted = c.omitted_source_limit.value, outputOmitted = c.omitted_output_limit.value
            let tailOmitted = c.omitted_entry_limit.value
            guard [verified, unavailable, sourceOmitted, outputOmitted, tailOmitted].allSatisfy({ $0 <= request.trail.items }),
                  verified + unavailable + sourceOmitted + outputOmitted <= c.entries_visited.value,
                  tailOmitted <= request.trail.items - c.entries_visited.value else { throw AtlasSavedError.invalidResponse }
            let built = w.export_bytes.value > 0, succeeded = w.status == "ok", error = w.error.value
            if validation?.complete == false {
                guard !built, !succeeded, w.effect == .none, c.entries_visited.value == 0,
                      c.verification_bytes_admitted.value == 0, c.source_bytes_read.value == 0,
                      w.member_read_calls.value == 0 else { throw AtlasSavedError.invalidResponse }
            }
            if built {
                guard let digest = w.export_digest.value, AtlasSavedWire.digest(digest),
                      w.export_characters.value > 0,
                      verified + unavailable + sourceOmitted + outputOmitted == c.entries_visited.value,
                      c.entries_visited.value + tailOmitted == request.trail.items else { throw AtlasSavedError.invalidResponse }
            } else {
                guard w.export_digest.value == nil, w.export_characters.value == 0 else { throw AtlasSavedError.invalidResponse }
            }
            guard succeeded == (error == nil), !succeeded || (built &&
                    (w.effect == .fileSyncRequested || w.effect == .fileAndParentSyncRequested)),
                  w.effect != .none || w.bytes_written.value == 0,
                  w.effect == .none || built,
                  !w.effect.completeFile || w.bytes_written.value == w.export_bytes.value,
                  w.complete_artifact == (built && w.effect.completeFile),
                  w.coverage_complete == (built && verified == request.trail.items && unavailable == 0 &&
                    sourceOmitted == 0 && outputOmitted == 0 && tailOmitted == 0) else { throw AtlasSavedError.invalidResponse }
            if let error {
                guard !error.code.isEmpty, error.code.utf8.count <= 512,
                      error.code.unicodeScalars.allSatisfy({ $0.value >= 32 && $0.value != 127 }) else {
                    throw AtlasSavedError.invalidResponse
                }
            }
            return Self(identity: identity, operation: request.operation, effect: w.effect,
                succeeded: succeeded, completeArtifact: w.complete_artifact, coverageComplete: w.coverage_complete,
                exportBytes: w.export_bytes.value, exportScalars: w.export_characters.value, bytesWritten: w.bytes_written.value,
                verified: verified, unavailable: unavailable, omitted: sourceOmitted + outputOmitted + tailOmitted,
                selectedBytes: c.selected_original_bytes.value, digest: w.export_digest.value, errorCode: error?.code,
                archiveValidation: validation)
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }
    private static func validateArchives(_ w: Wire, request: AtlasSavedTrailExportRequest) throws -> AtlasSavedTrailArchiveValidation? {
        guard !request.additionalArchives.isEmpty else {
            guard w.archive_validation == nil, w.additional_archives == nil else { throw AtlasSavedError.invalidResponse }
            return nil
        }
        guard let v = w.archive_validation, let archives = w.additional_archives,
              archives.count == request.additionalArchives.count,
              v.routing == "verified-archive-digest", v.duplicate_policy == "first-verified-handle",
              v.validation_admission == "observed-file-length-plus-one-eof-probe",
              v.validation_scope == "explicit-supplemental-archive-envelopes",
              v.max_archives.value == UInt64(AtlasSavedTrailExportRequest.maximumAdditionalArchives + 1),
              v.max_validation_bytes.value == request.limits.archiveValidationBytes,
              v.max_directory_bytes.value == 64 * 1024 * 1024,
              v.validation_bytes_read.value <= v.validation_bytes_admitted.value,
              v.validation_bytes_admitted.value <= v.max_validation_bytes.value,
              v.retained_directory_bytes.value <= v.peak_directory_bytes.value,
              v.peak_directory_bytes.value <= v.max_directory_bytes.value else { throw AtlasSavedError.invalidResponse }
        var digests: Set<String> = [request.identity.digest]
        var validated: UInt64 = 1, opened: UInt64 = 0
        for (index, archive) in archives.enumerated() {
            guard archive.ordinal.value == UInt64(index),
                  archive.path.bytes == Array(request.additionalArchives[index].utf8),
                  archive.opened || archive.digest.value == nil else { throw AtlasSavedError.invalidResponse }
            if archive.opened { opened += 1 }
            if let digest = archive.digest.value {
                guard AtlasSavedWire.digest(digest) else { throw AtlasSavedError.invalidResponse }
                validated += 1; digests.insert(digest)
            }
        }
        let supplied = UInt64(archives.count + 1), distinct = UInt64(digests.count)
        guard v.supplied_archives.value == supplied, v.validated_archives.value == validated,
              v.distinct_archives.value == distinct, v.duplicate_archives.value == validated - distinct,
              v.opened_archive_paths.value == opened, validated - 1 <= opened,
              v.validation_complete == (validated == supplied) else { throw AtlasSavedError.invalidResponse }
        return AtlasSavedTrailArchiveValidation(supplied: supplied, validated: validated, distinct: distinct,
            duplicates: validated - distinct, openedPaths: opened, bytesRead: v.validation_bytes_read.value,
            complete: v.validation_complete)
    }
    private struct AdditionalArchive: Decodable {
        let ordinal: AtlasSavedNumber
        let path: AtlasSavedPath
        let opened: Bool
        let digest: AtlasSavedSearchOptional<String>
    }
    private struct ArchiveValidation: Decodable {
        let routing, duplicate_policy, validation_admission, validation_scope: String
        let supplied_archives, validated_archives, distinct_archives, duplicate_archives: AtlasSavedNumber
        let opened_archive_paths, validation_bytes_admitted, validation_bytes_read, validation_read_calls: AtlasSavedNumber
        let retained_directory_bytes, peak_directory_bytes, max_archives, max_validation_bytes, max_directory_bytes: AtlasSavedNumber
        let validation_complete: Bool
    }
    private struct Failure: Decodable { let code: String }
    private struct Budgets: Decodable {
        let entries, verification_source_bytes, selected_original_bytes, export_bytes, export_characters: AtlasSavedNumber
    }
    private struct Coverage: Decodable {
        let total_entries, entries_visited, verified_entries, unavailable_entries: AtlasSavedNumber
        let omitted_entry_limit, omitted_source_limit, omitted_output_limit: AtlasSavedNumber
        let verification_bytes_admitted, source_bytes_read, selected_original_bytes: AtlasSavedNumber
    }
    private struct Wire: Decodable {
        let schema, command, status, trail_digest, pack_schema, character_representation, archive_scope: String
        let operation, last_trail_export_attempt, trail_generation: AtlasSavedNumber
        let destination: AtlasSavedPath
        let budgets: Budgets
        let coverage: Coverage
        let additional_archives: [AdditionalArchive]?
        let archive_validation: ArchiveValidation?
        let coverage_complete, complete_artifact: Bool
        let export_bytes, export_characters, bytes_written, member_bytes_read, member_read_calls: AtlasSavedNumber
        let export_digest: AtlasSavedSearchOptional<String>
        let error: AtlasSavedSearchOptional<Failure>
        let effect: AtlasSavedIndexBuildEffect
        let archive_path_reopened, live_source_reopened, accepted_trail_unchanged, accepted_query_unchanged: Bool
        let source_derived_sensitive, redacted, encrypted, overwritten, power_loss_qualified: Bool
    }
}
