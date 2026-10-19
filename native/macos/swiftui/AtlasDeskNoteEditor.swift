import SwiftUI
import AppKit

@MainActor struct AtlasDeskNoteEditor: View {
    @Bindable var model: AtlasReadingDeskModel
    @Bindable var draft: AtlasDeskNoteDraft
    @Environment(\.dismiss) private var dismiss
    @State private var command: Task<Void, Never>?
    @State private var copyNotice = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(draft.target.isNew ? "New source note" : "Edit bookmark and note")
                .font(.title2.weight(.semibold))
            VStack(alignment: .leading, spacing: 3) {
                Text("Captured source \(draft.target.location.file) · revision \(draft.target.location.revision)")
                if let range = draft.target.location.selection {
                    Text("Exact original bytes \(range.start)..<\(range.end)")
                } else {
                    Text("Exact original byte \(draft.target.location.offset)")
                }
            }.font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            HStack {
                Text("Location label").font(.headline)
                Spacer()
                Text("\(draft.label.utf8.count) / 256 UTF-8 bytes").font(.caption).foregroundStyle(.secondary)
            }
            TextField("Why keep this location?", text: $draft.label)
                .disabled(draft.saving || draft.pending != nil)
            HStack {
                Text("Personal note").font(.headline)
                Spacer()
                Text("\(draft.body.utf8.count) / 16,384 UTF-8 bytes").font(.caption).foregroundStyle(.secondary)
            }
            TextEditor(text: $draft.body).font(.body)
                .frame(minHeight: 220, idealHeight: 320)
                .accessibilityLabel("Personal note text")
                .disabled(draft.saving || draft.pending != nil)
            if !draft.message.isEmpty {
                Text(draft.message).font(.callout).textSelection(.enabled)
            } else if let problem = draft.validation(in: model) {
                Text(problem).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            }
            if !copyNotice.isEmpty { Text(copyNotice).font(.caption).foregroundStyle(.secondary) }
            Text("The note stays attached to these captured bytes. Keep it in the desk, then save a new checkpoint to retain it after closing the app. Checkpoints include full note text and retained source bytes, unencrypted.")
                .font(.caption).foregroundStyle(.secondary)
            HStack {
                Button("Discard draft") { dismiss() }
                    .keyboardShortcut(.cancelAction).disabled(command != nil || draft.saving)
                Button("Copy note text") {
                    let board = NSPasteboard.general
                    board.clearContents()
                    copyNotice = board.setString(draft.body, forType: .string)
                        ? "Copied the full draft text." : "The clipboard did not accept the draft text. It remains in this editor."
                }.disabled(draft.body.isEmpty)
                .help("Keep a copy of the full draft, including text whose save outcome is still unconfirmed.")
                Spacer()
                if draft.saving {
                    ProgressView().controlSize(.small)
                    Button("Cancel request", action: model.cancel)
                } else if draft.pending != nil {
                    Button("Check saved state") { submit(check: true) }
                        .disabled(!model.canOperate || command != nil)
                } else {
                    Button("Keep note in desk") { submit(check: false) }
                        .keyboardShortcut(.return, modifiers: .command)
                        .disabled(!draft.canSave(in: model) || command != nil)
                }
            }
        }.padding(20).frame(minWidth: 560, idealWidth: 680, minHeight: 560)
        .interactiveDismissDisabled()
        .onDisappear { if draft.saving { model.cancel() } }
    }
    private func submit(check: Bool) {
        guard command == nil else { return }
        command = Task {
            let saved: Bool
            if check { saved = await draft.checkSavedState(in: model) }
            else { saved = await draft.save(in: model) }
            command = nil
            if saved { dismiss() }
        }
    }
}
