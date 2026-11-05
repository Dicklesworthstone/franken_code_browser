import Foundation

enum AtlasReadingDeskError: Error, Sendable {
    case unavailable, invalidInput, invalidNote, staleNote, staleReattachment, staleExport, invalidResponse, canceled, engine(String)
    var message: String {
        switch self {
        case .unavailable: return "The reading desk could not admit this operation."
        case .invalidInput: return "Choose a valid file, byte range or bookmark label (up to 256 UTF-8 bytes)."
        case .invalidNote: return "Use a nonempty label up to 256 UTF-8 bytes without NUL, and note text up to 16 KiB."
        case .staleNote: return "The original source range or saved note version has changed. Your draft is preserved; reopen the current note or choose a current range before saving."
        case .staleReattachment: return "The saved note or selected destination changed after confirmation. Select the current range and review the reattachment again."
        case .staleExport: return "The reading desk changed after export review. Review the current bookmarks, notes and source budget before exporting again. No export request was sent."
        case .invalidResponse: return "The reading desk response could not be verified. Reload its accepted state before continuing."
        case .canceled: return "The operation was canceled before its next request began."
        case .engine(let code): return "Reading desk operation unavailable (\(code))."
        }
    }
}

struct AtlasDeskNumber: Decodable, Sendable {
    let value: UInt64
    init(from decoder: Decoder) throws {
        let text = try decoder.singleValueContainer().decode(String.self)
        guard !text.isEmpty, text.utf8.count <= 20,
              text.utf8.allSatisfy({ (48...57).contains($0) }),
              text == "0" || !text.hasPrefix("0"), let value = UInt64(text) else {
            throw AtlasReadingDeskError.invalidResponse
        }
        self.value = value
    }
}

struct AtlasDeskRange: Decodable, Sendable, Equatable {
    let start: UInt64
    let end: UInt64
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        start = try c.decode(AtlasDeskNumber.self, forKey: .start).value
        end = try c.decode(AtlasDeskNumber.self, forKey: .end).value
        guard start <= end, end <= 4 * 1024 * 1024 else { throw AtlasReadingDeskError.invalidResponse }
    }
    enum CodingKeys: CodingKey { case start, end }
}

struct AtlasDeskLocation: Decodable, Sendable, Equatable {
    let file: UInt64
    let revision: UInt64
    let offset: UInt64
    let selection: AtlasDeskRange?
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        file = try c.decode(AtlasDeskNumber.self, forKey: .file_id).value
        revision = try c.decode(AtlasDeskNumber.self, forKey: .source_revision).value
        offset = try c.decode(AtlasDeskNumber.self, forKey: .offset).value
        guard c.contains(.selection) else { throw AtlasReadingDeskError.invalidResponse }
        selection = try c.decodeIfPresent(AtlasDeskRange.self, forKey: .selection)
        guard file > 0, revision > 0, offset <= 4 * 1024 * 1024 else { throw AtlasReadingDeskError.invalidResponse }
    }
    enum CodingKeys: CodingKey { case file_id, source_revision, offset, selection }
}

struct AtlasDeskPane: Decodable, Sendable, Identifiable {
    let id: UInt64
    let label: String
    let pinned: Bool
    let location: AtlasDeskLocation
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(AtlasDeskNumber.self, forKey: .pane).value
        label = try c.decode(String.self, forKey: .label)
        pinned = try c.decode(Bool.self, forKey: .pinned)
        location = try AtlasDeskLocation(from: decoder)
        guard id > 0, !label.isEmpty, label.utf8.count <= 16_384 else { throw AtlasReadingDeskError.invalidResponse }
    }
    enum CodingKeys: CodingKey { case pane, label, pinned }
}

struct AtlasDeskBookmark: Decodable, Sendable, Identifiable {
    let id: UInt64
    let label: String
    let body: String
    let updateSequence: UInt64
    let location: AtlasDeskLocation
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(AtlasDeskNumber.self, forKey: .bookmark).value
        label = try c.decode(String.self, forKey: .label)
        body = try c.decode(String.self, forKey: .body)
        updateSequence = try c.decode(AtlasDeskNumber.self, forKey: .update_sequence).value
        location = try AtlasDeskLocation(from: decoder)
        guard id > 0, !label.isEmpty, label.utf8.count <= 256,
              body.utf8.count <= 16_384, updateSequence > 0 else { throw AtlasReadingDeskError.invalidResponse }
    }
    enum CodingKeys: CodingKey { case bookmark, label, body, update_sequence }
}

/// An editor owns this exact source anchor and note version. Navigation,
/// checkpoint replacement and another edit cannot silently retarget its draft.
struct AtlasDeskNoteTarget: Sendable {
    enum Kind: Sendable { case create(pane: UInt64), edit(bookmark: UInt64, sequence: UInt64) }
    let owner: UInt64
    let location: AtlasDeskLocation
    let kind: Kind

    init(state: AtlasDeskState, pane: AtlasDeskPane) {
        owner = state.owner; location = pane.location; kind = .create(pane: pane.id)
    }
    init(state: AtlasDeskState, bookmark: AtlasDeskBookmark) {
        self.init(owner: state.owner, bookmark: bookmark)
    }
    init(owner: UInt64, bookmark: AtlasDeskBookmark) {
        self.owner = owner; location = bookmark.location
        kind = .edit(bookmark: bookmark.id, sequence: bookmark.updateSequence)
    }
    var isNew: Bool { if case .create = kind { return true }; return false }
    func isCurrent(in state: AtlasDeskState?) -> Bool {
        guard let state, state.owner == owner else { return false }
        switch kind {
        case .create(let pane): return state.panes.first(where: { $0.id == pane })?.location == location
        case .edit(let bookmark, let sequence):
            guard let current = state.bookmarks.first(where: { $0.id == bookmark }) else { return false }
            return current.location == location && current.updateSequence == sequence
        }
    }
    static func validText(label: String, body: String) -> Bool {
        !label.isEmpty && label.utf8.count <= 256 && !label.utf8.contains(0) && body.utf8.count <= 16_384
    }
}

/// Retain the issued operation when its reply and reconciliation are both
/// unavailable. Checking its outcome sends only a state request, never a retry.
struct AtlasDeskNoteSubmission: Sendable {
    let target: AtlasDeskNoteTarget
    let attempt: UInt64
    let baseRevision: UInt64
    let priorBookmarks: Set<UInt64>
    let label: String
    let body: String

    init(target: AtlasDeskNoteTarget, state: AtlasDeskState, attempt: UInt64, label: String, body: String) throws {
        guard target.isCurrent(in: state) else { throw AtlasReadingDeskError.staleNote }
        guard AtlasDeskNoteTarget.validText(label: label, body: body), attempt > state.lastAttempt,
              !target.isNew || state.bookmarks.count < 32 else { throw AtlasReadingDeskError.invalidNote }
        if case .edit(_, let sequence) = target.kind, sequence == UInt64.max { throw AtlasReadingDeskError.unavailable }
        self.target = target; self.attempt = attempt; baseRevision = state.revision
        priorBookmarks = Set(state.bookmarks.map(\.id)); self.label = label; self.body = body
    }
    var request: AtlasDeskRequest {
        switch target.kind {
        case .create(let pane):
            return .annotate(revision: baseRevision, attempt: attempt, pane: pane, label: label, body: body)
        case .edit(let bookmark, _):
            return .editBookmark(revision: baseRevision, attempt: attempt, bookmark: bookmark, label: label, body: body)
        }
    }
    func outcome(in state: AtlasDeskState) -> AtlasDeskNoteOutcome {
        guard state.owner == target.owner, state.revision >= baseRevision else { return .unconfirmed(self) }
        // The foreign request has drained before reconciliation. A revision
        // below this attempt proves that this note mutation did not commit.
        if state.revision < attempt { return .notAccepted }
        guard state.revision == attempt, state.lastAttempt >= attempt else { return .unconfirmed(self) }
        let mark: AtlasDeskBookmark?
        switch target.kind {
        case .create:
            let added = state.bookmarks.filter { !priorBookmarks.contains($0.id) }
            guard added.count == 1, state.bookmarks.count == priorBookmarks.count + 1,
                  priorBookmarks.isSubset(of: Set(state.bookmarks.map(\.id))), added[0].updateSequence == 1 else {
                return .unconfirmed(self)
            }
            mark = added[0]
        case .edit(let bookmark, let sequence):
            guard sequence < UInt64.max, Set(state.bookmarks.map(\.id)) == priorBookmarks else { return .unconfirmed(self) }
            mark = state.bookmarks.first { $0.id == bookmark && $0.updateSequence == sequence + 1 }
        }
        guard let mark, mark.location == target.location,
              mark.label.utf8.elementsEqual(label.utf8), mark.body.utf8.elementsEqual(body.utf8) else {
            return .unconfirmed(self)
        }
        return .accepted(mark)
    }
}

enum AtlasDeskNoteOutcome: Sendable {
    case accepted(AtlasDeskBookmark)
    case notAccepted
    case unconfirmed(AtlasDeskNoteSubmission)
}


/// A deliberate destination freezes both the note version and the selected
/// captured range. The engine compares original bytes; matching lengths here
/// only make a useful candidate and never establish source identity.
struct AtlasDeskReattachment: Sendable {
    let owner: UInt64
    let baseRevision: UInt64
    let bookmark: AtlasDeskBookmark
    let pane: AtlasDeskPane
    let priorBookmarks: Set<UInt64>

    init?(state: AtlasDeskState, bookmark: AtlasDeskBookmark, pane: AtlasDeskPane) {
        guard let old = bookmark.location.selection, let selected = pane.location.selection,
              old.start < old.end, selected.start < selected.end,
              old.end - old.start == selected.end - selected.start,
              bookmark.updateSequence < UInt64.max, bookmark.location != pane.location else { return nil }
        owner = state.owner; baseRevision = state.revision
        self.bookmark = bookmark; self.pane = pane
        priorBookmarks = Set(state.bookmarks.map(\.id))
        guard isCurrent(in: state) else { return nil }
    }
    func isCurrent(in state: AtlasDeskState?) -> Bool {
        guard let state, state.owner == owner, state.revision == baseRevision,
              state.activePane == pane.id,
              state.panes.first(where: { $0.id == pane.id })?.location == pane.location,
              let current = state.bookmarks.first(where: { $0.id == bookmark.id }) else { return false }
        return current.location == bookmark.location && current.updateSequence == bookmark.updateSequence
            && current.label.utf8.elementsEqual(bookmark.label.utf8)
            && current.body.utf8.elementsEqual(bookmark.body.utf8)
    }
    func request(attempt: UInt64) -> AtlasDeskRequest {
        .apply(revision: baseRevision, attempt: attempt, kind: 12, subject: bookmark.id,
            start: pane.id, end: bookmark.updateSequence, label: "")
    }
    func outcome(in state: AtlasDeskState, attempt: UInt64) -> AtlasDeskReattachmentOutcome {
        guard state.owner == owner, state.revision >= baseRevision, attempt > baseRevision else { return .unconfirmed }
        // Called only after the mutation has drained. A lower accepted revision
        // proves nonacceptance; an advanced or unrelated state is not a receipt.
        if state.revision < attempt { return .notAccepted }
        guard state.revision == attempt, state.lastAttempt >= attempt,
              Set(state.bookmarks.map(\.id)) == priorBookmarks,
              let current = state.bookmarks.first(where: { $0.id == bookmark.id }),
              current.updateSequence == bookmark.updateSequence + 1,
              current.location == pane.location,
              current.label.utf8.elementsEqual(bookmark.label.utf8),
              current.body.utf8.elementsEqual(bookmark.body.utf8) else { return .unconfirmed }
        return .accepted
    }
}

enum AtlasDeskReattachmentOutcome: Sendable {
    case accepted, notAccepted, unconfirmed
}

struct AtlasDeskState: Sendable {
    let owner: UInt64
    let revision: UInt64
    let lastAttempt: UInt64
    let panes: [AtlasDeskPane]
    let bookmarks: [AtlasDeskBookmark]
    let activePane: UInt64?
    let retainedSources: UInt64
    let retainedBytes: UInt64
    let canBack: Bool
    let canForward: Bool
    var active: AtlasDeskPane? { panes.first { $0.id == activePane } }

    static func decode(_ json: String?, handle: UInt64, acceptedAttempt: UInt64? = nil) throws -> Self {
        let data = try AtlasDeskWire.admit(json, handle: handle, command: "state")
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            guard w.model_revision.value <= w.last_attempt.value,
                  w.panes.count <= 8, w.bookmarks.count <= 32,
                  Set(w.panes.map(\.id)).count == w.panes.count,
                  Set(w.bookmarks.map(\.id)).count == w.bookmarks.count,
                  w.active_pane == nil || w.panes.contains(where: { $0.id == w.active_pane?.value }),
                  w.retained_sources.value <= 32, w.retained_source_bytes.value <= 32 * 1024 * 1024,
                  w.persistence == "explicit-checkpoint", !w.autosave,
                  w.additional_source_bytes_read.value == 0, !w.native_presented else {
                throw AtlasReadingDeskError.invalidResponse
            }
            if let acceptedAttempt {
                guard acceptedAttempt > 0, w.model_revision.value == acceptedAttempt,
                      w.last_attempt.value == acceptedAttempt else { throw AtlasReadingDeskError.invalidResponse }
            }
            return Self(owner: handle, revision: w.model_revision.value, lastAttempt: w.last_attempt.value,
                panes: w.panes, bookmarks: w.bookmarks, activePane: w.active_pane?.value,
                retainedSources: w.retained_sources.value, retainedBytes: w.retained_source_bytes.value,
                canBack: w.can_go_back, canForward: w.can_go_forward)
        } catch let error as AtlasReadingDeskError { throw error }
        catch { throw AtlasReadingDeskError.invalidResponse }
    }
    private struct Wire: Decodable {
        let model_revision, last_attempt: AtlasDeskNumber
        let panes: [AtlasDeskPane]
        let bookmarks: [AtlasDeskBookmark]
        let active_pane: AtlasDeskNumber?
        let retained_sources, retained_source_bytes: AtlasDeskNumber
        let can_go_back, can_go_forward: Bool
        let persistence: String
        let autosave, native_presented: Bool
        let additional_source_bytes_read: AtlasDeskNumber
    }
}

struct AtlasDeskPage: Sendable {
    let range: AtlasDeskRange
    let text: String
    let nextOffset: UInt64?
    let firstLine: UInt64?
    let reachesEnd: Bool
    let hasReplacements: Bool

    static func decode(_ json: String?, state: AtlasDeskState, pane: AtlasDeskPane, offset: UInt64) throws -> Self {
        let data = try AtlasDeskWire.admit(json, handle: state.owner, command: "window")
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            let location = try JSONDecoder().decode(AtlasDeskLocation.self, from: data)
            guard w.model_revision.value == state.revision, w.last_attempt.value == state.lastAttempt,
                  w.pane.value == pane.id, location == pane.location,
                  w.original_range.start <= offset, offset <= w.original_range.end,
                  w.original_range.end - w.original_range.start <= 64 * 1024,
                  w.text.utf8.count <= 256 * 1024, w.rows.count <= 1024,
                  w.text_kind == "logical-captured-text-not-shaped", w.line_semantics == "editor-rows",
                  w.additional_source_bytes_read.value == 0, !w.native_presented,
                  !w.reaches_eof || w.next_offset == nil else { throw AtlasReadingDeskError.invalidResponse }
            if let next = w.next_offset?.value {
                guard next == w.original_range.end, next > offset else { throw AtlasReadingDeskError.invalidResponse }
            }
            return Self(range: w.original_range, text: w.text, nextOffset: w.next_offset?.value,
                firstLine: w.rows.first?.line?.value, reachesEnd: w.reaches_eof, hasReplacements: w.has_replacements)
        } catch let error as AtlasReadingDeskError { throw error }
        catch { throw AtlasReadingDeskError.invalidResponse }
    }
    private struct Wire: Decodable {
        let model_revision, last_attempt, pane: AtlasDeskNumber
        let original_range: AtlasDeskRange
        let text, text_kind, line_semantics: String
        let has_replacements, reaches_eof, native_presented: Bool
        let next_offset: AtlasDeskNumber?
        let rows: [Row]
        let additional_source_bytes_read: AtlasDeskNumber
    }
    private struct Row: Decodable { let line: AtlasDeskNumber? }
}

struct AtlasDeskSaveReceipt: Sendable {
    let effect: String
    let written: UInt64
    let checkpointBytes: UInt64
    let sourceBytes: UInt64
    let digest: String?
    let errorCode: String?
    let succeeded: Bool

    var message: String {
        if succeeded {
            return "Saved the reading desk with \(sourceBytes) original source bytes. \(written) bytes written; file and parent synchronization completed."
        }
        if effect == "none" {
            return "No checkpoint destination was created (\(errorCode ?? "unavailable")). Choose a new destination or retry the explicit save."
        }
        let stage: String
        switch effect {
        case "destination-created-incomplete": stage = "The destination contains an incomplete checkpoint."
        case "complete-file-sync-unconfirmed": stage = "The complete checkpoint was written, but file synchronization was not confirmed."
        case "complete-file-sync-requested": stage = "The complete checkpoint was written and file synchronization completed; parent synchronization was not confirmed."
        default: stage = "The checkpoint write outcome requires inspection."
        }
        return "\(written) bytes written. \(stage) Inspect the selected destination before retrying and use a new filename (\(errorCode ?? "unavailable"))."
    }

    static func decode(_ json: String?, state: AtlasDeskState) throws -> Self {
        let data = try AtlasDeskWire.admit(json, handle: state.owner, command: "save", allowWriteError: true)
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            let effects = ["none", "destination-created-incomplete", "complete-file-sync-unconfirmed",
                "complete-file-sync-requested", "complete-file-and-parent-sync-requested"]
            guard w.model_revision.value == state.revision, w.last_attempt.value == state.lastAttempt,
                  effects.contains(w.effect), w.bytes_written.value <= w.checkpoint_bytes.value,
                  w.checkpoint_bytes.value <= 64 * 1024 * 1024,
                  w.source_bytes.value == state.retainedBytes,
                  w.contains_source_payloads, w.contains_personal_labels, !w.encrypted, !w.overwritten,
                  !w.native_presented,
                  w.error == nil || (!w.error!.code.isEmpty && w.error!.code.utf8.count <= 512),
                  w.checkpoint_digest == nil || (w.checkpoint_digest!.utf8.count == 64 &&
                    w.checkpoint_digest!.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) })),
                  (w.status == "ok") == (w.error == nil),
                  (w.checkpoint_bytes.value == 0) == (w.checkpoint_digest == nil) else {
                throw AtlasReadingDeskError.invalidResponse
            }
            switch w.effect {
            case "none":
                guard w.bytes_written.value == 0, w.error != nil else { throw AtlasReadingDeskError.invalidResponse }
            case "destination-created-incomplete":
                guard w.checkpoint_bytes.value > 0, w.bytes_written.value < w.checkpoint_bytes.value,
                      w.error != nil else { throw AtlasReadingDeskError.invalidResponse }
            case "complete-file-sync-unconfirmed", "complete-file-sync-requested":
                guard w.checkpoint_bytes.value > 0, w.bytes_written.value == w.checkpoint_bytes.value,
                      w.error != nil else { throw AtlasReadingDeskError.invalidResponse }
            case "complete-file-and-parent-sync-requested":
                guard w.checkpoint_bytes.value > 0, w.bytes_written.value == w.checkpoint_bytes.value,
                      w.error == nil else { throw AtlasReadingDeskError.invalidResponse }
            default: throw AtlasReadingDeskError.invalidResponse
            }
            return Self(effect: w.effect, written: w.bytes_written.value, checkpointBytes: w.checkpoint_bytes.value,
                sourceBytes: w.source_bytes.value, digest: w.checkpoint_digest, errorCode: w.error?.code,
                succeeded: w.status == "ok")
        } catch let error as AtlasReadingDeskError { throw error }
        catch { throw AtlasReadingDeskError.invalidResponse }
    }
    private struct Wire: Decodable {
        let model_revision, last_attempt, bytes_written, checkpoint_bytes, source_bytes: AtlasDeskNumber
        let status, effect: String
        let checkpoint_digest: String?
        let contains_source_payloads, contains_personal_labels, encrypted, overwritten, native_presented: Bool
        let error: AtlasDeskWire.Failure?
    }
}

/// A native save-panel review grants disclosure of this exact accepted desk,
/// not whichever bookmarks happen to be present after an asynchronous change.
struct AtlasDeskSourcePackTarget: Sendable {
    let state: AtlasDeskState
    func isCurrent(in current: AtlasDeskState?) -> Bool {
        current?.owner == state.owner && current?.revision == state.revision && current?.lastAttempt == state.lastAttempt
    }
    var selectionCount: Int { state.bookmarks.filter { $0.location.selection != nil }.count }
}

struct AtlasDeskSourcePackLimits: Sendable, Equatable {
    let sourceBytes: UInt64
    var outputBytes: UInt64 = 16 * 1024 * 1024
    var outputScalars: UInt64 = 16 * 1024 * 1024
    var isValid: Bool {
        outputBytes > 0 && outputBytes <= 16 * 1024 * 1024 && outputScalars > 0 && outputScalars <= 16 * 1024 * 1024
    }
}

struct AtlasDeskSourcePackReceipt: Sendable {
    let effect: String
    let written, exportBytes, exportScalars, selectedBytes: UInt64
    let exported, omittedNoSelection, omittedSourceLimit, omittedOutputLimit: UInt64
    let digest: String?
    let errorCode: String?
    let succeeded, complete: Bool

    var message: String {
        if succeeded {
            let coverage = complete ? "All bookmarked selections are included."
                : "Omitted: \(omittedNoSelection) without a saved selection, \(omittedSourceLimit) over the source budget and \(omittedOutputLimit) over the JSON budget. All labels and full notes are included."
            return "Exported \(exported) selections (\(selectedBytes) original bytes). \(written) JSON bytes written; file and parent synchronization completed. \(coverage)"
        }
        if effect == "none" { return "No export destination was created (\(errorCode ?? "unavailable")). Review the budget and choose a new filename." }
        let stage: String
        switch effect {
        case "destination-created-incomplete": stage = "The destination contains an incomplete source pack."
        case "complete-file-sync-unconfirmed": stage = "The complete pack was written; file synchronization was not confirmed."
        case "complete-file-sync-requested": stage = "The complete pack was written and file synchronization completed; parent synchronization was not confirmed."
        default: stage = "The export outcome requires inspection."
        }
        return "\(written) bytes written. \(stage) Inspect the destination before retrying and use a new filename (\(errorCode ?? "unavailable"))."
    }

    static func decode(_ json: String?, target: AtlasDeskSourcePackTarget, limits: AtlasDeskSourcePackLimits) throws -> Self {
        let state = target.state
        let data = try AtlasDeskWire.admit(json, handle: state.owner, command: "export", allowWriteError: true)
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data), c = w.coverage
            let counts = [c.exported_selections.value, c.omitted_no_selection.value,
                c.omitted_source_limit.value, c.omitted_output_limit.value]
            let total = UInt64(state.bookmarks.count)
            guard limits.isValid, w.pack_schema == "fcb.desk-source-pack/1",
                  w.model_revision.value == state.revision, w.last_attempt.value == state.lastAttempt,
                  w.budgets.source_bytes.value == limits.sourceBytes,
                  w.budgets.output_bytes.value == limits.outputBytes, w.budgets.output_scalars.value == limits.outputScalars,
                  w.bytes_written.value <= w.export_bytes.value, w.export_bytes.value <= limits.outputBytes,
                  w.export_scalars.value <= limits.outputScalars, w.export_scalars.value <= w.export_bytes.value,
                  c.total_bookmarks.value == total, counts.allSatisfy({ $0 <= total }),
                  c.selected_original_bytes.value <= limits.sourceBytes,
                  w.contains_source_payloads == (c.selected_original_bytes.value > 0),
                  w.contains_personal_labels, w.contains_user_text, !w.redacted, !w.encrypted,
                  !w.overwritten, !w.live_roots_accessed, !w.native_presented, !w.power_loss_qualified,
                  w.error == nil || (!w.error!.code.isEmpty && w.error!.code.utf8.count <= 512),
                  (w.status == "ok") == (w.error == nil),
                  (w.export_bytes.value == 0) == (w.export_digest == nil),
                  w.export_digest == nil || (w.export_digest!.utf8.count == 64 &&
                    w.export_digest!.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) })) else {
                throw AtlasReadingDeskError.invalidResponse
            }
            if w.export_bytes.value > 0 {
                let selected = state.bookmarks.compactMap { $0.location.selection }
                let selectedBytes = selected.reduce(UInt64(0)) { $0 + $1.end - $1.start }
                guard counts.reduce(0, +) == total, w.export_scalars.value > 0,
                      c.omitted_no_selection.value == total - UInt64(selected.count),
                      c.exported_selections.value <= UInt64(selected.count),
                      c.selected_original_bytes.value <= selectedBytes,
                      c.selected_original_bytes.value <= w.export_bytes.value / 2,
                      w.complete == (c.exported_selections.value == total) else { throw AtlasReadingDeskError.invalidResponse }
            } else {
                guard counts.allSatisfy({ $0 == 0 }), c.selected_original_bytes.value == 0,
                      w.export_scalars.value == 0, !w.complete else { throw AtlasReadingDeskError.invalidResponse }
            }
            switch w.effect {
            case "none":
                guard w.bytes_written.value == 0, w.error != nil else { throw AtlasReadingDeskError.invalidResponse }
            case "destination-created-incomplete":
                guard w.export_bytes.value > 0, w.bytes_written.value < w.export_bytes.value,
                      w.error != nil else { throw AtlasReadingDeskError.invalidResponse }
            case "complete-file-sync-unconfirmed", "complete-file-sync-requested":
                guard w.export_bytes.value > 0, w.bytes_written.value == w.export_bytes.value,
                      w.error != nil else { throw AtlasReadingDeskError.invalidResponse }
            case "complete-file-and-parent-sync-requested":
                guard w.export_bytes.value > 0, w.bytes_written.value == w.export_bytes.value,
                      w.error == nil else { throw AtlasReadingDeskError.invalidResponse }
            default: throw AtlasReadingDeskError.invalidResponse
            }
            return Self(effect: w.effect, written: w.bytes_written.value, exportBytes: w.export_bytes.value,
                exportScalars: w.export_scalars.value, selectedBytes: c.selected_original_bytes.value,
                exported: c.exported_selections.value, omittedNoSelection: c.omitted_no_selection.value,
                omittedSourceLimit: c.omitted_source_limit.value, omittedOutputLimit: c.omitted_output_limit.value,
                digest: w.export_digest, errorCode: w.error?.code, succeeded: w.status == "ok", complete: w.complete)
        } catch let error as AtlasReadingDeskError { throw error }
        catch { throw AtlasReadingDeskError.invalidResponse }
    }
    private struct Wire: Decodable {
        let model_revision, last_attempt, bytes_written, export_bytes, export_scalars: AtlasDeskNumber
        let status, effect, pack_schema: String
        let export_digest: String?
        let budgets: Budgets
        let coverage: Coverage
        let complete, contains_source_payloads, contains_personal_labels, contains_user_text: Bool
        let redacted, encrypted, overwritten, live_roots_accessed, native_presented, power_loss_qualified: Bool
        let error: AtlasDeskWire.Failure?
    }
    private struct Budgets: Decodable { let source_bytes, output_bytes, output_scalars: AtlasDeskNumber }
    private struct Coverage: Decodable {
        let total_bookmarks, exported_selections, omitted_no_selection, omitted_source_limit, omitted_output_limit, selected_original_bytes: AtlasDeskNumber
    }
}

enum AtlasDeskWire {
    static let maximumResponseBytes = 4 * 1024 * 1024
    struct Failure: Decodable { let code: String }
    private struct Header: Decodable {
        let schema, status: String
        let owner: AtlasDeskNumber
        let command: String?
        let error: Failure?
    }
    static func admit(_ json: String?, handle: UInt64, command: String, allowWriteError: Bool = false) throws -> Data {
        guard let json, !json.isEmpty, json.utf8.count <= maximumResponseBytes else { throw AtlasReadingDeskError.unavailable }
        let data = Data(json.utf8)
        let header: Header
        do { header = try JSONDecoder().decode(Header.self, from: data) }
        catch { throw AtlasReadingDeskError.invalidResponse }
        guard handle > 0, header.schema == "fcb.reading-desk/1", header.owner.value == handle,
              ["ok", "error"].contains(header.status) else { throw AtlasReadingDeskError.invalidResponse }
        guard header.status != "ok" || header.error == nil else { throw AtlasReadingDeskError.invalidResponse }
        if header.status == "error", !(allowWriteError && header.command == command && ["save", "export"].contains(command)) {
            guard let code = header.error?.code, !code.isEmpty, code.utf8.count <= 512 else { throw AtlasReadingDeskError.invalidResponse }
            throw AtlasReadingDeskError.engine(code)
        }
        guard header.command == command else { throw AtlasReadingDeskError.invalidResponse }
        return data
    }
}

enum AtlasDeskRequest: Sendable {
    case start, state
    case capture(revision: UInt64, attempt: UInt64, path: String)
    case apply(revision: UInt64, attempt: UInt64, kind: UInt64, subject: UInt64, start: UInt64, end: UInt64, label: String)
    case annotate(revision: UInt64, attempt: UInt64, pane: UInt64, label: String, body: String)
    case editBookmark(revision: UInt64, attempt: UInt64, bookmark: UInt64, label: String, body: String)
    case window(revision: UInt64, pane: UInt64, offset: UInt64)
    case keep(revision: UInt64, attempt: UInt64, reader: UInt64, start: UInt64, end: UInt64)
    case openReader(revision: UInt64, pane: UInt64, destination: UInt64)
    case restore(revision: UInt64, attempt: UInt64, path: String)
    case save(revision: UInt64, path: String, sourceBytes: UInt64)
    case exportSourcePack(revision: UInt64, path: String, limits: AtlasDeskSourcePackLimits)
}

struct AtlasReadingDeskTransport: Sendable {
    let create: @Sendable () -> UInt64
    let run: @Sendable (UInt64, AtlasDeskRequest) -> String?
    let cancel: @Sendable (UInt64) -> Bool
    let close: @Sendable (UInt64) -> Bool
    let retirementFailed: @Sendable () -> Void
    /// Explicit capability for the loaded reader registry used by import. An
    /// equal numeric handle in a different embedded transport is not authority.
    let readerDomain: UUID?

    init(create: @escaping @Sendable () -> UInt64,
        run: @escaping @Sendable (UInt64, AtlasDeskRequest) -> String?,
        cancel: @escaping @Sendable (UInt64) -> Bool,
        close: @escaping @Sendable (UInt64) -> Bool,
        retirementFailed: @escaping @Sendable () -> Void,
        readerDomain: UUID? = nil) {
        self.create = create; self.run = run; self.cancel = cancel; self.close = close
        self.retirementFailed = retirementFailed; self.readerDomain = readerDomain
    }
}
