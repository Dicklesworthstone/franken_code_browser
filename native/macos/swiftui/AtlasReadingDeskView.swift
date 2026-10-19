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
    @Environment(\.dismiss) private var dismiss
    @State private var confirmRestore = false
    @State private var panelNotice = ""
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
                Button("Save new checkpoint…", action: saveCheckpoint).disabled(!model.canOperate || !model.hasUnsavedContents)
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
            Text("Checkpoints contain all retained source bytes and personal labels, unencrypted. Source files are read only. Save explicitly before closing the app.")
                .font(.caption2).foregroundStyle(.secondary).frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(16).frame(minWidth: 900, idealWidth: 1150, minHeight: 600, idealHeight: 740)
        .onAppear { model.start() }
        .sheet(item: $readerCapture) { capture in AtlasDeskReaderView(desk: model, capture: capture, pins: pins) }
        .onDisappear { readerTask?.cancel(); readerCapture?.reader.close(); pins.closeAll() }
        .alert("Replace this reading desk?", isPresented: $confirmRestore) {
            Button("Choose checkpoint…", action: chooseCheckpoint)
            Button("Cancel", role: .cancel) { }
        } message: {
            Text("Opening a checkpoint replaces the current \(model.state?.panes.count ?? 0) panes and \(model.state?.bookmarks.count ?? 0) bookmarks after validation. Save a new checkpoint first if you want to preserve this arrangement.")
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
            Text("Named bookmarks").font(.headline)
            TextField("Why keep this location?", text: $model.bookmarkLabel)
                .onSubmit { model.bookmark() }.disabled(!model.canOperate || model.state?.active == nil)
            Button("Bookmark current range", action: model.bookmark)
                .disabled(!model.canOperate || model.state?.active == nil || model.bookmarkLabel.isEmpty || model.bookmarkLabel.utf8.count > 256)
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 8) {
                    ForEach(model.state?.bookmarks ?? []) { bookmark in
                        HStack(alignment: .top) {
                            Button { model.apply(kind: 4, subject: bookmark.id) } label: {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(bookmark.label).lineLimit(3)
                                    Text("Byte \(bookmark.location.offset)").font(.caption2).foregroundStyle(.secondary)
                                }.frame(maxWidth: .infinity, alignment: .leading)
                            }
                            Button { model.apply(kind: 5, subject: bookmark.id) } label: { Image(systemName: "bookmark.slash") }
                                .help("Forget this bookmark")
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
        panel.message = "Capture up to 4 MiB of current file bytes. Later reads, bookmarks and saved checkpoints retain this exact capture."
        panel.canChooseFiles = true; panel.canChooseDirectories = false; panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        guard let access = AtlasDeskFileAccess(selectedURL: url) else { panelNotice = "The selected file did not grant current read access."; return }
        panelNotice = ""; model.capture(path: url.path, accessLease: access)
    }
    private func chooseCheckpoint() {
        let panel = NSOpenPanel()
        panel.title = "Open a saved reading desk"
        panel.message = "Restore exact saved source bytes, bookmarks and history. Source labels in the checkpoint never grant access to working-tree files."
        panel.canChooseFiles = true; panel.canChooseDirectories = false; panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        guard let access = AtlasDeskFileAccess(selectedURL: url) else { panelNotice = "The selected checkpoint did not grant current read access."; return }
        panelNotice = ""; model.restore(path: url.path, accessLease: access)
    }
    private func saveCheckpoint() {
        let panel = NSSavePanel()
        panel.title = "Save a new reading desk checkpoint"
        panel.nameFieldStringValue = "Reading-desk.fcbdesk"
        panel.message = "Includes all \(model.state?.retainedBytes ?? 0) retained source bytes and personal labels, unencrypted. Choose a new filename; existing checkpoints are preserved."
        panel.canCreateDirectories = true
        guard panel.runModal() == .OK, let url = panel.url else { return }
        guard let access = AtlasDeskFileAccess(selectedURL: url) else { panelNotice = "The selected destination did not grant current write access."; return }
        panelNotice = ""; model.save(path: url.path, accessLease: access)
    }
}
