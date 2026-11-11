import SwiftUI
import AppKit

/// Adopts the security-scoped access supplied by a successful native open/save
/// panel. The panel has already started this access; do not start it twice or
/// reject a new save destination based on another startAccessing call. The token
/// stops its adopted access once after every foreign read/write has drained.
private final class AtlasDeskFileAccess {
    let url: URL
    init?(selectedURL: URL) {
        guard selectedURL.isFileURL else { return nil }
        url = selectedURL
    }
    deinit { url.stopAccessingSecurityScopedResource() }
}

@MainActor struct AtlasReadingDeskView: View {
    @Bindable var model: AtlasReadingDeskModel
    var project: AtlasProjectBrowserModel? = nil
    @Environment(\.dismiss) private var dismiss
    @State private var confirmRestore = false
    @State private var panelNotice = ""
    @State private var noteDraft: AtlasDeskNoteDraft?
    @State private var reattachment: AtlasDeskReattachment?
    @State private var comparisonReview: AtlasDeskComparisonReview?
    @State private var readerCapture: AtlasDeskReaderCapture?
    @State private var readerTask: Task<Void, Never>?
    @State private var pins = AtlasPinnedReaders<AtlasPagedReaderModel>(
        canFork: { $0.canPin }, fork: { try await $0.fork() }, retire: { $0.close() },
        failureMessage: { ($0 as? AtlasReaderError)?.message ?? "The retained source could not be pinned." })

    var body: some View {
        VStack(spacing: 10) {
            HStack {
                Text("Reading desk").font(.title2.weight(.semibold))
                Spacer()
                Button("Capture file…", action: chooseSource).disabled(!model.canOperate)
                Button("Open checkpoint…") {
                    if model.hasUnsavedContents { confirmRestore = true } else { chooseCheckpoint() }
                }.disabled(!model.canOperate)
                Button("Save new checkpoint…", action: saveCheckpoint).disabled(!model.canSaveCheckpoint)
                Menu("Export source pack…") {
                    Button("Up to 64 KiB of selected source…") { exportSourcePack(sourceBytes: 64 * 1024) }
                    Button("Up to 1 MiB of selected source…") { exportSourcePack(sourceBytes: 1024 * 1024) }
                    Button("Up to 4 MiB of selected source…") { exportSourcePack(sourceBytes: 4 * 1024 * 1024) }
                }.disabled(!model.canOperate || model.state?.bookmarks.isEmpty != false)
                Button("Done") { readerTask?.cancel(); pins.closeAll(); dismiss() }.keyboardShortcut(.cancelAction)
            }
            HSplitView {
                sidebar.frame(minWidth: 240, idealWidth: 270, maxWidth: 360)
                reader.frame(minWidth: 460, maxWidth: .infinity, maxHeight: .infinity)
            }
            HStack {
                if model.busy { ProgressView().controlSize(.small); Button("Cancel", action: model.cancel) }
                Text(panelNotice.isEmpty ? model.notice : panelNotice)
                    .font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                Spacer()
                Button("Reload state", action: model.reload).disabled(!model.canOperate)
            }
            Text("Checkpoints contain all retained source bytes, personal labels and full note text, unencrypted. Source files are read only. Save explicitly before closing the app.")
                .font(.caption2).foregroundStyle(.secondary).frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(16).frame(minWidth: 900, idealWidth: 1150, minHeight: 600, idealHeight: 740)
        .onAppear { model.start() }
        .sheet(item: $readerCapture) { capture in
            AtlasDeskReaderView(desk: model, capture: capture, pins: pins, project: project)
        }
        .sheet(item: $noteDraft) { draft in AtlasDeskNoteEditor(model: model, draft: draft) }
        .sheet(item: $comparisonReview) { review in AtlasDeskReattachmentReviewView(model: model, review: review) }
        .onDisappear { reattachment = nil; comparisonReview = nil; readerTask?.cancel(); readerCapture?.reader.close(); pins.closeAll() }
        .alert("Reattach this saved note?", isPresented: Binding(
            get: { reattachment != nil }, set: { if !$0 { reattachment = nil } }), presenting: reattachment) { target in
            Button("Reattach note") { panelNotice = ""; model.reattachBookmark(target) }
                .disabled(!model.canOperate || !target.isCurrent(in: model.state))
            Button("Cancel", role: .cancel) { }
        } message: { target in
            Text("Move “\(target.bookmark.label)” from \(rangeDescription(target.bookmark.location)) to \(rangeDescription(target.pane.location)) in “\(target.pane.label)”? The original bytes must match exactly. Its label and note text will be preserved.")
        }
        .alert("Replace this reading desk?", isPresented: $confirmRestore) {
            Button("Choose checkpoint…", action: chooseCheckpoint)
            Button("Cancel", role: .cancel) { }
        } message: {
            Text("Opening a checkpoint replaces the current \(model.state?.panes.count ?? 0) panes and \(model.state?.bookmarks.count ?? 0) bookmarks and notes after validation. Save a new checkpoint first if you want to preserve their text and arrangement.")
        }
    }

    private var sidebar: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Button { model.apply(kind: 9) } label: { Label("Back", systemImage: "chevron.left") }
                    .disabled(!model.canOperate || model.state?.canBack != true)
                Button { model.apply(kind: 10) } label: { Label("Forward", systemImage: "chevron.right") }
                    .disabled(!model.canOperate || model.state?.canForward != true)
            }
            Text("Retained sources").font(.headline)
            if let state = model.state {
                Text("\(state.retainedSources) sources · \(state.retainedBytes) original bytes")
                    .font(.caption).foregroundStyle(.secondary)
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 6) {
                        ForEach(state.panes) { pane in
                            VStack(alignment: .leading, spacing: 4) {
                                Button { model.apply(kind: 0, subject: pane.id) } label: {
                                    HStack {
                                        Image(systemName: pane.pinned ? "pin.fill" : "doc.text")
                                        Text(pane.label).lineLimit(2).truncationMode(.middle)
                                        Spacer()
                                        if pane.id == state.activePane { Image(systemName: "checkmark.circle.fill") }
                                    }.frame(maxWidth: .infinity, alignment: .leading)
                                }.disabled(!model.canOperate)
                                HStack {
                                    Button(pane.pinned ? "Unpin" : "Pin") { model.apply(kind: pane.pinned ? 7 : 6, subject: pane.id) }
                                    Button("Close pane") { model.apply(kind: 8, subject: pane.id) }
                                }.controlSize(.small).disabled(!model.canOperate)
                            }.padding(.vertical, 4)
                        }
                    }
                }.frame(minHeight: 100)
            }
            Divider()
            HStack {
                Text("Bookmarks and notes").font(.headline)
                Spacer()
                Text("\(model.state?.bookmarks.count ?? 0) / 32").font(.caption).foregroundStyle(.secondary)
            }
            TextField("Why keep this location?", text: $model.bookmarkLabel)
                .onSubmit { model.bookmark() }.disabled(!model.canOperate || model.state?.active == nil)
            Button("Bookmark current range", action: model.bookmark)
                .disabled(!model.canOperate || model.state?.active == nil || model.bookmarkLabel.isEmpty
                    || model.bookmarkLabel.utf8.count > 256 || model.bookmarkLabel.utf8.contains(0)
                    || (model.state?.bookmarks.count ?? 32) >= 32)
            Button("Add note to current range…") {
                guard let state = model.state, let pane = state.active else { return }
                panelNotice = ""; noteDraft = AtlasDeskNoteDraft(state: state, pane: pane, label: model.bookmarkLabel)
            }.disabled(!model.canOperate || model.state?.active == nil || (model.state?.bookmarks.count ?? 32) >= 32)
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 8) {
                    ForEach(model.state?.bookmarks ?? []) { bookmark in
                        VStack(alignment: .leading, spacing: 4) {
                            HStack(alignment: .top) {
                                Button { model.apply(kind: 4, subject: bookmark.id) } label: {
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(bookmark.label).lineLimit(3)
                                        Text("Byte \(bookmark.location.offset)\(bookmark.body.isEmpty ? "" : " · note")")
                                            .font(.caption2).foregroundStyle(.secondary)
                                    }.frame(maxWidth: .infinity, alignment: .leading)
                                }
                                Button {
                                    guard let state = model.state else { return }
                                    panelNotice = ""; noteDraft = AtlasDeskNoteDraft(state: state, bookmark: bookmark)
                                } label: { Image(systemName: "square.and.pencil") }
                                .help("Edit this label and personal note without changing its source anchor")
                                Button { model.apply(kind: 5, subject: bookmark.id) } label: { Image(systemName: "bookmark.slash") }
                                    .help("Forget this bookmark and its note")
                            }
                            Button("Reattach to selected range…") {
                                panelNotice = ""; reattachment = reattachmentTarget(bookmark)
                            }
                            .font(.caption)
                            .disabled(reattachmentTarget(bookmark) == nil)
                            .help("Select a nonempty range of the same length in a retained source. Reattachment compares original bytes before moving the note.")
                            Button("Find range in another capture…") {
                                guard let state = model.state else { return }
                                panelNotice = ""; comparisonReview = AtlasDeskComparisonReview(state: state, bookmark: bookmark)
                            }
                            .font(.caption)
                            .disabled((bookmark.location.selection.map { $0.start >= $0.end } ?? true)
                                || bookmark.updateSequence == UInt64.max || model.state?.panes.isEmpty != false)
                            .help("Compare the note’s exact saved source with a retained capture, review a verified matching range, then confirm reattachment.")
                            if !bookmark.body.isEmpty {
                                Text(String(bookmark.body.prefix(200))).font(.caption).foregroundStyle(.secondary)
                                    .lineLimit(3).textSelection(.enabled)
                            }
                        }.disabled(!model.canOperate)
                    }
                }
            }.frame(minHeight: 100)
        }.padding(.trailing, 12)
    }

    private var reader: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let pane = model.state?.active {
                HStack {
                    Text(pane.label).font(.headline).lineLimit(2).textSelection(.enabled)
                    Spacer()
                    Button("Open full reader") { openReader(pane.id) }
                        .disabled(!model.canOperate || readerTask != nil || model.readerDomain == nil)
                        .help("Read this exact retained capture with Find, Outline, Markdown, Split preview, copying, pinning and comparison. Saved labels never reopen disk files.")
                }
                HStack {
                    Text("Start byte").font(.caption)
                    TextField("0", text: $model.offsetInput).frame(width: 95).onSubmit { model.goToOffset() }
                    Button("Go") { model.goToOffset() }
                    Text("End byte").font(.caption)
                    TextField("0", text: $model.selectionEndInput).frame(width: 95)
                    Button("Select range") { model.goToOffset(select: true) }
                    Spacer()
                    Button("Next page", action: model.nextPage).disabled(model.page?.nextOffset == nil)
                }.disabled(!model.canOperate)
                if let selected = pane.location.selection {
                    Text("Exact selected source range: \(selected.start)..<\(selected.end)")
                        .font(.caption).foregroundStyle(.secondary)
                }
                if let page = model.page {
                    Text("Page \(page.range.start)..<\(page.range.end)\(page.firstLine.map { " · from line \($0)" } ?? "")\(page.reachesEnd ? " · end of capture" : "")")
                        .font(.caption).foregroundStyle(.secondary)
                    ScrollView([.horizontal, .vertical]) {
                        Text(page.text).font(.system(size: 12, design: .monospaced))
                            .textSelection(.enabled).fixedSize(horizontal: true, vertical: true)
                            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading).padding(10)
                    }.background(Color(nsColor: .textBackgroundColor))
                    if page.hasReplacements {
                        Text("This logical text page contains decoding replacements. Original bytes remain retained in the checkpoint.")
                            .font(.caption2).foregroundStyle(.secondary)
                    }
                } else {
                    Text(model.busy ? "Reading retained source…" : "This retained source has no accepted page. Reload its state to retry.")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            } else {
                ContentUnavailableView("Keep a source to begin", systemImage: "books.vertical",
                    description: Text("Use Keep in reading desk from a retained reader, capture a file explicitly, or open a saved checkpoint."))
            }
        }.padding(.leading, 12)
    }
    private func reattachmentTarget(_ bookmark: AtlasDeskBookmark) -> AtlasDeskReattachment? {
        guard let state = model.state, let pane = state.active else { return nil }
        return AtlasDeskReattachment(state: state, bookmark: bookmark, pane: pane)
    }
    private func rangeDescription(_ location: AtlasDeskLocation) -> String {
        let range = location.selection.map { "bytes \($0.start)..<\($0.end)" } ?? "byte \(location.offset)"
        return "\(range) (capture \(location.file), revision \(location.revision))"
    }
    private func openReader(_ pane: UInt64) {
        guard model.canOperate, readerTask == nil else { return }
        panelNotice = ""
        readerTask = Task {
            do {
                let opened = try await model.openReader(pane: pane, transport: .native,
                    search: .native, outline: .native, document: .native)
                if Task.isCancelled { opened.reader.close() }
                else { readerCapture = opened }
            } catch {
                if !Task.isCancelled {
                    panelNotice = (error as? AtlasReaderError)?.message
                        ?? (error as? AtlasReadingDeskError)?.message ?? "The retained source could not be opened."
                }
            }
            readerTask = nil
        }
    }
    private func chooseSource() {
        let panel = NSOpenPanel()
        panel.title = "Capture a source file in the reading desk"
        panel.message = "Capture up to 4 MiB of current file bytes. Later reads, bookmarks, notes and saved checkpoints retain this exact capture."
        panel.canChooseFiles = true; panel.canChooseDirectories = false; panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        guard let access = AtlasDeskFileAccess(selectedURL: url) else { panelNotice = "The selected file did not grant current read access."; return }
        panelNotice = ""; model.capture(path: url.path, accessLease: access)
    }
    private func chooseCheckpoint() {
        let panel = NSOpenPanel()
        panel.title = "Open a saved reading desk"
        panel.message = "Restore exact saved source bytes, bookmarks, personal notes and history. Source labels in the checkpoint never grant access to working-tree files."
        panel.canChooseFiles = true; panel.canChooseDirectories = false; panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        guard let access = AtlasDeskFileAccess(selectedURL: url) else { panelNotice = "The selected checkpoint did not grant current read access."; return }
        panelNotice = ""; model.restore(path: url.path, accessLease: access)
    }
    private func saveCheckpoint() {
        let panel = NSSavePanel()
        panel.title = "Save a new reading desk checkpoint"
        panel.nameFieldStringValue = "Reading-desk.fcbdesk"
        panel.message = "Includes all \(model.state?.retainedBytes ?? 0) retained source bytes, personal labels and full note text, unencrypted. Choose a new filename; existing checkpoints are preserved."
        panel.canCreateDirectories = true
        guard panel.runModal() == .OK, let url = panel.url else { return }
        guard let access = AtlasDeskFileAccess(selectedURL: url) else { panelNotice = "The selected destination did not grant current write access."; return }
        panelNotice = ""; model.save(path: url.path, accessLease: access)
    }
    private func exportSourcePack(sourceBytes: UInt64) {
        guard model.canOperate, let state = model.state, !state.bookmarks.isEmpty else { return }
        let target = AtlasDeskSourcePackTarget(state: state)
        let limits = AtlasDeskSourcePackLimits(sourceBytes: sourceBytes)
        let panel = NSSavePanel()
        panel.title = "Export bookmarked source and notes"
        panel.prompt = "Export"
        panel.nameFieldStringValue = "Reading-desk-source.json"
        panel.message = "Includes all \(state.bookmarks.count) bookmark labels and full personal notes, plus up to \(sourceBytes) original bytes from \(target.selectionCount) saved selections, unredacted and unencrypted. Whole selections that exceed the budget are listed as omitted. JSON is limited to 16 MiB. Source labels may contain private paths. Review your notes before sharing. Choose a new filename; existing files are preserved."
        panel.canCreateDirectories = true
        guard panel.runModal() == .OK, let url = panel.url else { return }
        guard let access = AtlasDeskFileAccess(selectedURL: url) else { panelNotice = "The selected destination did not grant current write access."; return }
        panelNotice = ""
        model.exportSourcePack(target, limits: limits, path: url.path, accessLease: access)
    }
}


private struct AtlasDeskComparisonReview: Identifiable {
    let id = UUID()
    let state: AtlasDeskState
    let bookmark: AtlasDeskBookmark
}

/// Comparison is read-only. The single prominent confirmation submits an
/// atomic source-bound note command; it never changes a pane as an intermediate
/// step. Dismissing the preview cancels only unfinished comparison work.
@MainActor private struct AtlasDeskReattachmentReviewView: View {
    @Bindable var model: AtlasReadingDeskModel
    let review: AtlasDeskComparisonReview
    @Environment(\.dismiss) private var dismiss
    @State private var paneID: UInt64
    @State private var proposal: AtlasDeskReattachmentProposal?
    @State private var task: Task<Void, Never>?
    @State private var notice = ""

    init(model: AtlasReadingDeskModel, review: AtlasDeskComparisonReview) {
        self.model = model; self.review = review
        let candidate = review.state.panes.first { $0.location.file != review.bookmark.location.file
            || $0.location.revision != review.bookmark.location.revision }
        _paneID = State(initialValue: candidate?.id ?? review.state.activePane ?? review.state.panes.first?.id ?? 0)
    }
    private var target: AtlasDeskComparisonTarget? {
        guard let pane = review.state.panes.first(where: { $0.id == paneID }) else { return nil }
        return AtlasDeskComparisonTarget(state: review.state, bookmark: review.bookmark, pane: pane)
    }
    private var confirmation: AtlasDeskReattachment? {
        guard let proposal, proposal.target.pane.id == paneID, proposal.target.isCurrent(in: model.state) else { return nil }
        return AtlasDeskReattachment(proposal: proposal)
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Recover a saved source range").font(.title2.weight(.semibold))
                Spacer()
                Button("Done") { task?.cancel(); dismiss() }.keyboardShortcut(.cancelAction)
            }
            Text(review.bookmark.label).font(.headline).textSelection(.enabled)
            if !review.bookmark.body.isEmpty {
                Text(String(review.bookmark.body.prefix(512))).font(.callout).lineLimit(5).textSelection(.enabled)
            }
            Text("The note stays attached to its original capture until you confirm a destination. Capture the updated file in the reading desk first, then choose that retained source below.")
                .font(.callout).foregroundStyle(.secondary)
            Picker("Compare with", selection: $paneID) {
                ForEach(review.state.panes) { pane in
                    Text("\(pane.label) · capture \(pane.location.file), revision \(pane.location.revision)").tag(pane.id)
                }
            }.disabled(task != nil)
            HStack {
                Button("Compare captured sources") { compare(expanded: false) }
                    .disabled(!model.canOperate || target?.isCurrent(in: model.state) != true || task != nil)
                if let proposal, !proposal.complete, proposal.target.maxWork < 64 * 1024 * 1024 {
                    Button("Compare with larger budget") { compare(expanded: true) }
                        .disabled(!model.canOperate || target?.isCurrent(in: model.state) != true || task != nil)
                }
                if task != nil { ProgressView().controlSize(.small); Button("Cancel comparison") { task?.cancel() } }
            }
            if target?.isCurrent(in: model.state) != true {
                Text("The desk or saved note changed. Close this review and open it again from the current note.")
                    .foregroundStyle(.orange)
            }
            if let proposal, proposal.target.pane.id == paneID {
                result(proposal)
            } else {
                Text(notice.isEmpty ? "Comparison uses exact saved bytes, including the original capture if its pane has been closed." : notice)
                    .foregroundStyle(.secondary).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
            HStack {
                Text("The note’s label and text are preserved. Save a new checkpoint after reattaching.")
                    .font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("Reattach note to this range") {
                    guard let confirmation, confirmation.isCurrent(in: model.state), model.canOperate else { return }
                    model.reattachBookmark(confirmation); dismiss()
                }.buttonStyle(.borderedProminent).disabled(confirmation == nil || !model.canOperate || task != nil)
            }
        }
        .padding(20).frame(minWidth: 820, idealWidth: 1000, minHeight: 600, idealHeight: 720)
        .onChange(of: paneID) { _, _ in proposal = nil; notice = "" }
        .onDisappear { task?.cancel() }
    }

    @ViewBuilder private func result(_ value: AtlasDeskReattachmentProposal) -> some View {
        switch value.outcome {
        case .mapped:
            Text("The saved selection has an exact byte match in this alignment.").font(.headline)
            Text("Review both captures before confirming. Repeated text can have several possible alignments; equal bytes alone do not establish that the files are versions of one another.")
                .font(.caption).foregroundStyle(.secondary)
        case .unchanged:
            Text("This is already the same saved source range.").font(.headline)
        case .changed:
            Text("No unchanged span contains the whole saved selection.").font(.headline)
            Text("The selection crosses changed text. Keep the original evidence, or select a byte-identical range manually in the destination pane.")
                .font(.caption).foregroundStyle(.secondary)
        case .unresolved:
            Text("This range could not be mapped within the comparison budget.").font(.headline)
            Text("Try the larger budget, or review the source and select a byte-identical destination range manually.")
                .font(.caption).foregroundStyle(.secondary)
        }
        if !value.complete {
            Text("The full comparison stopped at the \(value.quality == "work-limit" ? "work" : "edit-distance") limit. Any proposed range shown here was verified byte for byte; unresolved regions are excluded.")
                .font(.caption).foregroundStyle(.secondary)
        }
        HStack(alignment: .top, spacing: 12) {
            preview(value.beforePreview, source: value.beforeSource, selected: review.bookmark.location.selection!, title: "Saved source")
            if let after = value.afterPreview, let selected = value.targetRange {
                preview(after, source: value.afterSource, selected: selected, title: "Proposed destination")
            }
        }.frame(maxHeight: .infinity)
        Text("\(value.workUnits) / \(value.target.maxWork) comparison work units · no source file reopened")
            .font(.caption2).foregroundStyle(.secondary)
    }

    private func preview(_ value: AtlasDeskReattachmentPreview, source: AtlasDeskComparisonSource,
        selected: AtlasDeskRange, title: String) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(title).font(.headline)
            Text(source.label).font(.caption).lineLimit(2).textSelection(.enabled)
            Text("Capture \(source.file_id.value), revision \(source.source_revision.value) · bytes \(selected.start)..<\(selected.end)")
                .font(.caption2).foregroundStyle(.secondary)
            ScrollView([.horizontal, .vertical]) {
                Text(value.text).font(.system(size: 12, design: .monospaced)).textSelection(.enabled)
                    .fixedSize(horizontal: true, vertical: true).frame(maxWidth: .infinity, alignment: .topLeading).padding(8)
            }.background(Color(nsColor: .textBackgroundColor))
            Text("Preview bytes \(value.original_range.start)..<\(value.original_range.end)\(value.has_replacements ? " · decoding replacements" : "")")
                .font(.caption2).foregroundStyle(.secondary)
            if value.selectionRange == nil {
                Text("The full selected range is not representable inside this bounded text preview. Its original byte range remains unchanged.")
                    .font(.caption2).foregroundStyle(.secondary)
            }
        }.frame(maxWidth: .infinity, maxHeight: .infinity)
    }
    private func compare(expanded: Bool) {
        guard let target, model.canOperate, task == nil,
              let request = AtlasDeskComparisonTarget(state: target.state, bookmark: target.bookmark, pane: target.pane,
                maxWork: expanded ? 64 * 1024 * 1024 : 8 * 1024 * 1024, maxEdits: expanded ? 512 : 256) else { return }
        proposal = nil; notice = "Comparing the retained captures…"
        task = Task {
            do { proposal = try await model.suggestReattachment(request); notice = "" }
            catch { notice = (error as? AtlasReadingDeskError)?.message ?? "The comparison was canceled or could not finish." }
            task = nil
        }
    }
}
