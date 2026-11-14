import SwiftUI
import AppKit

/// The saved source and optional parent trail stay frozen while a user writes
/// the rationale and chooses a new output file. A write never changes the
/// attached trail or silently republishes after an uncertain reply.
@MainActor struct AtlasSavedTrailAuthoringView: View {
    @Bindable var model: AtlasSavedRepositoryModel
    @Bindable var desk: AtlasReadingDeskModel
    @Bindable var draft: AtlasSavedTrailDraft
    @Environment(\.dismiss) private var dismiss
    @State private var panelNotice = ""
    @State private var choosingDestination = false

    private var writing: Bool {
        model.trailWriteInProgress && model.trailWriteRequest?.operation == draft.submission?.operation
    }
    private var canSave: Bool { !choosingDestination && !writing && draft.validation(in: model) == nil }
    private var currentOperation: Bool {
        draft.submission != nil && model.trailWriteRequest?.operation == draft.submission?.operation &&
            model.trailWriteRequest?.identity == draft.identity
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Label("Add to reading trail", systemImage: "list.number").font(.title2.weight(.semibold))
                Spacer()
                Button(draft.closeProtectionRevision == nil ? "Done" : "Discard draft") { dismiss() }
                    .disabled(writing || choosingDestination).keyboardShortcut(.cancelAction)
            }
            Text(verbatim: draft.source.member.path.display).font(.headline).lineLimit(2).textSelection(.enabled)
            Text("Saved archive \(draft.identity.digest)\nCaptured source \(draft.source.member.digest ?? "")")
                .font(.caption2.monospaced()).foregroundStyle(.secondary).textSelection(.enabled)

            VStack(alignment: .leading, spacing: 10) {
                Picker("Trail", selection: $draft.append) {
                    Text("New trail").tag(false)
                    if draft.parent != nil { Text("Append to attached trail").tag(true) }
                }.pickerStyle(.segmented)
                if draft.append, let parent = draft.parent {
                    Text(verbatim: parent.title.isEmpty ? "Untitled trail" : parent.title).font(.headline)
                    Text("Adds item \(parent.items + 1) after all \(parent.items) existing items, keeping their order and notes. The result is a new file with the attached trail as its parent.")
                        .font(.caption).foregroundStyle(.secondary)
                } else {
                    TextField("Trail title", text: $draft.title).textFieldStyle(.roundedBorder)
                        .accessibilityLabel("New reading trail title, maximum 256 UTF-8 bytes")
                    Text("\(draft.title.utf8.count) / 256 title bytes").font(.caption2).foregroundStyle(.secondary)
                }
                HStack {
                    Text("Original byte range").font(.callout.weight(.semibold))
                    Spacer()
                    Menu("Use source range") {
                        ForEach(draft.presets) { preset in
                            Button("\(preset.label): \(preset.range.start)..<\(preset.range.end)") { draft.choose(preset) }
                        }
                    }.controlSize(.small)
                }
                HStack {
                    TextField("Start", text: $draft.startInput).accessibilityLabel("Original byte range start")
                    Text("..<").font(.body.monospaced())
                    TextField("End", text: $draft.endInput).accessibilityLabel("Original byte range end, exclusive")
                    Text("of \(draft.source.member.observedBytes) bytes").font(.caption).foregroundStyle(.secondary)
                }.textFieldStyle(.roundedBorder)
                Text("The range uses original source bytes. Equal endpoints create a bookmark; 0..<\(draft.source.member.observedBytes) retains the whole-file reference. No source payload is copied into the trail.")
                    .font(.caption).foregroundStyle(.secondary)
                HStack {
                    Text("Why this source matters").font(.callout.weight(.semibold))
                    Spacer()
                    Text("\(draft.rationale.utf8.count) / 4,096 UTF-8 bytes").font(.caption2).foregroundStyle(.secondary)
                }
                TextEditor(text: $draft.rationale).font(.body)
                    .frame(minHeight: 120, idealHeight: 160)
                    .overlay(RoundedRectangle(cornerRadius: 5).stroke(Color.secondary.opacity(0.35)))
                    .accessibilityLabel("Reading trail rationale, optional, maximum 4096 UTF-8 bytes")
            }.disabled(writing || choosingDestination)

            if !writing, let validation = draft.validation(in: model) {
                Text(validation).font(.caption).foregroundStyle(.orange).textSelection(.enabled)
            }
            if !panelNotice.isEmpty {
                Text(verbatim: panelNotice).font(.caption).foregroundStyle(.orange).textSelection(.enabled)
            }
            if let submitted = draft.submission {
                Divider()
                Text("Destination: \(String(reflecting: submitted.destination))")
                    .font(.caption).lineLimit(2).textSelection(.enabled)
                if currentOperation {
                    Text(verbatim: model.trailWriteNotice).font(.caption)
                        .foregroundStyle(model.trailWriteOutcomeKnown ? Color.secondary : Color.orange)
                        .textSelection(.enabled)
                }
                if let receipt = draft.receipt {
                    Text("\(receipt.bytesWritten) bytes written · \(receipt.effect.rawValue)")
                        .font(.caption2).foregroundStyle(.secondary).textSelection(.enabled)
                    if let digest = receipt.digest {
                        Text("Trail SHA-256: \(digest)").font(.caption2.monospaced()).textSelection(.enabled)
                    }
                }
            }
            HStack {
                Button("Copy rationale") {
                    NSPasteboard.general.clearContents()
                    panelNotice = NSPasteboard.general.setString(draft.rationale, forType: .string)
                        ? "Copied the full rationale." : "The clipboard could not accept the rationale. Your draft is unchanged."
                }.disabled(draft.rationale.isEmpty)
                if currentOperation {
                    Button("Reload write receipt", action: model.reloadTrailWriteReceipt).disabled(!model.canOperate)
                }
                Spacer()
                if writing {
                    ProgressView().controlSize(.small)
                    Button("Cancel write", action: model.cancel).disabled(model.trailWriteCancellationRequested)
                } else {
                    Button("Save new trail file…", action: chooseDestination).disabled(!canSave)
                        .keyboardShortcut(.defaultAction)
                }
            }.controlSize(.small)
            Text("The new FCBT file contains source paths, digests, ranges and your text without encryption. After saving, use Open reading trail to follow it. Existing trail files are never replaced.")
                .font(.caption).foregroundStyle(.secondary)
        }
        .padding(18).frame(minWidth: 620, idealWidth: 760, minHeight: 600)
        .interactiveDismissDisabled()
        .onAppear {
            draft.accept(model.trailWriteReceipt)
            desk.registerNoteDraft(draft.id) { [weak draft] in draft?.closeProtectionRevision }
        }
        .onChange(of: model.trailWriteReceipt?.operation) { _, _ in draft.accept(model.trailWriteReceipt) }
        .onDisappear {
            desk.unregisterNoteDraft(draft.id)
            if writing { model.cancel() }
        }
    }

    private func chooseDestination() {
        guard canSave, let selection = try? draft.selection() else { return }
        let parent = draft.selectedParent, title = draft.effectiveTitle, rationale = draft.rationale
        choosingDestination = true
        defer { choosingDestination = false }
        let panel = NSSavePanel()
        panel.title = "Save a new FCBT reading trail"
        panel.message = "Create a new file containing the reviewed original-byte reference and rationale. Choose a new filename; an existing file will not be overwritten."
        panel.nameFieldStringValue = "ReadingTrail.fcbt"
        panel.canCreateDirectories = true
        guard panel.runModal() == .OK, let url = panel.url else { return }
        guard draft.validation(in: model) == nil, draft.range == selection.range,
              draft.effectiveTitle.utf8.elementsEqual(title.utf8), draft.rationale.utf8.elementsEqual(rationale.utf8),
              draft.selectedParent?.generation == parent?.generation, draft.selectedParent?.digest == parent?.digest else {
            panelNotice = "The reviewed source, range or parent changed while choosing a file. Review this draft before saving again."
            return
        }
        let access = AtlasSavedFileAccess(selectedURL: url)
        guard let path = access.nativePath else {
            panelNotice = "The selected filename cannot be represented exactly by the native save interface. Your draft remains available."
            return
        }
        guard let request = model.writeTrail(from: draft.source, selection: selection, parent: parent,
            title: title, rationale: rationale, destination: path, accessLease: access) else {
            panelNotice = model.trailWriteNotice; return
        }
        panelNotice = ""; draft.submitted(request)
    }
}
