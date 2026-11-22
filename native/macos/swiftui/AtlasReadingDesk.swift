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
    init(file: UInt64, revision: UInt64, offset: UInt64, selection: AtlasDeskRange?) {
        self.file = file; self.revision = revision; self.offset = offset; self.selection = selection
    }
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
              !target.isNew || state.annotationCount < 32 else { throw AtlasReadingDeskError.invalidNote }
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
    let destination: AtlasDeskLocation
    private let requiresSelectedRange: Bool
    private let priorActivePane: UInt64?

    init?(state: AtlasDeskState, bookmark: AtlasDeskBookmark, pane: AtlasDeskPane) {
        guard let old = bookmark.location.selection, let selected = pane.location.selection,
              old.start < old.end, selected.start < selected.end,
              old.end - old.start == selected.end - selected.start,
              bookmark.updateSequence < UInt64.max, bookmark.location != pane.location else { return nil }
        owner = state.owner; baseRevision = state.revision
        self.bookmark = bookmark; self.pane = pane
        priorBookmarks = Set(state.bookmarks.map(\.id))
        destination = pane.location; requiresSelectedRange = true; priorActivePane = state.activePane
        guard isCurrent(in: state) else { return nil }
    }
    init?(proposal: AtlasDeskReattachmentProposal) {
        guard proposal.outcome == .mapped, let range = proposal.targetRange,
              proposal.target.bookmark.updateSequence < UInt64.max else { return nil }
        let target = proposal.target, state = target.state
        owner = state.owner; baseRevision = state.revision
        bookmark = target.bookmark; pane = target.pane
        priorBookmarks = Set(state.bookmarks.map(\.id)); priorActivePane = state.activePane
        destination = AtlasDeskLocation(file: pane.location.file, revision: pane.location.revision,
            offset: range.start, selection: range)
        requiresSelectedRange = false
        guard isCurrent(in: state) else { return nil }
    }
    func isCurrent(in state: AtlasDeskState?) -> Bool {
        guard let state, state.owner == owner, state.revision == baseRevision,
              (!requiresSelectedRange || state.activePane == pane.id),
              state.panes.first(where: { $0.id == pane.id })?.location == pane.location,
              let current = state.bookmarks.first(where: { $0.id == bookmark.id }) else { return false }
        return current.location == bookmark.location && current.updateSequence == bookmark.updateSequence
            && current.label.utf8.elementsEqual(bookmark.label.utf8)
            && current.body.utf8.elementsEqual(bookmark.body.utf8)
    }
    func request(attempt: UInt64) -> AtlasDeskRequest {
        if requiresSelectedRange {
            return .apply(revision: baseRevision, attempt: attempt, kind: 12, subject: bookmark.id,
                start: pane.id, end: bookmark.updateSequence, label: "")
        }
        // A guided destination is explicit and independent of the pane's
        // current selection. One engine transaction compares and moves it.
        return .reattachRange(revision: baseRevision, attempt: attempt, bookmark: bookmark.id,
            sequence: bookmark.updateSequence, pane: pane.id, file: destination.file,
            sourceRevision: destination.revision, start: destination.selection!.start, end: destination.selection!.end)
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
              current.location == destination,
              current.label.utf8.elementsEqual(bookmark.label.utf8),
              current.body.utf8.elementsEqual(bookmark.body.utf8) else { return .unconfirmed }
        if !requiresSelectedRange {
            guard state.activePane == priorActivePane,
                  state.panes.first(where: { $0.id == pane.id })?.location == pane.location else { return .unconfirmed }
        }
        return .accepted
    }
}

enum AtlasDeskReattachmentOutcome: Sendable {
    case accepted, notAccepted, unconfirmed
}

/// Absolute offsets of one saved observation. Only the range's length is
/// bounded; a passage from a multi-gigabyte file must keep its original origin.
struct AtlasDeskObservedRange: Decodable, Sendable, Equatable {
    let start, end: UInt64
    var count: UInt64 { end - start }
    init(start: UInt64, end: UInt64) throws {
        guard start <= end else { throw AtlasReadingDeskError.invalidInput }
        self.start = start; self.end = end
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        start = try c.decode(AtlasDeskNumber.self, forKey: .start).value
        end = try c.decode(AtlasDeskNumber.self, forKey: .end).value
        guard start <= end else { throw AtlasReadingDeskError.invalidResponse }
    }
    enum CodingKeys: CodingKey { case start, end }
}

struct AtlasDeskObservation: Decodable, Sendable, Identifiable, Equatable {
    let id, originOwner, originFile, originObservation: UInt64
    let path, pathHex, encoding, consistency, extentSHA256: String
    let capturedRange: AtlasDeskObservedRange
    let observedLength: UInt64
    let finalObservedLength: UInt64?
    static let maximumBytes: UInt64 = 1024 * 1024
    static let maximumSelectionBytes: UInt64 = 256 * 1024
    static let encodings = ["utf8", "utf16le", "utf16be"]
    static let consistencies = ["host-supplied-sequence", "unchanged-metadata-not-atomic",
        "changed-during-read", "short-read", "metadata-unavailable"]

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(AtlasDeskNumber.self, forKey: .observation).value
        originOwner = try c.decode(AtlasDeskNumber.self, forKey: .origin_owner).value
        originFile = try c.decode(AtlasDeskNumber.self, forKey: .origin_file).value
        originObservation = try c.decode(AtlasDeskNumber.self, forKey: .origin_observation).value
        path = try c.decode(String.self, forKey: .path)
        pathHex = try c.decode(String.self, forKey: .path_hex)
        encoding = try c.decode(String.self, forKey: .encoding)
        consistency = try c.decode(String.self, forKey: .consistency)
        extentSHA256 = try c.decode(String.self, forKey: .extent_sha256)
        capturedRange = try c.decode(AtlasDeskObservedRange.self, forKey: .captured_range)
        observedLength = try c.decode(AtlasDeskNumber.self, forKey: .observed_length).value
        finalObservedLength = try c.decode(AtlasDeskPresentOptional<AtlasDeskNumber>.self,
            forKey: .final_observed_length).value?.value
        guard try c.decode(String.self, forKey: .capture_kind) == "observed-extent",
              try c.decode(Bool.self, forKey: .whole_file_immutable) == false,
              try c.decode(Bool.self, forKey: .source_reopened) == false else {
            throw AtlasReadingDeskError.invalidResponse
        }
        guard id > 0, originOwner > 0, originFile > 0, originObservation > 0,
              Self.validPath(pathHex), !path.isEmpty, path.utf8.count <= 7 * 16_384,
              Self.encodings.contains(encoding), Self.consistencies.contains(consistency),
              capturedRange.count > 0, capturedRange.count <= Self.maximumBytes,
              capturedRange.end <= observedLength,
              Self.validHex(extentSHA256, bytes: 32) else { throw AtlasReadingDeskError.invalidResponse }
    }
    enum CodingKeys: CodingKey {
        case observation, origin_owner, origin_file, origin_observation, path, path_hex,
             encoding, consistency, extent_sha256, captured_range, observed_length, final_observed_length,
             capture_kind, whole_file_immutable, source_reopened
    }
    static func validHex(_ text: String, bytes: UInt64) -> Bool {
        bytes <= maximumBytes && text.utf8.count == Int(bytes * 2)
            && text.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
    static func validPath(_ hex: String) -> Bool {
        // Saved provenance is an arbitrary byte label, including NUL. It is
        // never interpreted as a live pathname; reader inputs have separate
        // filesystem authority and path validation before an import.
        !hex.isEmpty && hex.utf8.count <= 32_768 && hex.utf8.count.isMultiple(of: 2)
            && validHex(hex, bytes: UInt64(hex.utf8.count / 2))
    }
}

struct AtlasDeskObservedPassage: Decodable, Sendable, Identifiable {
    let id, observation, updateSequence: UInt64
    let label, body: String
    let selection: AtlasDeskObservedRange
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(AtlasDeskNumber.self, forKey: .passage).value
        observation = try c.decode(AtlasDeskNumber.self, forKey: .observation).value
        updateSequence = try c.decode(AtlasDeskNumber.self, forKey: .update_sequence).value
        label = try c.decode(String.self, forKey: .label)
        body = try c.decode(String.self, forKey: .body)
        selection = try c.decode(AtlasDeskObservedRange.self, forKey: .selection)
        guard id > 0, observation > 0, updateSequence > 0,
              !label.isEmpty, label.utf8.count <= 256, body.utf8.count <= 16_384,
              selection.count > 0, selection.count <= AtlasDeskObservation.maximumSelectionBytes else {
            throw AtlasReadingDeskError.invalidResponse
        }
    }
    enum CodingKeys: CodingKey { case passage, observation, update_sequence, label, body, selection }
    func matches(_ other: Self) -> Bool {
        id == other.id && observation == other.observation && updateSequence == other.updateSequence
            && selection == other.selection && label.utf8.elementsEqual(other.label.utf8)
            && body.utf8.elementsEqual(other.body.utf8)
    }
}

/// Immutable provenance supplied by an already resolved native selection. It
/// authorizes no pathname: the separate lifetime callback supplies its owner.
struct AtlasDeskObservedImport: Sendable {
    let originOwner, originFile, originObservation: UInt64
    let pathHex, encoding, consistency, extentSHA256: String
    let capturedRange, selection: AtlasDeskObservedRange
    let observedLength: UInt64
    let finalObservedLength: UInt64?

    var isValid: Bool {
        originOwner > 0 && originFile > 0 && originObservation > 0
            && AtlasDeskObservation.validPath(pathHex)
            && AtlasDeskObservation.encodings.contains(encoding)
            && AtlasDeskObservation.consistencies.contains(consistency)
            && AtlasDeskObservation.validHex(extentSHA256, bytes: 32)
            && capturedRange.count > 0 && capturedRange.count <= AtlasDeskObservation.maximumBytes
            && capturedRange.end <= observedLength
            && selection.count > 0 && selection.count <= AtlasDeskObservation.maximumSelectionBytes
            && selection.start >= capturedRange.start && selection.end <= capturedRange.end
    }
    func matches(_ observation: AtlasDeskObservation, passage: AtlasDeskObservedPassage) -> Bool {
        passage.observation == observation.id && passage.selection == selection
            && observation.originOwner == originOwner && observation.originFile == originFile
            && observation.originObservation == originObservation && observation.pathHex == pathHex
            && observation.encoding == encoding && observation.consistency == consistency
            && observation.extentSHA256 == extentSHA256 && observation.capturedRange == capturedRange
            && observation.observedLength == observedLength && observation.finalObservedLength == finalObservedLength
    }
}

struct AtlasDeskObservedTarget: Sendable, Identifiable {
    let owner: UInt64
    let observation: AtlasDeskObservation
    let passage: AtlasDeskObservedPassage
    var id: UInt64 { passage.id }

    init?(state: AtlasDeskState, passage: AtlasDeskObservedPassage) {
        guard let observation = state.observations.first(where: { $0.id == passage.observation }),
              state.observedPassages.contains(where: { $0.matches(passage) }) else { return nil }
        owner = state.owner; self.observation = observation; self.passage = passage
    }
    func isCurrent(in state: AtlasDeskState?) -> Bool {
        guard let state, state.owner == owner else { return false }
        return state.observations.contains { $0 == observation }
            && state.observedPassages.contains { $0.matches(passage) }
    }
}

struct AtlasDeskObservedPage: Sendable {
    let target: AtlasDeskObservedTarget
    let text, originalHex: String
    let containsReplacements: Bool
    let withLocationText: String

    static func decode(_ json: String?, state: AtlasDeskState, target: AtlasDeskObservedTarget) throws -> Self {
        guard target.isCurrent(in: state) else { throw AtlasReadingDeskError.staleNote }
        let data = try AtlasDeskWire.admit(json, handle: state.owner, command: "observed-passage")
        do {
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            guard wire.model_revision.value == state.revision, wire.last_attempt.value == state.lastAttempt,
                  wire.observation == target.observation, wire.passage.matches(target.passage),
                  wire.capture_kind == "observed-extent", !wire.whole_file_immutable,
                  !wire.source_reopened, !wire.native_presented, wire.additional_source_bytes_read.value == 0,
                  wire.text.utf8.count <= Int(AtlasDeskObservation.maximumSelectionBytes * 3),
                  AtlasDeskObservation.validHex(wire.original_hex, bytes: target.passage.selection.count),
                  !wire.contains_replacements || wire.text.unicodeScalars.contains(where: { $0.value == 0xfffd }) else {
                throw AtlasReadingDeskError.invalidResponse
            }
            let encoded: Data?
            switch target.observation.encoding {
            case "utf8": encoded = Data(wire.text.utf8)
            case "utf16le": encoded = wire.text.data(using: .utf16LittleEndian)
            case "utf16be": encoded = wire.text.data(using: .utf16BigEndian)
            default: encoded = nil
            }
            guard let encoded else { throw AtlasReadingDeskError.invalidResponse }
            let roundTrips = encoded.count == Int(target.passage.selection.count)
                && encoded.map({ String(format: "%02x", $0) }).joined() == wire.original_hex
            guard roundTrips == !wire.contains_replacements else { throw AtlasReadingDeskError.invalidResponse }
            let observation = target.observation, range = target.passage.selection
            var provenance: [String: Any] = [
                "source": "saved-observed-passage", "capture_kind": "observed-extent", "whole_file_immutable": false,
                "desk_owner": String(state.owner), "passage": String(target.passage.id),
                "origin_owner": String(observation.originOwner), "origin_file": String(observation.originFile),
                "origin_observation": String(observation.originObservation),
                "path": observation.path, "path_hex": observation.pathHex,
                "encoding": observation.encoding, "consistency": observation.consistency,
                "observed_length": String(observation.observedLength),
                "captured_range": ["start": String(observation.capturedRange.start), "end": String(observation.capturedRange.end)],
                "original_range": ["start": String(range.start), "end": String(range.end)],
                "extent_sha256": observation.extentSHA256,
                "contains_replacements": wire.contains_replacements, "line_endings": "preserved"
            ]
            provenance["final_observed_length"] = observation.finalObservedLength.map { String($0) } as Any? ?? NSNull()
            let location = String(decoding: try JSONSerialization.data(withJSONObject: provenance, options: [.sortedKeys]), as: UTF8.self)
            return Self(target: target, text: wire.text, originalHex: wire.original_hex,
                containsReplacements: wire.contains_replacements,
                withLocationText: "FCB saved observed passage (original byte offsets)\n" + location + "\n\n" + wire.text)
        } catch let error as AtlasReadingDeskError { throw error }
        catch { throw AtlasReadingDeskError.invalidResponse }
    }
    private struct Wire: Decodable {
        let model_revision, last_attempt, additional_source_bytes_read: AtlasDeskNumber
        let observation: AtlasDeskObservation
        let passage: AtlasDeskObservedPassage
        let text, original_hex, capture_kind: String
        let contains_replacements, whole_file_immutable, source_reopened, native_presented: Bool
    }
}

struct AtlasDeskObservedNoteSubmission: Sendable {
    let target: AtlasDeskObservedTarget
    let attempt, baseRevision: UInt64
    let label, body: String
    init(target: AtlasDeskObservedTarget, state: AtlasDeskState, attempt: UInt64, label: String, body: String) throws {
        guard target.isCurrent(in: state) else { throw AtlasReadingDeskError.staleNote }
        guard AtlasDeskNoteTarget.validText(label: label, body: body),
              target.passage.updateSequence < UInt64.max, attempt > state.lastAttempt else {
            throw AtlasReadingDeskError.invalidNote
        }
        self.target = target; self.attempt = attempt; baseRevision = state.revision
        self.label = label; self.body = body
    }
    func outcome(in state: AtlasDeskState) -> AtlasDeskObservedNoteOutcome {
        guard state.owner == target.owner, state.revision >= baseRevision else { return .unconfirmed(self) }
        if state.revision < attempt { return .notAccepted }
        guard state.revision == attempt, state.lastAttempt >= attempt,
              state.observations.contains(where: { $0 == target.observation }),
              let passage = state.observedPassages.first(where: { $0.id == target.passage.id }),
              passage.observation == target.passage.observation, passage.selection == target.passage.selection,
              passage.updateSequence == target.passage.updateSequence + 1,
              passage.label.utf8.elementsEqual(label.utf8), passage.body.utf8.elementsEqual(body.utf8),
              let current = AtlasDeskObservedTarget(state: state, passage: passage) else { return .unconfirmed(self) }
        return .accepted(current)
    }
}
enum AtlasDeskObservedNoteOutcome: Sendable {
    case accepted(AtlasDeskObservedTarget), notAccepted, unconfirmed(AtlasDeskObservedNoteSubmission)
}

/// A separate optional native registry capability. An observed handle from an
/// unrelated embedded transport cannot be used just because its number agrees.
struct AtlasDeskObservedTransport: Sendable {
    let domain: UUID
    let keep: @Sendable (UInt64, UInt64, UInt64, UInt64, UInt64, UInt64, UInt64) -> String?
    let read: @Sendable (UInt64, UInt64, UInt64) -> String?
    let edit: @Sendable (UInt64, UInt64, UInt64, UInt64, UInt64, String, String) -> String?
    let forget: @Sendable (UInt64, UInt64, UInt64, UInt64, UInt64) -> String?
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
    var observations: [AtlasDeskObservation] = []
    var observedPassages: [AtlasDeskObservedPassage] = []
    var annotationCount: Int { bookmarks.count + observedPassages.count }
    var active: AtlasDeskPane? { panes.first { $0.id == activePane } }

    static func decode(_ json: String?, handle: UInt64, acceptedAttempt: UInt64? = nil) throws -> Self {
        let data = try AtlasDeskWire.admit(json, handle: handle, command: "state")
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            guard w.model_revision.value <= w.last_attempt.value,
                  w.panes.count <= 8, w.bookmarks.count + w.observed_passages.count <= 32,
                  Set(w.panes.map(\.id)).count == w.panes.count,
                  Set(w.bookmarks.map(\.id)).count == w.bookmarks.count,
                  Set(w.observations.map(\.id)).count == w.observations.count,
                  Set(w.observed_passages.map(\.id)).count == w.observed_passages.count,
                  w.observations.count <= 32, UInt64(w.observations.count) <= w.retained_sources.value,
                  w.observations.reduce(UInt64(0), { $0 + $1.capturedRange.count }) <= w.retained_source_bytes.value,
                  w.observed_passages.allSatisfy({ passage in
                      w.observations.contains { $0.id == passage.observation
                          && passage.selection.start >= $0.capturedRange.start
                          && passage.selection.end <= $0.capturedRange.end }
                  }),
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
                canBack: w.can_go_back, canForward: w.can_go_forward,
                observations: w.observations, observedPassages: w.observed_passages)
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
        let observations: [AtlasDeskObservation]
        let observed_passages: [AtlasDeskObservedPassage]
        enum CodingKeys: CodingKey {
            case model_revision, last_attempt, panes, bookmarks, active_pane,
                 retained_sources, retained_source_bytes, can_go_back, can_go_forward,
                 persistence, autosave, native_presented, additional_source_bytes_read,
                 observations, observed_passages
        }
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            model_revision = try c.decode(AtlasDeskNumber.self, forKey: .model_revision)
            last_attempt = try c.decode(AtlasDeskNumber.self, forKey: .last_attempt)
            panes = try c.decode([AtlasDeskPane].self, forKey: .panes)
            bookmarks = try c.decode([AtlasDeskBookmark].self, forKey: .bookmarks)
            active_pane = try c.decodeIfPresent(AtlasDeskNumber.self, forKey: .active_pane)
            retained_sources = try c.decode(AtlasDeskNumber.self, forKey: .retained_sources)
            retained_source_bytes = try c.decode(AtlasDeskNumber.self, forKey: .retained_source_bytes)
            can_go_back = try c.decode(Bool.self, forKey: .can_go_back)
            can_go_forward = try c.decode(Bool.self, forKey: .can_go_forward)
            persistence = try c.decode(String.self, forKey: .persistence)
            autosave = try c.decode(Bool.self, forKey: .autosave)
            native_presented = try c.decode(Bool.self, forKey: .native_presented)
            additional_source_bytes_read = try c.decode(AtlasDeskNumber.self, forKey: .additional_source_bytes_read)
            // Old injected/embedded transports may omit both collections. A
            // present null or only one collection cannot erase saved passages.
            guard c.contains(.observations) == c.contains(.observed_passages) else {
                throw AtlasReadingDeskError.invalidResponse
            }
            observations = c.contains(.observations) ? try c.decode([AtlasDeskObservation].self, forKey: .observations) : []
            observed_passages = c.contains(.observed_passages) ? try c.decode([AtlasDeskObservedPassage].self, forKey: .observed_passages) : []
        }
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
    var selectionCount: Int {
        state.bookmarks.filter { $0.location.selection != nil }.count + state.observedPassages.count
    }
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
    var includedObservedPassages: UInt64 = 0
    var excludedObservedPassages: UInt64 = 0

    var message: String {
        if succeeded {
            let coverage = complete
                ? (includedObservedPassages > 0 ? "All bookmarked selections and observed passages are included."
                    : "All bookmarked selections are included.")
                : "Omitted source payloads: \(omittedNoSelection) without a saved selection, \(omittedSourceLimit) over the source budget and \(omittedOutputLimit) over the JSON budget. All exported records retain their labels, full notes and provenance."
            let observed = includedObservedPassages > 0
                ? " \(includedObservedPassages) observed passage records preserve original locations and observation evidence; their payloads contain only the selected bytes."
                : (excludedObservedPassages > 0
                    ? " Excluded \(excludedObservedPassages) observed passages and their notes; this host exports bookmarked captures only."
                    : "")
            return "Exported \(exported) selections (\(selectedBytes) original bytes). \(written) JSON bytes written; file and parent synchronization completed. \(coverage)\(observed)"
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
            let withObservations = w.pack_schema == "fcb.desk-source-pack/2"
            let bookmarks = UInt64(state.bookmarks.count), observations = UInt64(state.observedPassages.count)
            let includedObservations = withObservations ? observations : 0
            let total = bookmarks + includedObservations
            let observationCoverage: Bool
            if withObservations {
                observationCoverage = observations > 0 && c.total_observed_passages?.value == observations
                    && w.observed_passages_included == true && w.excluded_observed_passages?.value == 0
                    && w.completeness_scope == "all-bookmarked-selections-and-observed-passages"
            } else {
                // Accept earlier hosts only when the narrower coverage is
                // explicit. Legacy receipts without these fields are valid
                // solely for desks with no observed passages to omit.
                observationCoverage = (c.total_observed_passages?.value ?? 0) == 0
                    && (w.completeness_scope == nil || w.completeness_scope == "all-bookmarked-selections")
                    && ((w.observed_passages_included == nil && w.excluded_observed_passages == nil && observations == 0)
                        || (w.observed_passages_included == false && w.excluded_observed_passages?.value == observations))
            }
            guard limits.isValid, ["fcb.desk-source-pack/1", "fcb.desk-source-pack/2"].contains(w.pack_schema),
                  w.model_revision.value == state.revision, w.last_attempt.value == state.lastAttempt,
                  w.budgets.source_bytes.value == limits.sourceBytes,
                  w.budgets.output_bytes.value == limits.outputBytes, w.budgets.output_scalars.value == limits.outputScalars,
                  w.bytes_written.value <= w.export_bytes.value, w.export_bytes.value <= limits.outputBytes,
                  w.export_scalars.value <= limits.outputScalars, w.export_scalars.value <= w.export_bytes.value,
                  c.total_bookmarks.value == bookmarks, counts.allSatisfy({ $0 <= total }),
                  c.selected_original_bytes.value <= limits.sourceBytes,
                  w.contains_source_payloads == (c.selected_original_bytes.value > 0),
                  w.contains_personal_labels, w.contains_user_text, !w.redacted, !w.encrypted,
                  observationCoverage,
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
                let observedBytes = withObservations
                    ? state.observedPassages.reduce(UInt64(0)) { $0 + $1.selection.count } : 0
                let selectedBytes = selected.reduce(observedBytes) { $0 + $1.end - $1.start }
                let selectedCount = UInt64(selected.count) + includedObservations
                guard counts.reduce(0, +) == total, w.export_scalars.value > 0,
                      c.omitted_no_selection.value == bookmarks - UInt64(selected.count),
                      c.exported_selections.value <= selectedCount,
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
                digest: w.export_digest, errorCode: w.error?.code, succeeded: w.status == "ok", complete: w.complete,
                includedObservedPassages: includedObservations,
                excludedObservedPassages: w.excluded_observed_passages?.value ?? 0)
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
        let observed_passages_included: Bool?
        let excluded_observed_passages: AtlasDeskNumber?
        let completeness_scope: String?
    }
    private struct Budgets: Decodable { let source_bytes, output_bytes, output_scalars: AtlasDeskNumber }
    private struct Coverage: Decodable {
        let total_bookmarks, exported_selections, omitted_no_selection, omitted_source_limit, omitted_output_limit, selected_original_bytes: AtlasDeskNumber
        let total_observed_passages: AtlasDeskNumber?
    }
}

enum AtlasDeskWire {
    // Full state can include 32 escaped path labels and 32 full personal notes.
    // Other receipts keep the original smaller framing allowance.
    static let maximumResponseBytes = 16 * 1024 * 1024
    static let maximumNonStateResponseBytes = 4 * 1024 * 1024
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
        guard (command == "state" && header.command == "state") || data.count <= maximumNonStateResponseBytes else {
            throw AtlasReadingDeskError.invalidResponse
        }
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
    case suggestReattachment(revision: UInt64, bookmark: UInt64, sequence: UInt64, pane: UInt64,
        maxWork: UInt64, maxEdits: UInt64)
    case reattachRange(revision: UInt64, attempt: UInt64, bookmark: UInt64, sequence: UInt64, pane: UInt64,
        file: UInt64, sourceRevision: UInt64, start: UInt64, end: UInt64)
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
    let observed: AtlasDeskObservedTransport?

    init(create: @escaping @Sendable () -> UInt64,
        run: @escaping @Sendable (UInt64, AtlasDeskRequest) -> String?,
        cancel: @escaping @Sendable (UInt64) -> Bool,
        close: @escaping @Sendable (UInt64) -> Bool,
        retirementFailed: @escaping @Sendable () -> Void,
        readerDomain: UUID? = nil, observed: AtlasDeskObservedTransport? = nil) {
        self.create = create; self.run = run; self.cancel = cancel; self.close = close
        self.retirementFailed = retirementFailed; self.readerDomain = readerDomain
        self.observed = observed
    }
}


/// Freeze the original note and the user-chosen destination before worker
/// comparison. The saved source may exist only through this note, with no open
/// pane. A failed comparison never changes its anchor or consumes an attempt.
struct AtlasDeskComparisonTarget: Sendable {
    let state: AtlasDeskState
    let bookmark: AtlasDeskBookmark
    let pane: AtlasDeskPane
    let maxWork: UInt64
    let maxEdits: UInt64

    init?(state: AtlasDeskState, bookmark: AtlasDeskBookmark, pane: AtlasDeskPane,
        maxWork: UInt64 = 8 * 1024 * 1024, maxEdits: UInt64 = 256) {
        guard let selected = bookmark.location.selection, selected.start < selected.end,
              bookmark.updateSequence < UInt64.max, maxWork <= 64 * 1024 * 1024, maxEdits <= 512 else { return nil }
        self.state = state; self.bookmark = bookmark; self.pane = pane
        self.maxWork = maxWork; self.maxEdits = maxEdits
        guard isCurrent(in: state) else { return nil }
    }
    func isCurrent(in current: AtlasDeskState?) -> Bool {
        guard let current, current.owner == state.owner, current.revision == state.revision,
              current.lastAttempt == state.lastAttempt,
              let saved = current.bookmarks.first(where: { $0.id == bookmark.id }),
              let target = current.panes.first(where: { $0.id == pane.id }) else { return false }
        return saved.updateSequence == bookmark.updateSequence && saved.location == bookmark.location
            && saved.label.utf8.elementsEqual(bookmark.label.utf8) && saved.body.utf8.elementsEqual(bookmark.body.utf8)
            && target.location == pane.location && target.label.utf8.elementsEqual(pane.label.utf8)
    }
    var request: AtlasDeskRequest {
        .suggestReattachment(revision: state.revision, bookmark: bookmark.id, sequence: bookmark.updateSequence,
            pane: pane.id, maxWork: maxWork, maxEdits: maxEdits)
    }
}

private struct AtlasDeskPresentOptional<Value: Decodable & Sendable>: Decodable, Sendable {
    let value: Value?
    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { value = nil } else { value = try c.decode(Value.self) }
    }
}

struct AtlasDeskComparisonSource: Decodable, Sendable {
    let file_id, source_revision, bytes: AtlasDeskNumber
    let label: String
    func matches(_ location: AtlasDeskLocation) -> Bool {
        file_id.value == location.file && source_revision.value == location.revision
            && bytes.value <= 4 * 1024 * 1024 && !label.isEmpty && label.utf8.count <= 16_384
            && location.offset <= bytes.value && (location.selection?.end ?? 0) <= bytes.value
    }
}

struct AtlasDeskReattachmentPreview: Decodable, Sendable {
    let original_range: AtlasDeskRange
    let content_start: AtlasDeskNumber
    let text, text_kind: String
    let has_replacements: Bool
    private let selection_window_utf8_range: AtlasDeskPresentOptional<AtlasDeskRange>
    var selectionRange: AtlasDeskRange? { selection_window_utf8_range.value }
    func validate(selected: AtlasDeskRange, sourceBytes: UInt64) throws {
        let anchor = max(selected.start, content_start.value)
        guard [0, 2, 3].contains(content_start.value), content_start.value <= sourceBytes,
              original_range.start <= anchor, anchor <= original_range.end,
              original_range.start < original_range.end || (anchor == sourceBytes && original_range.end == sourceBytes),
              original_range.end <= sourceBytes, original_range.end - original_range.start <= 4096,
              text.utf8.count <= 16_384, text_kind == "logical-captured-text-not-shaped" else {
            throw AtlasReadingDeskError.invalidResponse
        }
        if let range = selectionRange {
            // A nonempty original range can decode to an empty text range
            // (for example an encoding BOM). Original bytes remain authority.
            guard selected.start >= original_range.start, selected.end <= original_range.end, range.start <= range.end,
                  range.end <= UInt64(text.utf8.count) else { throw AtlasReadingDeskError.invalidResponse }
            let bytes = Array(text.utf8)
            guard (range.start == UInt64(bytes.count) || bytes[Int(range.start)] & 0xC0 != 0x80),
                  (range.end == UInt64(bytes.count) || bytes[Int(range.end)] & 0xC0 != 0x80) else {
                throw AtlasReadingDeskError.invalidResponse
            }
        }
    }
}

struct AtlasDeskReattachmentProposal: Sendable {
    enum Outcome: String, Decodable, Sendable { case mapped, unchanged, changed, unresolved }
    let target: AtlasDeskComparisonTarget
    let outcome: Outcome
    let targetRange: AtlasDeskRange?
    let complete: Bool
    let quality: String
    let workUnits: UInt64
    let beforeSource: AtlasDeskComparisonSource
    let afterSource: AtlasDeskComparisonSource
    let beforePreview: AtlasDeskReattachmentPreview
    let afterPreview: AtlasDeskReattachmentPreview?

    static func decode(_ json: String?, target: AtlasDeskComparisonTarget) throws -> Self {
        let data = try AtlasDeskWire.admit(json, handle: target.state.owner, command: "suggest-reattachment")
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            guard target.isCurrent(in: target.state),
                  w.model_revision.value == target.state.revision, w.last_attempt.value == target.state.lastAttempt,
                  w.bookmark.value == target.bookmark.id, w.update_sequence.value == target.bookmark.updateSequence,
                  w.target_pane.value == target.pane.id, w.original_range == target.bookmark.location.selection,
                  w.original_range.start < w.original_range.end,
                  w.before_source.matches(target.bookmark.location), w.after_source.matches(target.pane.location),
                  w.after_source.label.utf8.elementsEqual(target.pane.label.utf8),
                  w.original_range.end <= w.before_source.bytes.value,
                  w.max_work.value == target.maxWork, w.work_units.value <= target.maxWork,
                  w.max_edit_distance.value == target.maxEdits, w.max_source_bytes.value == 4 * 1024 * 1024,
                  ["exact", "work-limit", "edit-limit"].contains(w.comparison_quality),
                  w.comparison_complete == (w.comparison_quality == "exact"),
                  w.coordinate_domain == "original-bytes-may-split-text",
                  w.alignment_semantics == "deterministic-not-identity-proof",
                  !w.source_reopened, !w.native_presented, w.additional_source_bytes_read.value == 0 else {
                throw AtlasReadingDeskError.invalidResponse
            }
            try w.before_preview.validate(selected: w.original_range, sourceBytes: w.before_source.bytes.value)
            let range = w.target_range.value, preview = w.after_preview.value
            switch w.outcome {
            case .mapped, .unchanged:
                guard let range, let preview, range.start < range.end, range.end <= w.after_source.bytes.value,
                      range.end - range.start == w.original_range.end - w.original_range.start else {
                    throw AtlasReadingDeskError.invalidResponse
                }
                let unchanged = target.bookmark.location.file == target.pane.location.file
                    && target.bookmark.location.revision == target.pane.location.revision && range == w.original_range
                guard (w.outcome == .unchanged) == unchanged else { throw AtlasReadingDeskError.invalidResponse }
                try preview.validate(selected: range, sourceBytes: w.after_source.bytes.value)
            case .changed, .unresolved:
                guard range == nil, preview == nil, w.outcome != .unresolved || !w.comparison_complete else {
                    throw AtlasReadingDeskError.invalidResponse
                }
            }
            return Self(target: target, outcome: w.outcome, targetRange: range,
                complete: w.comparison_complete, quality: w.comparison_quality, workUnits: w.work_units.value,
                beforeSource: w.before_source, afterSource: w.after_source,
                beforePreview: w.before_preview, afterPreview: preview)
        } catch let error as AtlasReadingDeskError { throw error }
        catch { throw AtlasReadingDeskError.invalidResponse }
    }
    private struct Wire: Decodable {
        let model_revision, last_attempt, bookmark, update_sequence, target_pane: AtlasDeskNumber
        let outcome: Outcome
        let original_range: AtlasDeskRange
        let target_range: AtlasDeskPresentOptional<AtlasDeskRange>
        let comparison_complete: Bool
        let comparison_quality, coordinate_domain, alignment_semantics: String
        let source_reopened, native_presented: Bool
        let additional_source_bytes_read, work_units, max_work, max_edit_distance, max_source_bytes: AtlasDeskNumber
        let before_source, after_source: AtlasDeskComparisonSource
        let before_preview: AtlasDeskReattachmentPreview
        let after_preview: AtlasDeskPresentOptional<AtlasDeskReattachmentPreview>
    }
}
