import Foundation
import Observation

/// Frozen disclosure authority for one exact archived selection. Reader IDs
/// qualify the originating capture; archived member IDs are a separate domain.
struct AtlasSavedTrailSelection: Sendable {
    let identity: AtlasSavedIdentity
    let member: AtlasSavedMember
    let readerIdentity: AtlasReaderIdentity
    let range: AtlasSavedTrailRange
    init(identity: AtlasSavedIdentity, member: AtlasSavedMember,
        readerIdentity: AtlasReaderIdentity, start: UInt64, end: UInt64) throws {
        guard member.captured, member.id < identity.members, let digest = member.digest, AtlasSavedWire.digest(digest),
              readerIdentity.owner != identity.owner, readerIdentity.pathBytes == member.path.bytes,
              readerIdentity.capturedBytes == member.observedBytes, start <= end, end <= member.observedBytes else {
            throw AtlasSavedError.invalidInput
        }
        self.identity = identity; self.member = member; self.readerIdentity = readerIdentity
        range = try AtlasSavedTrailRange(start: start, end: end)
    }
}

struct AtlasSavedTrailParent: Sendable {
    let identity: AtlasSavedIdentity
    let generation: UInt64
    let digest, title: String
    let items, referenceBytes: UInt64
    init(_ page: AtlasSavedTrailPage) {
        identity = page.identity; generation = page.generation; digest = page.digest
        title = page.title; items = page.total; referenceBytes = page.referenceBytes
    }
    func matches(_ page: AtlasSavedTrailPage) -> Bool {
        identity == page.identity && generation == page.generation && digest == page.digest &&
            title.utf8.elementsEqual(page.title.utf8) && items == page.total && referenceBytes == page.referenceBytes
    }
}

struct AtlasSavedTrailWriteRequest: Sendable {
    let selection: AtlasSavedTrailSelection
    let operation: UInt64
    let parent: AtlasSavedTrailParent?
    let title, rationale, destination: String
    var identity: AtlasSavedIdentity { selection.identity }
    var totalItems: UInt64 { (parent?.items ?? 0) + 1 }
    var referenceBytes: UInt64 { (parent?.referenceBytes ?? 0) + (selection.range.end - selection.range.start) }
    init(selection: AtlasSavedTrailSelection, operation: UInt64, parent: AtlasSavedTrailParent?,
        title: String, rationale: String, destination: String) throws {
        let (_, overflow) = (parent?.referenceBytes ?? 0)
            .addingReportingOverflow(selection.range.end - selection.range.start)
        guard operation > 0, AtlasSavedIndexBuildRequest.validDestination(destination),
              title.utf8.count <= 256, rationale.utf8.count <= 4096, !overflow else { throw AtlasSavedError.invalidInput }
        if let parent {
            guard parent.identity == selection.identity, parent.generation > 0, AtlasSavedWire.digest(parent.digest),
                  parent.items < 1024, title.utf8.elementsEqual(parent.title.utf8) else { throw AtlasSavedError.invalidInput }
        }
        self.selection = selection; self.operation = operation; self.parent = parent
        self.title = title; self.rationale = rationale; self.destination = destination
    }
}

/// The receipt describes the actual create-only write, including a canceled
/// operation which left a complete file. Reading it never repeats publication.
struct AtlasSavedTrailWriteReceipt: Sendable {
    let identity: AtlasSavedIdentity
    let operation: UInt64
    let destination: AtlasSavedPath
    let effect: AtlasSavedIndexBuildEffect
    let succeeded, sourceVerified, completeArtifact: Bool
    let trailBytes, bytesWritten, memberBytes, memberCalls: UInt64
    let digest: String?
    let totalItems, referenceBytes: UInt64?
    let errorCode, nextAction: String?

    var message: String {
        if succeeded { return "Saved a new reading trail. It contains exact source references and rationale. Open that file explicitly to follow or extend it." }
        let code = errorCode ?? "unknown outcome"
        switch effect {
        case .none: return "No new trail file was created (\(code)). Your draft remains available; choose a new filename for an explicit retry."
        case .incomplete: return "An incomplete trail remains at the selected destination (\(code)). Inspect it before retrying and choose a different new filename."
        case .fileSyncUnconfirmed: return "Complete trail bytes were written, but file synchronization was not confirmed (\(code)). The destination can be inspected explicitly."
        case .fileSyncRequested: return "Complete trail bytes were written and file synchronization was requested (\(code)). The destination can be inspected explicitly."
        case .fileAndParentSyncRequested: return "The completed trail and its parent directory were synchronized, but the operation reported \(code). The destination can be inspected explicitly."
        }
    }

    static func decode(_ json: String?, request: AtlasSavedTrailWriteRequest) throws -> Self {
        guard let json, !json.isEmpty, json.utf8.count <= 256 * 1024 else { throw AtlasSavedError.unavailable }
        do {
            let data = Data(json.utf8)
            let w = try JSONDecoder().decode(Wire.self, from: data)
            let identity = try JSONDecoder().decode(AtlasSavedIdentity.self, from: data)
            let selection = request.selection, member = selection.member
            guard identity == request.identity, w.schema == "fcb.saved-repository/1", w.command == "trail-write",
                  w.status == "ok" || w.status == "error", w.operation.value == request.operation,
                  w.last_trail_write_attempt.value == request.operation,
                  w.destination.bytes == Array(request.destination.utf8),
                  w.mode == (request.parent == nil ? "new" : "append"),
                  w.parent_generation.value?.value == request.parent?.generation,
                  w.parent_digest.value == request.parent?.digest, w.parent_items.value == (request.parent?.items ?? 0),
                  w.member.value == member.id, w.file_id.value == member.file, w.source_revision.value == member.file,
                  w.path.bytes == member.path.bytes, w.source_digest == member.digest,
                  w.source_bytes.value == member.observedBytes, w.selection == selection.range,
                  w.title.utf8.elementsEqual(request.title.utf8), w.rationale.utf8.elementsEqual(request.rationale.utf8),
                  !w.archive_path_reopened, !w.live_source_reopened, !w.source_payload_embedded,
                  !w.trail_attached, w.accepted_trail_unchanged, w.accepted_query_unchanged,
                  w.source_derived_sensitive, !w.encrypted, !w.overwritten, !w.power_loss_qualified, !w.truncated,
                  w.trail_bytes.value <= 4 * 1024 * 1024, w.bytes_written.value <= w.trail_bytes.value,
                  w.member_bytes_read.value <= member.observedBytes, w.member_read_calls.value <= 1_048_576 else {
                throw AtlasSavedError.invalidResponse
            }
            let succeeded = w.status == "ok", error = w.error.value
            guard succeeded == (error == nil), !succeeded ||
                    w.effect == .fileSyncRequested || w.effect == .fileAndParentSyncRequested else {
                throw AtlasSavedError.invalidResponse
            }
            if let error {
                guard !error.code.isEmpty, error.code.utf8.count <= 512,
                      error.code.unicodeScalars.allSatisfy({ $0.value >= 32 && $0.value != 127 }),
                      !error.next_action.isEmpty, error.next_action.utf8.count <= 4096,
                      error.next_action.unicodeScalars.allSatisfy({ $0.value >= 32 && $0.value != 127 }) else {
                    throw AtlasSavedError.invalidResponse
                }
            }
            if w.source_verified {
                guard w.member_bytes_read.value == member.observedBytes,
                      (member.observedBytes == 0 && w.member_read_calls.value == 0) ||
                        (member.observedBytes > 0 && w.member_read_calls.value > 0) else { throw AtlasSavedError.invalidResponse }
            }
            let digest = w.trail_digest.value, items = w.total_items.value?.value, references = w.reference_bytes.value?.value
            if w.trail_bytes.value == 0 {
                guard digest == nil, items == nil, references == nil else { throw AtlasSavedError.invalidResponse }
            } else {
                guard w.source_verified, let digest, AtlasSavedWire.digest(digest),
                      items == request.totalItems, references == request.referenceBytes else { throw AtlasSavedError.invalidResponse }
            }
            guard w.effect != .none || w.bytes_written.value == 0,
                  w.effect == .none || (w.trail_bytes.value > 0 && digest != nil),
                  !w.effect.completeFile || (w.trail_bytes.value > 0 && w.bytes_written.value == w.trail_bytes.value),
                  w.complete_artifact == w.effect.completeFile, w.attach_eligible == w.complete_artifact,
                  !succeeded || (w.source_verified && w.complete_artifact) else { throw AtlasSavedError.invalidResponse }
            return Self(identity: identity, operation: request.operation, destination: w.destination, effect: w.effect,
                succeeded: succeeded, sourceVerified: w.source_verified, completeArtifact: w.complete_artifact,
                trailBytes: w.trail_bytes.value, bytesWritten: w.bytes_written.value,
                memberBytes: w.member_bytes_read.value, memberCalls: w.member_read_calls.value,
                digest: digest, totalItems: items, referenceBytes: references, errorCode: error?.code, nextAction: error?.next_action)
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }
    private struct Failure: Decodable { let code, next_action: String }
    private struct Wire: Decodable {
        let schema, status, command, mode: String
        let operation, last_trail_write_attempt: AtlasSavedNumber
        let destination: AtlasSavedPath
        let parent_generation: AtlasSavedSearchOptional<AtlasSavedNumber>
        let parent_digest: AtlasSavedSearchOptional<String>
        let parent_items, member, file_id, source_revision: AtlasSavedNumber
        let path: AtlasSavedPath
        let source_digest: String
        let source_bytes: AtlasSavedNumber
        let selection: AtlasSavedTrailRange
        let title, rationale: String
        let source_verified: Bool
        let member_bytes_read, member_read_calls: AtlasSavedNumber
        let trail_bytes, bytes_written: AtlasSavedNumber
        let trail_digest: AtlasSavedSearchOptional<String>
        let total_items, reference_bytes: AtlasSavedSearchOptional<AtlasSavedNumber>
        let effect: AtlasSavedIndexBuildEffect
        let complete_artifact, attach_eligible: Bool
        let archive_path_reopened, live_source_reopened, source_payload_embedded, trail_attached: Bool
        let accepted_trail_unchanged, accepted_query_unchanged, source_derived_sensitive: Bool
        let encrypted, overwritten, power_loss_qualified, truncated: Bool
        let error: AtlasSavedSearchOptional<Failure>
    }
}

struct AtlasSavedTrailPreset: Identifiable {
    let id, label: String
    let range: AtlasSavedTrailRange
}

/// A draft owns its exact captured source and text while the user selects a
/// destination. Rejected writes and uncertain replies never erase that text.
@Observable @MainActor final class AtlasSavedTrailDraft: Identifiable {
    let id = UUID()
    let source: AtlasSavedReaderCapture
    let identity: AtlasSavedIdentity
    let readerIdentity: AtlasReaderIdentity
    let parent: AtlasSavedTrailParent?
    let presets: [AtlasSavedTrailPreset]
    var append: Bool { didSet { revision = UUID() } }
    var title = "Reading trail" { didSet { revision = UUID() } }
    var rationale = "" { didSet { revision = UUID() } }
    var startInput: String { didSet { revision = UUID() } }
    var endInput: String { didSet { revision = UUID() } }
    private(set) var submission: AtlasSavedTrailWriteRequest?
    private(set) var receipt: AtlasSavedTrailWriteReceipt?
    private var revision = UUID()

    init(source: AtlasSavedReaderCapture, parent: AtlasSavedTrailParent?) throws {
        guard let identity = source.archiveIdentity, let page = source.reader.page,
              source.reader.canKeep, source.reader.retainedCaptureSHA256 == source.member.digest,
              page.identity.pathBytes == source.member.path.bytes,
              page.identity.capturedBytes == source.member.observedBytes else { throw AtlasSavedError.unavailable }
        self.source = source; self.identity = identity; readerIdentity = page.identity; self.parent = parent
        append = parent != nil
        var presets: [AtlasSavedTrailPreset] = []
        var initialRationale = ""
        func add(_ id: String, _ label: String, _ start: UInt64, _ end: UInt64) throws {
            guard start <= end, end <= source.member.observedBytes else { throw AtlasSavedError.invalidResponse }
            presets.append(.init(id: id, label: label, range: try .init(start: start, end: end)))
        }
        if source.reader.hasTextSelectionIntent {
            guard !source.reader.resolvingTextSelection, let selected = source.reader.textSelection,
                  selected.identity.matches(page.identity), selected.pageStart == page.start, selected.pageEnd == page.end else {
                throw AtlasSavedError.unavailable
            }
            try add("text", "Selected text", selected.originalStart, selected.originalEnd)
        }
        if source.reader.findIsCurrent, let selected = page.selection, source.reader.matchHex != nil {
            try add("find", "Current Find result", selected.target.start, selected.target.end)
        } else if source.reader.outlineIsCurrent, let selected = page.symbolSelection, source.reader.symbolHex != nil {
            let range = selected.target.symbol.selectedRange
            try add("outline", "Current declaration", range.start, range.end)
        } else if source.reader.referencesAreCurrent, let selected = page.referenceSelection, source.reader.referenceHex != nil {
            let range = selected.target.occurrence.range
            try add("occurrence", "Current occurrence", range.start, range.end)
        }
        if let match = source.match, source.canUseMatch { try add("saved-match", "Saved search match", match.start, match.end) }
        else if case .trail(_, _, let entry) = source.origin, source.canUseTrailRange {
            try add("saved-trail", "Saved trail range", entry.range.start, entry.range.end)
            initialRationale = entry.rationale
        } else if case .importLink(_, let link, .evidence) = source.origin {
            try add("import", "Import statement", link.evidence.start, link.evidence.end)
        } else if case .structureHit(_, _, _, _, let item, _) = source.origin {
            try add("saved-structure", "Saved declaration", item.hit.start, item.hit.end)
        }
        try add("page", "Current source page", page.start, page.end)
        try add("file", "Whole captured file", 0, source.member.observedBytes)
        try add("point", "Bookmark at page start", page.start, page.start)
        self.presets = presets
        let initial = presets[0].range
        startInput = String(initial.start); endInput = String(initial.end)
        rationale = initialRationale
    }
    var effectiveTitle: String { append ? parent?.title ?? title : title }
    var selectedParent: AtlasSavedTrailParent? { append ? parent : nil }
    var range: AtlasSavedTrailRange? {
        guard let start = AtlasReaderWire.integer(startInput), let end = AtlasReaderWire.integer(endInput),
              start <= end, end <= source.member.observedBytes else { return nil }
        return try? .init(start: start, end: end)
    }
    var closeProtectionRevision: UUID? {
        if let submission, receipt?.operation == submission.operation, receipt?.succeeded == true,
           matchesForm(submission) { return nil }
        return revision
    }
    func choose(_ preset: AtlasSavedTrailPreset) { startInput = String(preset.range.start); endInput = String(preset.range.end) }
    func validation(in model: AtlasSavedRepositoryModel) -> String? {
        guard model.canAuthorTrail(from: source), source.reader.page?.identity.matches(readerIdentity) == true else {
            return "Keep the original saved archive and source reader open to write this trail. Your draft is preserved."
        }
        guard range != nil else { return "Enter a half-open original-byte range within 0…\(source.member.observedBytes). Equal endpoints create a point bookmark." }
        if effectiveTitle.utf8.count > 256 { return "The title exceeds 256 UTF-8 bytes." }
        if rationale.utf8.count > 4096 { return "The rationale exceeds 4,096 UTF-8 bytes. Your full text remains in this draft." }
        if append {
            guard let parent, model.trailIsCurrent, let current = model.trailPage, parent.matches(current) else {
                return "The attached trail changed. Choose New trail to keep this draft, or reopen the editor for the current attachment."
            }
            if parent.items >= 1024 { return "The attached trail already has 1,024 items. Start a new trail; prior visits are never dropped." }
            if let range, parent.referenceBytes.addingReportingOverflow(range.end - range.start).overflow {
                return "The appended trail would overflow its original-byte count. Start a new trail."
            }
        }
        return nil
    }
    func selection() throws -> AtlasSavedTrailSelection {
        guard let range else { throw AtlasSavedError.invalidInput }
        return try .init(identity: identity, member: source.member, readerIdentity: readerIdentity, start: range.start, end: range.end)
    }
    func submitted(_ request: AtlasSavedTrailWriteRequest) { submission = request; receipt = nil; revision = UUID() }
    func accept(_ receipt: AtlasSavedTrailWriteReceipt?) {
        guard let receipt, receipt.operation == submission?.operation, receipt.identity == identity else { return }
        self.receipt = receipt; revision = UUID()
    }
    private func matchesForm(_ request: AtlasSavedTrailWriteRequest) -> Bool {
        request.selection.range == range && request.parent?.generation == selectedParent?.generation &&
            request.parent?.digest == selectedParent?.digest && request.title.utf8.elementsEqual(effectiveTitle.utf8) &&
            request.rationale.utf8.elementsEqual(rationale.utf8)
    }
}

/// One explicit revision of the attached trail. The host edits its retained
/// canonical document; native pages never stand in for the full item inventory.
enum AtlasSavedTrailEditKind: UInt32, Sendable {
    case rename = 0, rationale = 1, move = 2, remove = 3
    var label: String {
        switch self {
        case .rename: return "Rename reading trail"
        case .rationale: return "Edit visit rationale"
        case .move: return "Move trail visit"
        case .remove: return "Remove trail visit"
        }
    }
    var wireName: String {
        switch self {
        case .rename: return "rename"
        case .rationale: return "set-rationale"
        case .move: return "move"
        case .remove: return "remove"
        }
    }
}
enum AtlasSavedTrailEdit: Sendable {
    case rename(String)
    case rationale(item: UInt64, text: String)
    case move(item: UInt64, to: UInt64)
    case remove(item: UInt64)
    var kind: AtlasSavedTrailEditKind {
        switch self {
        case .rename: return .rename
        case .rationale: return .rationale
        case .move: return .move
        case .remove: return .remove
        }
    }
    var item: UInt64? {
        switch self {
        case .rename: return nil
        case .rationale(let item, _), .move(let item, _), .remove(let item): return item
        }
    }
    var position: UInt64? { if case .move(_, let to) = self { return to }; return nil }
    var text: String {
        switch self {
        case .rename(let title), .rationale(_, let title): return title
        case .move, .remove: return ""
        }
    }
    func matches(_ other: Self) -> Bool {
        kind == other.kind && item == other.item && position == other.position && text.utf8.elementsEqual(other.text.utf8)
    }
}
struct AtlasSavedTrailEditRequest: Sendable {
    let operation: UInt64
    let parent: AtlasSavedTrailParent
    let entry: AtlasSavedTrailEntry?
    let edit: AtlasSavedTrailEdit
    let destination: String
    let totalItems, referenceBytes: UInt64
    var identity: AtlasSavedIdentity { parent.identity }
    var title: String { edit.kind == .rename ? edit.text : parent.title }

    init(operation: UInt64, parent: AtlasSavedTrailParent, entry: AtlasSavedTrailEntry?,
        edit: AtlasSavedTrailEdit, destination: String) throws {
        guard operation > 0, parent.generation > 0, AtlasSavedWire.digest(parent.digest),
              parent.items <= 1024, parent.title.utf8.count <= 256,
              AtlasSavedIndexBuildRequest.validDestination(destination) else { throw AtlasSavedError.invalidInput }
        if let item = edit.item {
            guard let entry, entry.id == item, item < parent.items else { throw AtlasSavedError.invalidInput }
            try entry.validate(in: parent.identity)
        } else if entry != nil { throw AtlasSavedError.invalidInput }
        switch edit {
        case .rename(let title):
            guard title.utf8.count <= 256 else { throw AtlasSavedError.invalidInput }
        case .rationale(_, let rationale):
            guard rationale.utf8.count <= 4096 else { throw AtlasSavedError.invalidInput }
        case .move(_, let to):
            guard to < parent.items else { throw AtlasSavedError.invalidInput }
        case .remove: break
        }
        let removedBytes = edit.kind == .remove ? entry!.range.end - entry!.range.start : 0
        guard removedBytes <= parent.referenceBytes else { throw AtlasSavedError.invalidInput }
        self.operation = operation; self.parent = parent; self.entry = entry; self.edit = edit; self.destination = destination
        totalItems = parent.items - (edit.kind == .remove ? 1 : 0)
        referenceBytes = parent.referenceBytes - removedBytes
    }
}

/// Edit receipts share the create/append operation clock and retained receipt
/// endpoint. They prove a metadata-only transformation without claiming source
/// verification, reopening archives, or changing the accepted attachment.
struct AtlasSavedTrailEditReceipt: Sendable {
    let identity: AtlasSavedIdentity
    let operation: UInt64
    let destination: AtlasSavedPath
    let effect: AtlasSavedIndexBuildEffect
    let succeeded, completeArtifact: Bool
    let trailBytes, bytesWritten: UInt64
    let digest: String?
    let totalItems, referenceBytes: UInt64?
    let errorCode, nextAction: String?
    var message: String {
        if succeeded { return "Saved the edited reading trail to a new file. Open that file explicitly to follow it or make another edit." }
        let code = errorCode ?? "unknown outcome"
        switch effect {
        case .none: return "No edited trail file was created (\(code)). Your draft remains available; choose a new filename for an explicit retry."
        case .incomplete: return "An incomplete edited trail remains at the destination (\(code)). Inspect it and choose a different new filename."
        case .fileSyncUnconfirmed: return "Complete edited trail bytes were written, but synchronization was not confirmed (\(code)). Inspect the destination explicitly."
        case .fileSyncRequested: return "Complete edited trail bytes were written and file synchronization was requested (\(code)). Inspect the destination explicitly."
        case .fileAndParentSyncRequested: return "The edited trail and its parent directory were synchronized, but the operation reported \(code). Inspect the destination explicitly."
        }
    }
    static func decode(_ json: String?, request: AtlasSavedTrailEditRequest) throws -> Self {
        guard let json, !json.isEmpty, json.utf8.count <= 256 * 1024 else { throw AtlasSavedError.unavailable }
        do {
            let data = Data(json.utf8)
            let w = try JSONDecoder().decode(Wire.self, from: data)
            let identity = try JSONDecoder().decode(AtlasSavedIdentity.self, from: data)
            guard identity == request.identity, w.schema == "fcb.saved-repository/1", w.command == "trail-edit",
                  w.status == "ok" || w.status == "error", w.mode == "edit",
                  w.operation.value == request.operation, w.last_trail_write_attempt.value == request.operation,
                  w.destination.bytes == Array(request.destination.utf8),
                  w.parent_generation.value == request.parent.generation, w.parent_digest == request.parent.digest,
                  w.parent_items.value == request.parent.items, w.title.utf8.elementsEqual(request.title.utf8),
                  w.edit.kind == request.edit.kind.wireName, w.edit.ordinal.value?.value == request.edit.item,
                  w.edit.destination_ordinal.value?.value == request.edit.position,
                  sameText(w.edit.title.value, request.edit.kind == .rename ? request.edit.text : nil),
                  sameText(w.edit.rationale.value, request.edit.kind == .rationale ? request.edit.text : nil),
                  !w.source_verified, w.source_anchors_unchanged, w.member_bytes_read.value == 0, w.member_read_calls.value == 0,
                  !w.archive_path_reopened, !w.live_source_reopened, !w.source_payload_embedded,
                  !w.trail_attached, w.accepted_trail_unchanged, w.accepted_query_unchanged,
                  w.source_derived_sensitive, !w.encrypted, !w.overwritten, !w.power_loss_qualified, !w.truncated,
                  w.trail_bytes.value <= 4 * 1024 * 1024, w.bytes_written.value <= w.trail_bytes.value else {
                throw AtlasSavedError.invalidResponse
            }
            let succeeded = w.status == "ok", error = w.error.value
            guard succeeded == (error == nil), !succeeded ||
                    w.effect == .fileSyncRequested || w.effect == .fileAndParentSyncRequested else {
                throw AtlasSavedError.invalidResponse
            }
            if let error {
                guard !error.code.isEmpty, error.code.utf8.count <= 512,
                      error.code.unicodeScalars.allSatisfy({ $0.value >= 32 && $0.value != 127 }),
                      !error.next_action.isEmpty, error.next_action.utf8.count <= 4096,
                      error.next_action.unicodeScalars.allSatisfy({ $0.value >= 32 && $0.value != 127 }) else {
                    throw AtlasSavedError.invalidResponse
                }
            }
            let digest = w.trail_digest.value, items = w.total_items.value?.value, references = w.reference_bytes.value?.value
            if w.trail_bytes.value == 0 {
                guard digest == nil, items == nil, references == nil else { throw AtlasSavedError.invalidResponse }
            } else {
                guard let digest, AtlasSavedWire.digest(digest), items == request.totalItems,
                      references == request.referenceBytes else { throw AtlasSavedError.invalidResponse }
            }
            guard w.effect != .none || w.bytes_written.value == 0,
                  w.effect == .none || (w.trail_bytes.value > 0 && digest != nil),
                  !w.effect.completeFile || (w.trail_bytes.value > 0 && w.bytes_written.value == w.trail_bytes.value),
                  w.complete_artifact == w.effect.completeFile, w.attach_eligible == w.complete_artifact,
                  !succeeded || w.complete_artifact else { throw AtlasSavedError.invalidResponse }
            return Self(identity: identity, operation: request.operation, destination: w.destination, effect: w.effect,
                succeeded: succeeded, completeArtifact: w.complete_artifact, trailBytes: w.trail_bytes.value,
                bytesWritten: w.bytes_written.value, digest: digest, totalItems: items, referenceBytes: references,
                errorCode: error?.code, nextAction: error?.next_action)
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }
    private static func sameText(_ actual: String?, _ expected: String?) -> Bool {
        switch (actual, expected) {
        case (nil, nil): return true
        case (let actual?, let expected?): return actual.utf8.elementsEqual(expected.utf8)
        default: return false
        }
    }
    private struct Failure: Decodable { let code, next_action: String }
    private struct Edit: Decodable {
        let kind: String
        let ordinal, destination_ordinal: AtlasSavedSearchOptional<AtlasSavedNumber>
        let title, rationale: AtlasSavedSearchOptional<String>
    }
    private struct Wire: Decodable {
        let schema, status, command, mode, title: String
        let operation, last_trail_write_attempt, parent_generation, parent_items: AtlasSavedNumber
        let destination: AtlasSavedPath
        let parent_digest: String
        let edit: Edit
        let source_verified, source_anchors_unchanged: Bool
        let member_bytes_read, member_read_calls, trail_bytes, bytes_written: AtlasSavedNumber
        let trail_digest: AtlasSavedSearchOptional<String>
        let total_items, reference_bytes: AtlasSavedSearchOptional<AtlasSavedNumber>
        let effect: AtlasSavedIndexBuildEffect
        let complete_artifact, attach_eligible: Bool
        let archive_path_reopened, live_source_reopened, source_payload_embedded, trail_attached: Bool
        let accepted_trail_unchanged, accepted_query_unchanged, source_derived_sensitive: Bool
        let encrypted, overwritten, power_loss_qualified, truncated: Bool
        let error: AtlasSavedSearchOptional<Failure>
    }
}

@Observable @MainActor final class AtlasSavedTrailEditDraft: Identifiable {
    let id = UUID()
    let parent: AtlasSavedTrailParent
    let entry: AtlasSavedTrailEntry?
    let kind: AtlasSavedTrailEditKind
    var text: String { didSet { revision = UUID() } }
    var positionInput: String { didSet { revision = UUID() } }
    private(set) var submission: AtlasSavedTrailEditRequest?
    private(set) var receipt: AtlasSavedTrailEditReceipt?
    private var revision = UUID()

    init(page: AtlasSavedTrailPage, entry: AtlasSavedTrailEntry?, kind: AtlasSavedTrailEditKind) throws {
        if kind == .rename {
            guard entry == nil else { throw AtlasSavedError.invalidInput }
        } else {
            guard let entry, page.items.contains(where: { $0.matches(entry) }) else { throw AtlasSavedError.invalidInput }
        }
        parent = AtlasSavedTrailParent(page); self.entry = entry; self.kind = kind
        text = kind == .rename ? page.title : entry?.rationale ?? ""
        positionInput = String((entry?.id ?? 0) + 1)
    }
    func edit() throws -> AtlasSavedTrailEdit {
        switch kind {
        case .rename: return .rename(text)
        case .rationale:
            guard let entry else { throw AtlasSavedError.invalidInput }
            return .rationale(item: entry.id, text: text)
        case .move:
            guard let entry, let position = AtlasReaderWire.integer(positionInput), position > 0,
                  position <= parent.items else { throw AtlasSavedError.invalidInput }
            return .move(item: entry.id, to: position - 1)
        case .remove:
            guard let entry else { throw AtlasSavedError.invalidInput }
            return .remove(item: entry.id)
        }
    }
    var closeProtectionRevision: UUID? {
        if let submission, receipt?.operation == submission.operation, receipt?.succeeded == true,
           let edit = try? edit(), submission.edit.matches(edit) { return nil }
        return revision
    }
    func validation(in model: AtlasSavedRepositoryModel) -> String? {
        guard model.trailIsCurrent, let current = model.trailPage, parent.matches(current) else {
            return "The attached trail changed. Your draft is preserved; reopen the editor for the current trail before saving."
        }
        guard model.canEditTrail else { return "Keep this saved archive open and wait for the current operation to finish. Your draft is preserved." }
        if kind == .rename && text.utf8.count > 256 { return "The title exceeds 256 UTF-8 bytes. Your full text remains in this draft." }
        if kind == .rationale && text.utf8.count > 4096 { return "The rationale exceeds 4,096 UTF-8 bytes. Your full text remains in this draft." }
        if (try? edit()) == nil { return "Enter the visit's final position from 1 through \(parent.items)." }
        return nil
    }
    func submitted(_ request: AtlasSavedTrailEditRequest) { submission = request; receipt = nil; revision = UUID() }
    func accept(_ receipt: AtlasSavedTrailEditReceipt?) {
        guard let receipt, receipt.operation == submission?.operation, receipt.identity == parent.identity else { return }
        self.receipt = receipt; revision = UUID()
    }
}
