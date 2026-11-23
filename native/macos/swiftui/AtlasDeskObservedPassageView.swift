import SwiftUI
import AppKit

@MainActor struct AtlasDeskObservedPassageView: View {
    @Bindable var model: AtlasReadingDeskModel
    @Bindable var draft: AtlasDeskObservedNoteDraft
    @Environment(\.dismiss) private var dismiss
    @State private var page: AtlasDeskObservedPage?
    @State private var command: Task<Void, Never>?
    @State private var notice = ""
    @State private var confirmDiscard = false
    @State private var confirmForget = false

    private var currentPage: AtlasDeskObservedPage? {
        guard let page, page.target.passage.matches(draft.target.passage),
              page.target.isCurrent(in: model.state) else { return nil }
        return page
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Saved observed passage").font(.title2.weight(.semibold))
                Spacer()
                Button("Done") {
                    if draft.hasUnsavedChanges { confirmDiscard = true } else { dismiss() }
                }.keyboardShortcut(.cancelAction).disabled(command != nil || draft.saving)
            }
            provenance
            if let page = currentPage {
                ScrollView([.horizontal, .vertical]) {
                    Text(verbatim: page.text).font(.system(size: 12, design: .monospaced))
                        .textSelection(.enabled).fixedSize(horizontal: true, vertical: true)
                        .frame(maxWidth: .infinity, alignment: .topLeading).padding(10)
                }
                .frame(minHeight: 150, idealHeight: 240, maxHeight: 360)
                .background(Color(nsColor: .textBackgroundColor))
                .accessibilityLabel("Saved observed source text")
                if page.containsReplacements {
                    Text("Decoded text contains replacement characters. Copy original hex to preserve the exact selected bytes.")
                        .font(.caption).foregroundStyle(.secondary)
                }
            } else {
                HStack {
                    Text(command == nil ? "The saved passage text is not loaded." : "Reading retained passage…")
                    Spacer()
                    Button("Read saved passage") { load() }
                        .disabled(command != nil || !model.canOperate || !draft.target.isCurrent(in: model.state))
                }.frame(minHeight: 100)
            }
            HStack {
                Menu("Copy passage") {
                    Button("Copy exact text") { copy(.text) }
                    Button("Copy text with location") { copy(.location) }
                    Button("Copy original hex") { copy(.hex) }
                }.disabled(currentPage == nil || command != nil || !model.canOperate)
                Text("Other file ranges remain unknown. This saved passage never reopens its source path.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Divider()
            HStack {
                Text("Passage label").font(.headline)
                Spacer()
                Text("\(draft.label.utf8.count) / 256 UTF-8 bytes").font(.caption).foregroundStyle(.secondary)
            }
            TextField("Why keep this passage?", text: $draft.label)
                .disabled(draft.saving || draft.pending != nil)
            HStack {
                Text("Personal note").font(.headline)
                Spacer()
                Text("\(draft.body.utf8.count) / 16,384 UTF-8 bytes").font(.caption).foregroundStyle(.secondary)
            }
            TextEditor(text: $draft.body).font(.body).frame(minHeight: 120, idealHeight: 170)
                .accessibilityLabel("Observed passage personal note")
                .disabled(draft.saving || draft.pending != nil)
            if !draft.message.isEmpty { Text(draft.message).font(.caption).textSelection(.enabled) }
            else if let problem = draft.validation(in: model) {
                Text(problem).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            }
            if !notice.isEmpty { Text(notice).font(.caption).foregroundStyle(.secondary).textSelection(.enabled) }
            Text("Checkpoints and automatic recovery include this passage, its retained decoding context and accepted note. Source packs include the selected original bytes, readable text when available, and the full note.")
                .font(.caption).foregroundStyle(.secondary)
            HStack {
                Button("Forget passage…", role: .destructive) { confirmForget = true }
                    .disabled(command != nil || !model.canOperate || !draft.target.isCurrent(in: model.state) || draft.pending != nil)
                Button("Copy note text") { publish(draft.body, message: "Copied the full draft note.") }
                    .disabled(draft.body.isEmpty)
                Spacer()
                if draft.saving {
                    ProgressView().controlSize(.small)
                    Button("Cancel request", action: model.cancel)
                } else if draft.pending != nil {
                    Button("Check saved state") { save(check: true) }
                        .disabled(command != nil || !model.canOperate)
                } else {
                    Button("Keep note in desk") { save(check: false) }
                        .keyboardShortcut(.return, modifiers: .command)
                        .disabled(command != nil || !draft.canSave(in: model))
                }
            }
        }
        .padding(20).frame(minWidth: 760, idealWidth: 940, minHeight: 700, idealHeight: 850)
        .interactiveDismissDisabled()
        .onAppear {
            model.registerNoteDraft(draft.id) { [weak draft] in draft?.closeProtectionRevision }
            load()
        }
        .onDisappear {
            model.unregisterNoteDraft(draft.id)
            command?.cancel()
            if draft.saving { model.cancel() }
        }
        .alert("Discard this note draft?", isPresented: $confirmDiscard) {
            Button("Keep editing", role: .cancel) { }
            Button("Discard draft", role: .destructive) { dismiss() }
        } message: {
            Text("The accepted passage remains in the reading desk. Unsaved edits in this editor will be discarded. If a save outcome is unconfirmed, check saved state before closing.")
        }
        .alert("Forget this observed passage?", isPresented: $confirmForget) {
            Button("Cancel", role: .cancel) { }
            Button("Forget passage", role: .destructive) {
                guard model.canOperate, draft.target.isCurrent(in: model.state), draft.pending == nil else { return }
                model.forgetObservedPassage(draft.target); dismiss()
            }
        } message: {
            Text("Remove “\(draft.target.passage.label)” and its saved personal note from this desk? Unsaved edits in this editor will also be discarded. Other passages that share its observed bytes are preserved.")
        }
    }

    private var provenance: some View {
        let observation = draft.target.observation, selected = draft.target.passage.selection
        return VStack(alignment: .leading, spacing: 3) {
            Text(verbatim: observation.path).font(.headline).lineLimit(2)
            Text("Original bytes \(selected.start)..<\(selected.end) · \(selected.count) selected bytes")
            Text("Retained context \(observation.capturedRange.start)..<\(observation.capturedRange.end) · \(observation.encoding) · \(consistency(observation.consistency))")
            Text("Observed file length \(observation.observedLength) bytes\(observation.finalObservedLength.map { " · final observed length \($0)" } ?? "")")
            Text("Extent SHA-256: \(observation.extentSHA256)").font(.system(.caption2, design: .monospaced))
        }.font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
    }
    private func consistency(_ value: String) -> String {
        switch value {
        case "host-supplied-sequence": return "supplied byte sequence"
        case "unchanged-metadata-not-atomic": return "metadata unchanged; observation was not atomic"
        case "changed-during-read": return "file changed while reading"
        case "short-read": return "read ended before the requested range"
        default: return "metadata unavailable"
        }
    }
    private func load() {
        guard command == nil, model.canOperate else { return }
        command = Task {
            do { page = try await model.readObservedPassage(draft.target); notice = "" }
            catch {
                page = nil
                notice = (error as? AtlasReadingDeskError)?.message ?? "The saved passage could not be read. Its note and original bytes remain in the desk."
            }
            command = nil
        }
    }
    private func save(check: Bool) {
        guard command == nil else { return }
        command = Task {
            let accepted: Bool
            if check { accepted = await draft.checkSavedState(in: model) }
            else { accepted = await draft.save(in: model) }
            command = nil
            if accepted { page = nil; load() }
        }
    }
    private enum CopyKind: Equatable { case text, location, hex }
    private func copy(_ kind: CopyKind) {
        guard let page = currentPage, model.canOperate else { return }
        let value: String
        switch kind {
        case .text: value = page.text
        case .location: value = page.withLocationText
        case .hex: value = page.originalHex
        }
        guard page.target.isCurrent(in: model.state) else { return }
        publish(value, message: kind == .hex ? "Copied exact selected original bytes as hex." : "Copied the saved passage with its original line endings.")
    }
    private func publish(_ value: String, message: String) {
        let board = NSPasteboard.general, version = NSPasteboard.general.changeCount
        switch AtlasReaderSelectionClipboard.publish(value, to: board, expectedChangeCount: version) {
        case .copied: notice = message
        case .tooLarge: notice = "This copy exceeds the clipboard allowance. The saved passage and note remain available."
        case .changed: notice = "Another app changed the clipboard before copying. Try Copy again."
        case .failed: notice = "The clipboard did not accept this copy. The original remains in the reading desk."
        }
    }
}
