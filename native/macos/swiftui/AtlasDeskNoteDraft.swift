import Foundation
import Observation

/// Personal text remains owned by its editor through rejection, stale targets,
/// cancellation and an uncertain delivery. Closing the editor is explicit.
@Observable @MainActor final class AtlasDeskNoteDraft: Identifiable {
    let id = UUID()
    private(set) var target: AtlasDeskNoteTarget
    var label: String
    var body: String
    private(set) var saving = false
    private(set) var pending: AtlasDeskNoteSubmission?
    private(set) var message = ""

    init(state: AtlasDeskState, pane: AtlasDeskPane, label: String = "") {
        target = AtlasDeskNoteTarget(state: state, pane: pane)
        self.label = label; body = ""
    }
    init(state: AtlasDeskState, bookmark: AtlasDeskBookmark) {
        target = AtlasDeskNoteTarget(state: state, bookmark: bookmark)
        label = bookmark.label; body = bookmark.body
    }

    func validation(in model: AtlasReadingDeskModel) -> String? {
        if label.isEmpty { return "Give this location a label." }
        if label.utf8.contains(0) { return "This label contains NUL. Replace those characters in the label before saving; your original note text remains in this draft." }
        if label.utf8.count > 256 { return "The label exceeds 256 UTF-8 bytes." }
        if body.utf8.count > 16_384 { return "The note exceeds 16,384 UTF-8 bytes." }
        if !target.isCurrent(in: model.state) { return AtlasReadingDeskError.staleNote.message }
        if target.isNew && (model.state?.bookmarks.count ?? 32) >= 32 {
            return "This desk already keeps 32 bookmarks and notes. Keep your draft while you choose an existing item to edit or forget."
        }
        if case .edit(_, let sequence) = target.kind, sequence == UInt64.max {
            return "This note has exhausted its update sequence. Its existing text and this draft are preserved."
        }
        return nil
    }
    func canSave(in model: AtlasReadingDeskModel) -> Bool {
        !saving && pending == nil && model.canOperate && validation(in: model) == nil
    }

    @discardableResult func save(in model: AtlasReadingDeskModel) async -> Bool {
        guard !saving, pending == nil, model.canOperate else { return false }
        if let problem = validation(in: model) { message = problem; return false }
        saving = true
        defer { saving = false }
        do {
            return finish(try await model.saveNote(target: target, label: label, body: body), model: model)
        } catch {
            message = (error as? AtlasReadingDeskError)?.message ?? "The note could not be submitted. Your draft is preserved."
            return false
        }
    }
    @discardableResult func checkSavedState(in model: AtlasReadingDeskModel) async -> Bool {
        guard !saving, let pending, model.canOperate else { return false }
        saving = true
        defer { saving = false }
        return finish(await model.resolveNote(pending), model: model)
    }
    private func finish(_ outcome: AtlasDeskNoteOutcome, model: AtlasReadingDeskModel) -> Bool {
        switch outcome {
        case .accepted(let bookmark):
            pending = nil
            target = AtlasDeskNoteTarget(owner: target.owner, bookmark: bookmark)
            // Even a programmatic edit during delivery cannot discard newer
            // draft text. Its next explicit save edits this accepted note.
            guard label.utf8.elementsEqual(bookmark.label.utf8), body.utf8.elementsEqual(bookmark.body.utf8) else {
                message = "The submitted text was saved. Your newer draft is preserved for a further explicit edit."
                return false
            }
            message = model.notice
            return true
        case .notAccepted:
            pending = nil; message = model.notice
            return false
        case .unconfirmed(let submission):
            pending = submission; message = model.notice
            return false
        }
    }
}
