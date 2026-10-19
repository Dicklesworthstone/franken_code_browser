import Foundation

/// Injected boundary for production desk/draft models. This does not qualify
/// the C ABI, AppKit text editing, checkpoint storage or a native Mac run.
private final class NoteFixture: @unchecked Sendable {
    private struct Mark {
        let id: UInt64
        var label: String
        var body: String
        var sequence: UInt64
        let file: UInt64
        let offset: UInt64
        let selection: (UInt64, UInt64)?
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
        switch request { case .annotate, .editBookmark: isNote = true; default: isNote = false }
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
            default: return error("DESK_UNSUPPORTED_FIXTURE_COMMAND")
            }
        }
    }
    private func admit(_ expected: UInt64, _ attempt: UInt64) -> Bool {
        guard expected == revision, attempt > lastAttempt else { return false }
        lastAttempt = attempt; return true
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
        await canceledDelivery()
        print("Desk note decoding, exact anchors, edit sequences, preserved drafts, lost replies and cancellation source fixtures passed")
    }

    @MainActor static func createEditRecall() async {
        let fixture = NoteFixture(), model = await started(fixture)
        let original = model.state!.active!.location, draft = newDraft(model), body = draft.body
        let created = await draft.save(in: model)
        precondition(created && fixture.acceptedCount == 1)
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
        let second = await changingDraft.save(in: changingModel)
        precondition(second && changingModel.state?.bookmarks.count == 1 && changingModel.state?.bookmarks[0].updateSequence == 2)
        changingModel.close()
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
