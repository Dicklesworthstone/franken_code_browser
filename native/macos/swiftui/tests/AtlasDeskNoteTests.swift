import Foundation

/// Injected boundary for production desk/draft models. This does not qualify
/// the C ABI, AppKit text editing, checkpoint storage or a native Mac run.
private final class NoteFixture: @unchecked Sendable {
    private struct Mark {
        let id: UInt64
        var label: String
        var body: String
        var sequence: UInt64
        var file: UInt64
        var offset: UInt64
        var selection: (UInt64, UInt64)?
    }
    private let lock = NSLock()
    let owner: UInt64 = 55
    let source = "retained source\n"
    private var revision: UInt64 = 1
    private var lastAttempt: UInt64 = 1
    private var pane: UInt64 = 1
    private var file: UInt64 = 1
    private var offset: UInt64 = 2
    private var selection: (UInt64, UInt64)? = (2, 7)
    private var marks: [Mark] = []
    private var nextBookmark: UInt64 = 1
    private var reattachmentRequests = 0
    private var comparisonRequests = 0
    private var noteRequests = 0
    private var acceptedNotes = 0
    private var stateReads = 0
    private var closes = 0
    private var loseReply = false
    private var rejectMutation = false
    private var failedReads = 0
    private var failPage = false
    private var enteredGate: DispatchSemaphore?
    private var resumeGate: DispatchSemaphore?

    var transport: AtlasReadingDeskTransport {
        AtlasReadingDeskTransport(create: { [self] in owner }, run: { [self] in response($0, $1) },
            cancel: { _ in true }, close: { [self] _ in locked { closes += 1 }; return true },
            retirementFailed: { fatalError("note desk retirement refused") })
    }
    var requestCount: Int { locked { noteRequests } }
    var reattachmentCount: Int { locked { reattachmentRequests } }
    var comparisonCount: Int { locked { comparisonRequests } }
    var proposalJSON: String { locked { encode(proposal(maxWork: 8 * 1024 * 1024, maxEdits: 256)) } }
    var acceptedCount: Int { locked { acceptedNotes } }
    var stateCount: Int { locked { stateReads } }
    var closeCount: Int { locked { closes } }
    var stateJSON: String { locked { encode(state()) } }
    func loseNextReply(stateFailures: Int = 0) { locked { loseReply = true; failedReads = stateFailures } }
    func rejectNextNote() { locked { rejectMutation = true } }
    func failNextPage() { locked { failPage = true } }
    func gateNextNote(entered: DispatchSemaphore, resume: DispatchSemaphore) {
        locked { enteredGate = entered; resumeGate = resume }
    }
    func selectOtherCapture(start: UInt64 = 2, end: UInt64 = 7) {
        locked { revision += 1; lastAttempt = revision; file += 1; offset = start; selection = (start, end) }
    }
    func movePane() { locked { revision += 1; lastAttempt = revision; offset = 10; selection = nil } }
    func replaceDesk() {
        locked { revision += 1; lastAttempt = revision; pane += 1; file += 1; offset = 0; selection = nil; marks = [] }
    }
    func changeNoteElsewhere() {
        locked { revision += 1; lastAttempt = revision; marks[0].body = "another accepted edit"; marks[0].sequence += 1 }
    }
    func seedBookmark(label: String = "legacy label", body: String = "", sequence: UInt64 = 1) {
        locked {
            marks.append(Mark(id: nextBookmark, label: label, body: body, sequence: sequence,
                file: file, offset: offset, selection: selection)); nextBookmark += 1
        }
    }
    private func locked<Value>(_ work: () -> Value) -> Value { lock.lock(); defer { lock.unlock() }; return work() }
    private func response(_ handle: UInt64, _ request: AtlasDeskRequest) -> String? {
        let isNote: Bool
        switch request {
        case .annotate, .editBookmark, .suggestReattachment, .reattachRange, .apply(_, _, 12, _, _, _, _): isNote = true
        default: isNote = false
        }
        let gates = locked { () -> (DispatchSemaphore?, DispatchSemaphore?) in
            guard isNote else { return (nil, nil) }
            let gates = (enteredGate, resumeGate); enteredGate = nil; resumeGate = nil; return gates
        }
        gates.0?.signal(); gates.1?.wait()
        return locked {
            precondition(handle == owner)
            switch request {
            case .start: return encode(state())
            case .state:
                stateReads += 1
                if failedReads > 0 { failedReads -= 1; return nil }
                return encode(state())
            case .annotate(let expected, let attempt, let subject, let label, let body):
                noteRequests += 1
                guard admit(expected, attempt) else { return error("DESK_STALE_ATTEMPT") }
                if rejectMutation { rejectMutation = false; return error("DESK_CANCELED") }
                guard subject == pane, marks.count < 32 else { return error("DESK_INVALID_LOCATION") }
                marks.append(Mark(id: nextBookmark, label: label, body: body, sequence: 1,
                    file: file, offset: offset, selection: selection)); nextBookmark += 1
                revision = attempt; acceptedNotes += 1
                return noteReply()
            case .editBookmark(let expected, let attempt, let subject, let label, let body):
                noteRequests += 1
                guard admit(expected, attempt) else { return error("DESK_STALE_ATTEMPT") }
                if rejectMutation { rejectMutation = false; return error("DESK_CANCELED") }
                guard let index = marks.firstIndex(where: { $0.id == subject }), marks[index].sequence < UInt64.max else {
                    return error("DESK_INVALID_BOOKMARK")
                }
                marks[index].label = label; marks[index].body = body; marks[index].sequence += 1
                revision = attempt; acceptedNotes += 1
                return noteReply()
            case .apply(let expected, let attempt, let kind, let subject, let start, let end, let label):
                if kind == 12 { reattachmentRequests += 1 }
                guard admit(expected, attempt) else { return error("DESK_STALE_ATTEMPT") }
                switch kind {
                case 1: offset = start; selection = nil
                case 2: offset = start; selection = (start, end)
                case 3:
                    marks.append(Mark(id: nextBookmark, label: label, body: "", sequence: 1,
                        file: file, offset: offset, selection: selection)); nextBookmark += 1
                case 4:
                    guard let mark = marks.first(where: { $0.id == subject }) else { return error("DESK_INVALID_BOOKMARK") }
                    file = mark.file; offset = mark.offset; selection = mark.selection
                case 5: marks.removeAll { $0.id == subject }
                case 12:
                    if rejectMutation { rejectMutation = false; return error("DESK_CANCELED") }
                    guard start == pane, let index = marks.firstIndex(where: { $0.id == subject }),
                          marks[index].sequence == end, end < UInt64.max else { return error("DESK_STALE_BOOKMARK") }
                    guard let old = marks[index].selection, let selected = selection,
                          old.0 < old.1, selected.0 < selected.1 else { return error("DESK_INVALID_LOCATION") }
                    let bytes = Array(source.utf8)
                    guard bytes[Int(old.0)..<Int(old.1)].elementsEqual(bytes[Int(selected.0)..<Int(selected.1)]) else {
                        return error("DESK_SELECTION_MISMATCH")
                    }
                    marks[index].file = file; marks[index].offset = offset; marks[index].selection = selection
                    marks[index].sequence += 1; revision = attempt
                    return noteReply()
                default: return error("DESK_UNSUPPORTED_FIXTURE_COMMAND")
                }
                revision = attempt; return encode(state())
            case .window(let expected, let subject, let position):
                guard expected == revision, subject == pane, position <= UInt64(source.utf8.count) else {
                    return error("DESK_INVALID_LOCATION")
                }
                if failPage { failPage = false; return nil }
                var w = header("window"); w.merge(location(file: file, offset: offset, selection: selection)) { _, next in next }
                w["pane"] = String(pane)
                w["original_range"] = ["start": String(position), "end": String(source.utf8.count)]
                w["text"] = String(decoding: source.utf8.dropFirst(Int(position)), as: UTF8.self)
                w["text_kind"] = "logical-captured-text-not-shaped"; w["line_semantics"] = "editor-rows"
                w["has_replacements"] = false; w["reaches_eof"] = true; w["next_offset"] = NSNull()
                w["rows"] = [["line": "1"]]; return encode(w)
            case .suggestReattachment(let expected, let bookmark, let sequence, let subject, let maxWork, let maxEdits):
                comparisonRequests += 1
                guard expected == revision, subject == pane,
                      let mark = marks.first, mark.id == bookmark, mark.sequence == sequence else {
                    return error("DESK_STALE_BOOKMARK")
                }
                return encode(proposal(maxWork: maxWork, maxEdits: maxEdits))
            case .reattachRange(let expected, let attempt, let bookmark, let sequence, let subject,
                let targetFile, let targetRevision, let start, let end):
                reattachmentRequests += 1
                guard admit(expected, attempt) else { return error("DESK_STALE_ATTEMPT") }
                if rejectMutation { rejectMutation = false; return error("DESK_CANCELED") }
                guard subject == pane, targetFile == file, targetRevision == 1,
                      let index = marks.firstIndex(where: { $0.id == bookmark }),
                      marks[index].sequence == sequence, sequence < UInt64.max,
                      let old = marks[index].selection, old.0 < old.1,
                      start < end, end <= UInt64(source.utf8.count) else { return error("DESK_INVALID_LOCATION") }
                let bytes = Array(source.utf8)
                guard bytes[Int(old.0)..<Int(old.1)].elementsEqual(bytes[Int(start)..<Int(end)]) else {
                    return error("DESK_SELECTION_MISMATCH")
                }
                marks[index].file = file; marks[index].offset = start; marks[index].selection = (start, end)
                marks[index].sequence += 1; revision = attempt
                return noteReply()
            default: return error("DESK_UNSUPPORTED_FIXTURE_COMMAND")
            }
        }
    }
    private func admit(_ expected: UInt64, _ attempt: UInt64) -> Bool {
        guard expected == revision, attempt > lastAttempt else { return false }
        lastAttempt = attempt; return true
    }
    private func proposal(maxWork: UInt64, maxEdits: UInt64) -> [String: Any] {
        let mark = marks[0], old = mark.selection!
        let range = ["start": String(old.0), "end": String(old.1)]
        let text = String(decoding: source.utf8.dropFirst(Int(old.0)), as: UTF8.self)
        let preview: [String: Any] = ["original_range": ["start": String(old.0), "end": String(source.utf8.count)],
            "content_start": "0", "text": text, "text_kind": "logical-captured-text-not-shaped",
            "has_replacements": false, "selection_window_utf8_range": ["start": "0", "end": String(old.1 - old.0)]]
        var w = header("suggest-reattachment")
        w["bookmark"] = String(mark.id); w["update_sequence"] = String(mark.sequence); w["target_pane"] = String(pane)
        w["original_range"] = range; w["target_range"] = range
        if maxWork == 0 { w["target_range"] = NSNull() }
        w["outcome"] = maxWork == 0 ? "unresolved" : mark.file == file ? "unchanged" : "mapped"
        w["comparison_complete"] = maxWork != 0; w["comparison_quality"] = maxWork == 0 ? "work-limit" : "exact"
        w["coordinate_domain"] = "original-bytes-may-split-text"
        w["alignment_semantics"] = "deterministic-not-identity-proof"; w["source_reopened"] = false
        w["work_units"] = String(min(maxWork, 32)); w["max_work"] = String(maxWork)
        w["max_edit_distance"] = String(maxEdits); w["max_source_bytes"] = String(4 * 1024 * 1024)
        w["before_source"] = ["file_id": String(mark.file), "source_revision": "1",
            "bytes": String(source.utf8.count), "label": "retained.rs"]
        w["after_source"] = ["file_id": String(file), "source_revision": "1",
            "bytes": String(source.utf8.count), "label": "retained.rs"]
        w["before_preview"] = preview; w["after_preview"] = preview
        if maxWork == 0 { w["after_preview"] = NSNull() }
        return w
    }
    private func noteReply() -> String? {
        if loseReply { loseReply = false; return nil }
        return encode(state())
    }
    private func header(_ command: String) -> [String: Any] {
        ["schema": "fcb.reading-desk/1", "status": "ok", "command": command, "owner": String(owner),
            "model_revision": String(revision), "last_attempt": String(lastAttempt),
            "additional_source_bytes_read": "0", "native_presented": false]
    }
    private func location(file: UInt64, offset: UInt64, selection: (UInt64, UInt64)?) -> [String: Any] {
        ["file_id": String(file), "source_revision": "1", "offset": String(offset),
            "selection": selection.map { ["start": String($0.0), "end": String($0.1)] } as Any? ?? NSNull()]
    }
    private func state() -> [String: Any] {
        var w = header("state")
        w["persistence"] = "explicit-checkpoint"; w["autosave"] = false
        w["retained_sources"] = "1"; w["retained_source_bytes"] = String(source.utf8.count)
        w["active_pane"] = String(pane); w["can_go_back"] = false; w["can_go_forward"] = false
        var p = location(file: file, offset: offset, selection: selection)
        p["pane"] = String(pane); p["label"] = "retained.rs"; p["pinned"] = false; w["panes"] = [p]
        w["bookmarks"] = marks.map { mark in
            var b = location(file: mark.file, offset: mark.offset, selection: mark.selection)
            b["bookmark"] = String(mark.id); b["label"] = mark.label; b["body"] = mark.body
            b["update_sequence"] = String(mark.sequence); return b
        }
        return w
    }
    private func error(_ code: String) -> String {
        encode(["schema": "fcb.reading-desk/1", "status": "error", "owner": String(owner), "error": ["code": code]])
    }
    private func encode(_ object: [String: Any]) -> String {
        String(decoding: try! JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]), as: UTF8.self)
    }
}

@main struct AtlasDeskNoteTests {
    @MainActor static func idle(_ model: AtlasReadingDeskModel) async {
        for _ in 0..<1000 {
            if !model.busy { return }; try? await Task.sleep(nanoseconds: 1_000_000)
        }
        fatalError("note desk operation did not drain")
    }
    @MainActor static func entered(_ gate: DispatchSemaphore) async {
        for _ in 0..<1000 {
            if gate.wait(timeout: .now()) == .success { return }; try? await Task.sleep(nanoseconds: 1_000_000)
        }
        fatalError("note operation did not enter the fixture")
    }
    @MainActor static func started(_ fixture: NoteFixture) async -> AtlasReadingDeskModel {
        let model = AtlasReadingDeskModel(transport: fixture.transport)
        model.start(); await idle(model); precondition(model.state?.active != nil); return model
    }
    @MainActor static func newDraft(_ model: AtlasReadingDeskModel) -> AtlasDeskNoteDraft {
        let draft = AtlasDeskNoteDraft(state: model.state!, pane: model.state!.active!, label: "Why this matters")
        draft.body = "First line\nNUL: \u{0}\nOriginal spelling: e\u{301} and 🦀"
        return draft
    }
    @MainActor static func main() async throws {
        try wireAndReceiptControls()
        await createEditRecall()
        await lostOutcomes()
        await staleAndChangedDrafts()
        await draftCloseProtection()
        await canceledDelivery()
        await reattachmentOutcomes()
        await reattachmentGuardsAndCancellation()
        try await guidedReattachmentOutcomes()
        try await guidedReattachmentGuardsAndCancellation()
        try guidedReattachmentWire()
        print("Desk note decoding, exact anchors, edit sequences, preserved drafts, lost replies and cancellation source fixtures passed")
    }

    @MainActor static func createEditRecall() async {
        let fixture = NoteFixture(), model = await started(fixture)
        let original = model.state!.active!.location, draft = newDraft(model), body = draft.body
        let created = await draft.save(in: model)
        precondition(created && fixture.acceptedCount == 1)
        precondition(!draft.hasUnsavedChanges, "accepted text is no longer an unsubmitted draft")
        let mark = model.state!.bookmarks[0]
        precondition(mark.location == original && mark.updateSequence == 1 && mark.body.utf8.elementsEqual(body.utf8))
        precondition(!draft.target.isNew, "an accepted create must become an edit, not another create")
        let edit = AtlasDeskNoteDraft(state: model.state!, bookmark: mark)
        edit.label = "Revised reason"; edit.body += "\nA later personal observation."
        fixture.failNextPage()
        let edited = await edit.save(in: model)
        precondition(edited && model.page == nil && model.state?.bookmarks.count == 1)
        precondition(model.state?.bookmarks[0].id == mark.id && model.state?.bookmarks[0].updateSequence == 2)
        precondition(model.state?.bookmarks[0].location == original, "editing personal text cannot move its original source anchor")
        precondition(model.notice.contains("source page is unavailable"), "a page failure must not erase a verified write")
        model.apply(kind: 1, subject: 1, start: 10); await idle(model)
        model.apply(kind: 4, subject: mark.id); await idle(model)
        precondition(model.state?.active?.location == original)
        model.bookmarkLabel = "A bookmark without note text"; model.bookmark(); await idle(model)
        precondition(model.state?.bookmarks[1].body == "" && model.state?.bookmarks[1].updateSequence == 1)
        let clear = AtlasDeskNoteDraft(state: model.state!, bookmark: model.state!.bookmarks[0])
        clear.body = ""
        let cleared = await clear.save(in: model)
        precondition(cleared && model.state?.bookmarks[0].id == mark.id && model.state?.bookmarks[0].body == "")
        model.apply(kind: 5, subject: mark.id); await idle(model)
        precondition(model.state?.bookmarks.contains(where: { $0.id == mark.id }) == false)
        model.close()

        let imported = NoteFixture(); imported.seedBookmark(label: "Imported\u{0}label", body: "Keep\u{0}all text")
        let importedModel = await started(imported)
        let importedDraft = AtlasDeskNoteDraft(state: importedModel.state!, bookmark: importedModel.state!.bookmarks[0])
        precondition(!importedDraft.canSave(in: importedModel) && importedDraft.label.utf8.contains(0))
        let refused = await importedDraft.save(in: importedModel)
        precondition(!refused && imported.requestCount == 0 && importedDraft.body == "Keep\u{0}all text")
        importedDraft.label = "Explicitly relabeled"
        let relabeled = await importedDraft.save(in: importedModel)
        precondition(relabeled && importedModel.state?.bookmarks[0].body == "Keep\u{0}all text")
        importedModel.close()
    }

    @MainActor static func lostOutcomes() async {
        for lostState in [false, true] {
            let fixture = NoteFixture(), model = await started(fixture), draft = newDraft(model)
            let originalBody = draft.body
            fixture.loseNextReply(stateFailures: lostState ? 1 : 0)
            let saved = await draft.save(in: model)
            precondition(fixture.requestCount == 1 && fixture.acceptedCount == 1)
            if lostState {
                precondition(!saved && draft.pending != nil && model.state == nil)
                precondition(draft.hasUnsavedChanges && model.hasUnverifiedChanges && model.closeSnapshot.requiresReview)
                precondition(draft.body.utf8.elementsEqual(originalBody.utf8) && !draft.canSave(in: model))
                let retry = await draft.save(in: model)
                precondition(!retry && fixture.requestCount == 1, "unconfirmed notes must never replay a create")
                let reads = fixture.stateCount
                let checked = await draft.checkSavedState(in: model)
                precondition(checked && fixture.stateCount == reads + 1 && fixture.requestCount == 1)
                precondition(draft.pending == nil && model.state?.bookmarks.count == 1)
            } else { precondition(saved && model.state?.bookmarks.count == 1) }
            model.close()
        }
        let rejected = NoteFixture(), model = await started(rejected), draft = newDraft(model)
        rejected.rejectNextNote()
        let saved = await draft.save(in: model)
        precondition(!saved && draft.pending == nil && rejected.acceptedCount == 0 && draft.canSave(in: model))
        let explicitRetry = await draft.save(in: model)
        precondition(explicitRetry && rejected.requestCount == 2 && rejected.acceptedCount == 1)
        model.close()
    }

    @MainActor static func staleAndChangedDrafts() async {
        for replacement in [false, true] {
            let fixture = NoteFixture(), model = await started(fixture), draft = newDraft(model)
            let body = draft.body
            if replacement { fixture.replaceDesk() } else { fixture.movePane() }
            model.reload(); await idle(model)
            let saved = await draft.save(in: model)
            precondition(!saved && fixture.requestCount == 0 && draft.body.utf8.elementsEqual(body.utf8))
            precondition(draft.message.contains("original source range"))
            model.close()
        }
        let fixture = NoteFixture(); fixture.seedBookmark(body: "Original note")
        let model = await started(fixture), draft = AtlasDeskNoteDraft(state: model.state!, bookmark: model.state!.bookmarks[0])
        draft.body = "My unfinished edit"
        fixture.changeNoteElsewhere(); model.reload(); await idle(model)
        let staleEdit = await draft.save(in: model)
        precondition(!staleEdit && fixture.requestCount == 0 && draft.body == "My unfinished edit")
        model.close()

        let changing = NoteFixture(), changingModel = await started(changing), changingDraft = newDraft(changingModel)
        let gate = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
        changing.gateNextNote(entered: gate, resume: resume)
        let first = Task { await changingDraft.save(in: changingModel) }
        await entered(gate)
        changingDraft.body = "A newer draft arrived during delivery"
        resume.signal()
        let dismissed = await first.value
        precondition(!dismissed && !changingDraft.target.isNew && changingDraft.body == "A newer draft arrived during delivery")
        precondition(changingDraft.hasUnsavedChanges, "accepting an older submission cannot clear a newer draft")
        let second = await changingDraft.save(in: changingModel)
        precondition(second && changingModel.state?.bookmarks.count == 1 && changingModel.state?.bookmarks[0].updateSequence == 2)
        precondition(!changingDraft.hasUnsavedChanges)
        changingModel.close()
    }

    @MainActor static func draftCloseProtection() async {
        let fixture = NoteFixture(); fixture.seedBookmark(label: "Existing label", body: "e\u{301}")
        let model = await started(fixture)
        let draft = AtlasDeskNoteDraft(state: model.state!, bookmark: model.state!.bookmarks[0])
        model.registerNoteDraft(draft.id) { [weak draft] in draft?.closeProtectionRevision }
        precondition(model.hasOpenNoteEditor && model.closeSnapshot.drafts.isEmpty && !draft.hasUnsavedChanges,
            "opening an unchanged note is not a new unsaved edit")
        draft.body = "é"
        precondition(draft.hasUnsavedChanges && model.closeSnapshot.drafts.count == 1,
            "canonical-equivalent but byte-distinct personal text is still an edit")
        let reviewed = model.closeSnapshot
        draft.body += " A later thought."
        precondition(model.closeSnapshot != reviewed,
            "live editor changes invalidate close approval without changing the desk revision")
        draft.body = "e\u{301}"
        precondition(!draft.hasUnsavedChanges && model.closeSnapshot.drafts.isEmpty)
        draft.label = "A new reason"
        precondition(draft.hasUnsavedChanges && model.closeSnapshot.drafts.count == 1)
        let accepted = await draft.save(in: model)
        precondition(accepted && !draft.hasUnsavedChanges && model.closeSnapshot.drafts.isEmpty)
        precondition(model.hasUnsavedContents, "keeping a draft in the desk still requires a checkpoint")
        model.unregisterNoteDraft(draft.id)
        precondition(!model.hasOpenNoteEditor)

        let blank = AtlasDeskNoteDraft(state: model.state!, pane: model.state!.active!)
        precondition(!blank.hasUnsavedChanges)
        model.registerNoteDraft(blank.id) { [weak blank] in blank?.closeProtectionRevision }
        blank.body = "An unfinished thought without a label"
        precondition(!blank.canSave(in: model) && blank.hasUnsavedChanges && model.closeSnapshot.drafts.count == 1,
            "validation failure must not hide unique draft text from close protection")
        model.unregisterNoteDraft(blank.id)
        model.close()
    }

    @MainActor static func canceledDelivery() async {
        let queued = NoteFixture(), queuedModel = await started(queued), queuedDraft = newDraft(queuedModel)
        let queuedSave = Task { await queuedDraft.save(in: queuedModel) }; queuedSave.cancel()
        let queuedResult = await queuedSave.value
        precondition(!queuedResult && queued.requestCount == 0 && !queuedDraft.body.isEmpty)
        queuedModel.close()
        for closing in [false, true] {
            let fixture = NoteFixture(), model = await started(fixture), draft = newDraft(model)
            let gate = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
            fixture.loseNextReply(); fixture.gateNextNote(entered: gate, resume: resume)
            let save = Task { await draft.save(in: model) }
            await entered(gate)
            if closing { model.close() } else { save.cancel(); model.cancel() }
            precondition(fixture.closeCount == 0, "the desk handle must survive its entered note mutation")
            resume.signal()
            let saved = await save.value
            precondition(saved && fixture.acceptedCount == 1 && fixture.requestCount == 1)
            if closing {
                precondition(model.state == nil && model.closed)
                for _ in 0..<1000 {
                    if fixture.closeCount == 1 { break }; try? await Task.sleep(nanoseconds: 1_000_000)
                }
                precondition(fixture.closeCount == 1)
            } else { precondition(model.state?.bookmarks.count == 1); model.close() }
        }
    }


    @MainActor static func reattachmentOutcomes() async {
        for lostReads in [-1, 0, 1] {
            let fixture = NoteFixture()
            fixture.seedBookmark(label: "Café", body: "Original e\u{301}\u{0} note")
            fixture.selectOtherCapture()
            let model = await started(fixture), before = model.state!.bookmarks[0]
            let target = AtlasDeskReattachment(state: model.state!, bookmark: before, pane: model.state!.active!)!
            if lostReads >= 0 { fixture.loseNextReply(stateFailures: lostReads) }
            if lostReads == -1 { fixture.failNextPage() }
            model.reattachBookmark(target); await idle(model)
            precondition(fixture.reattachmentCount == 1, "a lost reply must never replay reattachment")
            if lostReads == 1 {
                precondition(model.state == nil && model.notice.contains("could not be verified"))
                model.reattachBookmark(target)
                precondition(fixture.reattachmentCount == 1, "an unverified state cannot authorize a retry")
                model.reload(); await idle(model)
            } else {
                precondition(model.notice.contains("Reattached the saved note"))
                if lostReads == -1 {
                    precondition(model.page == nil && model.notice.contains("source page is unavailable"))
                }
            }
            let saved = model.state!.bookmarks[0]
            precondition(saved.id == before.id && saved.updateSequence == before.updateSequence + 1)
            precondition(saved.location == target.pane.location && saved.location != before.location)
            precondition(saved.label.utf8.elementsEqual(before.label.utf8) && saved.body.utf8.elementsEqual(before.body.utf8))
            precondition(!target.isCurrent(in: model.state))
            precondition(AtlasDeskReattachment(state: model.state!, bookmark: saved, pane: model.state!.active!) == nil)
            model.apply(kind: 4, subject: saved.id); await idle(model)
            precondition(model.state?.active?.location == target.pane.location, "recall uses the accepted destination")
            model.close()
        }
        let changed = NoteFixture(); changed.seedBookmark(body: "Do not move me")
        changed.selectOtherCapture(start: 3, end: 8)
        let model = await started(changed), original = model.state!.bookmarks[0]
        let target = AtlasDeskReattachment(state: model.state!, bookmark: original, pane: model.state!.active!)!
        model.reattachBookmark(target); await idle(model)
        precondition(changed.reattachmentCount == 1 && model.notice.contains("not accepted"))
        precondition(model.state?.bookmarks[0].location == original.location && model.state?.bookmarks[0].updateSequence == 1)
        model.close()
    }

    @MainActor static func reattachmentGuardsAndCancellation() async {
        for changedNote in [false, true] {
            let fixture = NoteFixture(); fixture.seedBookmark(body: "Preserve original")
            fixture.selectOtherCapture()
            let model = await started(fixture)
            let target = AtlasDeskReattachment(state: model.state!, bookmark: model.state!.bookmarks[0], pane: model.state!.active!)!
            if changedNote { fixture.changeNoteElsewhere() } else { fixture.movePane() }
            model.reload(); await idle(model)
            model.reattachBookmark(target); await idle(model)
            precondition(fixture.reattachmentCount == 0 && model.notice.contains("changed after confirmation"))
            model.close()
        }
        let fixture = NoteFixture(); fixture.seedBookmark(body: "Canceled delivery")
        fixture.selectOtherCapture()
        let model = await started(fixture)
        let target = AtlasDeskReattachment(state: model.state!, bookmark: model.state!.bookmarks[0], pane: model.state!.active!)!
        let gate = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
        fixture.loseNextReply(); fixture.gateNextNote(entered: gate, resume: resume)
        model.reattachBookmark(target); await entered(gate); model.cancel(); resume.signal(); await idle(model)
        precondition(fixture.reattachmentCount == 1 && model.state?.bookmarks[0].location == target.pane.location)
        precondition(model.notice.contains("Reattached the saved note"), "entered cancellation must reconcile the actual accepted mapping")
        model.close()

        for sequence in [UInt64(1), UInt64.max] {
            let invalid = NoteFixture(); invalid.seedBookmark(sequence: sequence)
            invalid.selectOtherCapture(start: 2, end: sequence == 1 ? 2 : 7)
            let desk = await started(invalid)
            precondition(AtlasDeskReattachment(state: desk.state!, bookmark: desk.state!.bookmarks[0], pane: desk.state!.active!) == nil)
            precondition(invalid.reattachmentCount == 0); desk.close()
        }
    }

    static func wireAndReceiptControls() throws {
        let fixture = NoteFixture(); fixture.seedBookmark()
        let json = fixture.stateJSON
        let original = try AtlasDeskState.decode(json, handle: fixture.owner)
        precondition(original.bookmarks[0].body.isEmpty && original.bookmarks[0].updateSequence == 1)
        let base = try JSONSerialization.jsonObject(with: Data(json.utf8)) as! [String: Any]
        let mark = (base["bookmarks"] as! [[String: Any]])[0]
        func encoded(_ object: [String: Any]) -> String {
            String(decoding: try! JSONSerialization.data(withJSONObject: object), as: UTF8.self)
        }
        func rejects(_ changed: [String: Any]) {
            var object = base; object["bookmarks"] = [changed]
            do { _ = try AtlasDeskState.decode(encoded(object), handle: fixture.owner); fatalError("invalid note state accepted") }
            catch { }
        }
        var invalid = mark; invalid.removeValue(forKey: "body"); rejects(invalid)
        invalid = mark; invalid["body"] = NSNull(); rejects(invalid)
        invalid = mark; invalid["body"] = String(repeating: "é", count: 8193); rejects(invalid)
        for sequence in ["0", "01", "18446744073709551616", 1, NSNull()] as [Any] {
            invalid = mark; invalid["update_sequence"] = sequence; rejects(invalid)
        }
        invalid = mark; invalid.removeValue(forKey: "update_sequence"); rejects(invalid)
        var aggregate = base
        aggregate["bookmarks"] = (1...32).map { id -> [String: Any] in
            var note = mark; note["bookmark"] = String(id); note["body"] = String(repeating: "\u{0}", count: 16_384); return note
        }
        let maximum = encoded(aggregate)
        precondition(maximum.utf8.count < AtlasDeskWire.maximumResponseBytes)
        let admitted = try AtlasDeskState.decode(maximum, handle: fixture.owner)
        precondition(admitted.bookmarks.count == 32 && admitted.bookmarks.allSatisfy { $0.body.utf8.count == 16_384 })
        precondition(AtlasDeskNoteTarget.validText(label: "Label", body: String(repeating: "é", count: 8192)))
        precondition(!AtlasDeskNoteTarget.validText(label: "Label", body: String(repeating: "é", count: 8193)))
        let target = AtlasDeskNoteTarget(state: original, bookmark: original.bookmarks[0])
        let composed = "é", decomposed = "e\u{301}"
        precondition(composed == decomposed && !composed.utf8.elementsEqual(decomposed.utf8))
        let submitted = try AtlasDeskNoteSubmission(target: target, state: original, attempt: 2, label: decomposed, body: decomposed)
        var receipt = base, result = mark
        result["label"] = decomposed; result["body"] = decomposed; result["update_sequence"] = "2"
        receipt["model_revision"] = "2"; receipt["last_attempt"] = "2"; receipt["bookmarks"] = [result]
        let accepted = try AtlasDeskState.decode(encoded(receipt), handle: fixture.owner)
        guard case .accepted = submitted.outcome(in: accepted) else { fatalError("exact note receipt refused") }
        for field in ["label", "body"] {
            var changed = result; changed[field] = composed; receipt["bookmarks"] = [changed]
            let state = try AtlasDeskState.decode(encoded(receipt), handle: fixture.owner)
            guard case .unconfirmed = submitted.outcome(in: state) else { fatalError("normalized note receipt silently accepted") }
        }
        receipt["bookmarks"] = [result]; receipt["model_revision"] = "3"; receipt["last_attempt"] = "3"
        let later = try AtlasDeskState.decode(encoded(receipt), handle: fixture.owner)
        guard case .unconfirmed = submitted.outcome(in: later) else { fatalError("a newer desk revision laundered an older note receipt") }
    }
}


extension AtlasDeskNoteTests {
    @MainActor static func guidedReattachmentOutcomes() async throws {
        for lostReads in [-1, 0, 1] {
            let fixture = NoteFixture(); fixture.seedBookmark(label: "Café", body: "Original e\u{301}\u{0} text")
            fixture.selectOtherCapture(); fixture.movePane()
            let model = await started(fixture), state = model.state!, mark = state.bookmarks[0], pane = state.active!
            precondition(pane.location.selection == nil)
            let target = AtlasDeskComparisonTarget(state: state, bookmark: mark, pane: pane)!
            let proposal = try await model.suggestReattachment(target)
            precondition(proposal.outcome == .mapped && fixture.comparisonCount == 1 && fixture.reattachmentCount == 0)
            precondition(model.state?.revision == state.revision && model.state?.bookmarks[0].location == mark.location)
            let confirmed = AtlasDeskReattachment(proposal: proposal)!
            precondition(confirmed.destination.selection == mark.location.selection)
            if lostReads >= 0 { fixture.loseNextReply(stateFailures: lostReads) }
            model.reattachBookmark(confirmed); await idle(model)
            precondition(fixture.reattachmentCount == 1, "confirmation is sent once, including after a lost reply")
            if lostReads == 1 {
                precondition(model.state == nil && model.notice.contains("could not be verified"))
                model.reattachBookmark(confirmed); precondition(fixture.reattachmentCount == 1)
                model.reload(); await idle(model)
            } else { precondition(model.notice.contains("Reattached the saved note")) }
            let saved = model.state!.bookmarks[0]
            precondition(saved.id == mark.id && saved.updateSequence == mark.updateSequence + 1)
            precondition(saved.location == confirmed.destination && saved.location != mark.location)
            precondition(saved.label.utf8.elementsEqual(mark.label.utf8) && saved.body.utf8.elementsEqual(mark.body.utf8))
            precondition(model.state?.active?.location == pane.location, "guided confirmation does not select or move a pane")
            precondition(!confirmed.isCurrent(in: model.state)); model.close()
        }

        // A proposal is not mutation authority: the confirmed command still
        // checks the actual original bytes, even after a forged matching range.
        let fixture = NoteFixture(); fixture.seedBookmark(body: "Keep my evidence")
        fixture.selectOtherCapture(); fixture.movePane()
        let model = await started(fixture), state = model.state!, mark = state.bookmarks[0]
        let target = AtlasDeskComparisonTarget(state: state, bookmark: mark, pane: state.active!)!
        var wire = try JSONSerialization.jsonObject(with: Data(fixture.proposalJSON.utf8)) as! [String: Any]
        wire["target_range"] = ["start": "3", "end": "8"]
        var preview = wire["after_preview"] as! [String: Any]
        preview["original_range"] = ["start": "3", "end": String(fixture.source.utf8.count)]
        preview["text"] = String(decoding: fixture.source.utf8.dropFirst(3), as: UTF8.self)
        wire["after_preview"] = preview
        let json = String(decoding: try JSONSerialization.data(withJSONObject: wire), as: UTF8.self)
        let altered = try AtlasDeskReattachmentProposal.decode(json, target: target)
        model.reattachBookmark(AtlasDeskReattachment(proposal: altered)!); await idle(model)
        precondition(model.notice.contains("not accepted") && fixture.reattachmentCount == 1)
        precondition(model.state?.bookmarks[0].location == mark.location && model.state?.bookmarks[0].updateSequence == 1)
        precondition(model.state?.active?.location == state.active?.location); model.close()
    }

    @MainActor static func guidedReattachmentGuardsAndCancellation() async throws {
        let stale = NoteFixture(); stale.seedBookmark(body: "Original note"); stale.selectOtherCapture()
        let staleModel = await started(stale), state = staleModel.state!
        let target = AtlasDeskComparisonTarget(state: state, bookmark: state.bookmarks[0], pane: state.active!)!
        let proposal = try await staleModel.suggestReattachment(target), confirmation = AtlasDeskReattachment(proposal: proposal)!
        stale.changeNoteElsewhere(); staleModel.reload(); await idle(staleModel)
        precondition(!target.isCurrent(in: staleModel.state))
        staleModel.reattachBookmark(confirmation); await idle(staleModel)
        precondition(stale.reattachmentCount == 0)
        do { _ = try await staleModel.suggestReattachment(target); fatalError("stale comparison was sent") } catch { }
        precondition(stale.comparisonCount == 1); staleModel.close()

        let fixture = NoteFixture(); fixture.seedBookmark(body: "Cancel comparison"); fixture.selectOtherCapture()
        let model = await started(fixture), before = model.state!
        let selected = AtlasDeskComparisonTarget(state: before, bookmark: before.bookmarks[0], pane: before.active!)!
        let gate = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
        fixture.gateNextNote(entered: gate, resume: resume)
        let comparison = Task { try await model.suggestReattachment(selected) }
        await entered(gate); comparison.cancel(); resume.signal()
        do { _ = try await comparison.value; fatalError("canceled proposal accepted") } catch { }
        precondition(!model.busy && fixture.reattachmentCount == 0)
        precondition(model.state?.revision == before.revision && model.state?.bookmarks[0].location == before.bookmarks[0].location)

        let acceptedProposal = try await model.suggestReattachment(selected)
        let confirmed = AtlasDeskReattachment(proposal: acceptedProposal)!
        let commitGate = DispatchSemaphore(value: 0), commitResume = DispatchSemaphore(value: 0)
        fixture.loseNextReply(); fixture.gateNextNote(entered: commitGate, resume: commitResume)
        model.reattachBookmark(confirmed); await entered(commitGate); model.cancel(); commitResume.signal(); await idle(model)
        precondition(fixture.reattachmentCount == 1 && model.state?.bookmarks[0].location == confirmed.destination)
        precondition(model.state?.active?.location == before.active?.location)
        model.close()
    }

    static func guidedReattachmentWire() throws {
        let fixture = NoteFixture(); fixture.seedBookmark(); fixture.selectOtherCapture(); fixture.movePane()
        let state = try AtlasDeskState.decode(fixture.stateJSON, handle: fixture.owner)
        let target = AtlasDeskComparisonTarget(state: state, bookmark: state.bookmarks[0], pane: state.active!)!
        let base = try JSONSerialization.jsonObject(with: Data(fixture.proposalJSON.utf8)) as! [String: Any]
        func encoded(_ value: [String: Any]) -> String {
            String(decoding: try! JSONSerialization.data(withJSONObject: value), as: UTF8.self)
        }
        func rejects(_ value: [String: Any]) {
            do { _ = try AtlasDeskReattachmentProposal.decode(encoded(value), target: target); fatalError("invalid comparison accepted") }
            catch { }
        }
        let valid = try AtlasDeskReattachmentProposal.decode(encoded(base), target: target)
        precondition(valid.outcome == .mapped && AtlasDeskReattachment(proposal: valid) != nil)
        for (field, value) in [
            ("owner", "999"), ("model_revision", "999"), ("last_attempt", "999"),
            ("bookmark", "999"), ("update_sequence", "2"), ("target_pane", "999"),
            ("max_work", "1"), ("work_units", String(target.maxWork + 1)),
            ("max_edit_distance", "257"), ("max_source_bytes", "1"),
            ("additional_source_bytes_read", "1"), ("comparison_quality", "guessed"),
            ("alignment_semantics", "identity-proof"), ("coordinate_domain", "decoded-utf8"),
        ] {
            var changed = base; changed[field] = value; rejects(changed)
        }
        for key in ["source_reopened", "native_presented"] {
            var changed = base; changed[key] = true; rejects(changed)
        }
        for key in ["target_range", "after_preview", "before_preview"] {
            var changed = base; changed.removeValue(forKey: key); rejects(changed)
        }
        var changed = base; changed["comparison_complete"] = false; rejects(changed)
        changed = base; changed["outcome"] = "unresolved"; rejects(changed)
        changed = base; changed["target_range"] = ["start": "3", "end": "7"]; rejects(changed)
        changed = base; changed["target_range"] = ["start": "11", "end": "16"]; rejects(changed)
        for (side, field, value) in [("before_source", "file_id", "999"), ("after_source", "source_revision", "999"),
            ("after_source", "bytes", "4"), ("after_source", "label", "another.rs")] {
            changed = base; var source = changed[side] as! [String: Any]; source[field] = value
            changed[side] = source; rejects(changed)
        }
        var preview = base["before_preview"] as! [String: Any]
        preview["selection_window_utf8_range"] = ["start": "0", "end": "1000"]
        changed = base; changed["before_preview"] = preview; rejects(changed)
        preview = base["before_preview"] as! [String: Any]; preview["content_start"] = "1"
        changed = base; changed["before_preview"] = preview; rejects(changed)
        preview = base["before_preview"] as! [String: Any]
        preview["text"] = "éxxx"; preview["selection_window_utf8_range"] = ["start": "1", "end": "5"]
        changed = base; changed["before_preview"] = preview; rejects(changed)

        changed = base; changed["comparison_quality"] = "edit-limit"; changed["comparison_complete"] = false
        let partial = try AtlasDeskReattachmentProposal.decode(encoded(changed), target: target)
        precondition(!partial.complete && AtlasDeskReattachment(proposal: partial) != nil,
            "a verified equal span remains usable when unrelated spans exhaust the budget")
        changed["outcome"] = "unresolved"; changed["target_range"] = NSNull(); changed["after_preview"] = NSNull()
        let unresolved = try AtlasDeskReattachmentProposal.decode(encoded(changed), target: target)
        precondition(unresolved.outcome == .unresolved && AtlasDeskReattachment(proposal: unresolved) == nil)
        changed["comparison_quality"] = "exact"; changed["comparison_complete"] = true; rejects(changed)
        changed["outcome"] = "changed"
        let edited = try AtlasDeskReattachmentProposal.decode(encoded(changed), target: target)
        precondition(edited.outcome == .changed && AtlasDeskReattachment(proposal: edited) == nil)

        let unchangedFixture = NoteFixture(); unchangedFixture.seedBookmark()
        let unchangedState = try AtlasDeskState.decode(unchangedFixture.stateJSON, handle: unchangedFixture.owner)
        let unchangedTarget = AtlasDeskComparisonTarget(state: unchangedState, bookmark: unchangedState.bookmarks[0], pane: unchangedState.active!)!
        let unchanged = try AtlasDeskReattachmentProposal.decode(unchangedFixture.proposalJSON, target: unchangedTarget)
        precondition(unchanged.outcome == .unchanged && AtlasDeskReattachment(proposal: unchanged) == nil)

        // A BOM is original evidence but has no logical display characters.
        // The engine explicitly reports its content boundary, and a preview
        // may be an empty EOF window without changing the selected byte range.
        var bomState = try JSONSerialization.jsonObject(with: Data(fixture.stateJSON.utf8)) as! [String: Any]
        var marks = bomState["bookmarks"] as! [[String: Any]]
        marks[0]["selection"] = ["start": "0", "end": "2"]; marks[0]["offset"] = "0"; bomState["bookmarks"] = marks
        var panes = bomState["panes"] as! [[String: Any]]
        panes[0]["offset"] = "2"; panes[0]["selection"] = NSNull(); bomState["panes"] = panes
        let decodedBOM = try AtlasDeskState.decode(encoded(bomState), handle: fixture.owner)
        let bomTarget = AtlasDeskComparisonTarget(state: decodedBOM, bookmark: decodedBOM.bookmarks[0], pane: decodedBOM.active!)!
        changed = base; changed["original_range"] = ["start": "0", "end": "2"]; changed["target_range"] = ["start": "0", "end": "2"]
        for side in ["before_source", "after_source"] {
            var source = changed[side] as! [String: Any]; source["bytes"] = "2"; changed[side] = source
        }
        let bomPreview: [String: Any] = ["original_range": ["start": "2", "end": "2"], "content_start": "2",
            "text": "", "text_kind": "logical-captured-text-not-shaped", "has_replacements": false,
            "selection_window_utf8_range": NSNull()]
        changed["before_preview"] = bomPreview; changed["after_preview"] = bomPreview
        let bom = try AtlasDeskReattachmentProposal.decode(encoded(changed), target: bomTarget)
        precondition(bom.targetRange?.start == 0 && bom.targetRange?.end == 2 && bom.beforePreview.text.isEmpty)
    }
}
