import Foundation
import Observation

/// Draft text survives stale anchors, cancellation and lost mutation replies.
/// Accepted text belongs to the Rust desk and its checkpoint, never this view.
@Observable @MainActor final class AtlasDeskObservedNoteDraft: Identifiable {
    let id = UUID()
    private(set) var target: AtlasDeskObservedTarget
    var label: String {
        didSet { if !label.utf8.elementsEqual(oldValue.utf8) { closeRevision = UUID() } }
    }
    var body: String {
        didSet { if !body.utf8.elementsEqual(oldValue.utf8) { closeRevision = UUID() } }
    }
    private(set) var saving = false
    private(set) var pending: AtlasDeskObservedNoteSubmission?
    private(set) var message = ""
    private var acceptedLabel: String
    private var acceptedBody: String
    private var closeRevision = UUID()

    init(target: AtlasDeskObservedTarget) {
        self.target = target
        label = target.passage.label; body = target.passage.body
        acceptedLabel = target.passage.label; acceptedBody = target.passage.body
    }
    var hasUnsavedChanges: Bool {
        saving || pending != nil || !label.utf8.elementsEqual(acceptedLabel.utf8)
            || !body.utf8.elementsEqual(acceptedBody.utf8)
    }
    var closeProtectionRevision: UUID? { hasUnsavedChanges ? closeRevision : nil }

    func validation(in model: AtlasReadingDeskModel) -> String? {
        if label.isEmpty { return "Give this passage a label." }
        if label.utf8.contains(0) { return "The label contains NUL. Replace it in the label; the original note text remains in this draft." }
        if label.utf8.count > 256 { return "The label exceeds 256 UTF-8 bytes." }
        if body.utf8.count > 16_384 { return "The note exceeds 16,384 UTF-8 bytes." }
        if !target.isCurrent(in: model.state) { return "This saved passage changed. Your draft is preserved; reopen the current passage before editing its note." }
        if target.passage.updateSequence == UInt64.max { return "This note has exhausted its update sequence. Its saved text and this draft remain available." }
        return nil
    }
    func canSave(in model: AtlasReadingDeskModel) -> Bool {
        !saving && pending == nil && model.canOperate && model.supportsObservedPassages && validation(in: model) == nil
    }

    @discardableResult func save(in model: AtlasReadingDeskModel) async -> Bool {
        guard !saving, pending == nil, model.canOperate else { return false }
        if let problem = validation(in: model) { message = problem; return false }
        saving = true; defer { saving = false }
        do {
            return finish(try await model.saveObservedNote(target: target, label: label, body: body), model: model)
        } catch {
            message = (error as? AtlasReadingDeskError)?.message ?? "The note could not be submitted. Your draft is preserved."
            return false
        }
    }
    @discardableResult func checkSavedState(in model: AtlasReadingDeskModel) async -> Bool {
        guard !saving, let pending, model.canOperate else { return false }
        saving = true; defer { saving = false }
        return finish(await model.resolveObservedNote(pending), model: model)
    }
    private func finish(_ outcome: AtlasDeskObservedNoteOutcome, model: AtlasReadingDeskModel) -> Bool {
        closeRevision = UUID()
        switch outcome {
        case .accepted(let accepted):
            pending = nil; target = accepted
            acceptedLabel = accepted.passage.label; acceptedBody = accepted.passage.body
            guard label.utf8.elementsEqual(acceptedLabel.utf8), body.utf8.elementsEqual(acceptedBody.utf8) else {
                message = "The submitted text was saved. Your newer draft is preserved for another explicit edit."
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
