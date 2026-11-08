import SwiftUI

/// The primary project view publishes bounded discovery pages immediately.
/// Every row opens through its retained session/page identity, including native
/// filenames that cannot be represented by the ordinary UTF-8 path opener.
@MainActor struct AtlasProjectBrowserView: View {
    @Bindable var model: AtlasProjectBrowserModel
    let entries: [AtlasProjectBrowserFile]
    let scopeLabel: String
    let preparing, canPrepare, hasAtlas: Bool
    let progress: String
    let build, cancelPreviews, refresh, showAtlas: () -> Void
    let canKeep: Bool
    let keep: (AtlasPagedReaderModel) -> Void
    @State private var capture: AtlasProjectBrowserCapture?
    @State private var readerTask: Task<Void, Never>?
    @State private var readerNotice = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label("Project files", systemImage: "folder").font(.headline)
                Spacer()
                if hasAtlas { Button("Show text atlas", action: showAtlas) }
                if preparing {
                    ProgressView().controlSize(.small)
                    Button("Stop previews", action: cancelPreviews)
                } else if !hasAtlas {
                    Button("Build text atlas", action: build).disabled(!canPrepare)
                        .help("Prepare previews from a separate catalog of up to 20,000 files. Browsing and reading do not require it.")
                }
                Button("Refresh project", action: refresh)
            }
            if preparing { Text(progress).font(.caption).foregroundStyle(.secondary) }
            if let info = model.info {
                HStack {
                    Text("\(info.discovered) files discovered").font(.callout.weight(.semibold))
                    Text(info.more ? (info.paused ? "Paused" : "More to discover")
                        : (info.complete ? "Discovery complete" : "Discovery incomplete"))
                        .font(.caption).foregroundStyle(info.complete ? Color.secondary : Color.orange)
                    Spacer()
                    if model.busy { ProgressView().controlSize(.small) }
                    Button(info.paused ? "Continue discovery" : "Discover next page", action: model.advance)
                        .disabled(!model.canContinue || readerTask != nil)
                        .help("Read the next bounded batch of directory metadata; already displayed pages are not rescanned")
                    Button("Stop", action: model.stop).disabled(model.closed || (!model.busy && (!info.more || info.paused)))
                }
                Text("Excluded: \(info.excluded) · Symlinks skipped: \(info.symlinks) · Special files: \(info.special) · Discovery omissions: \(info.omissionCount)")
                    .font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                if info.omissionCount > 0 {
                    Text("Unavailable: \(info.unavailable) · Cycles: \(info.cycles) · Path limit: \(info.pathLimited) · Depth limit: \(info.depthLimited) · Queue limit: \(info.queueRefused)")
                        .font(.caption2).foregroundStyle(.orange).textSelection(.enabled)
                }
                if let page = model.page, model.currentPage {
                    Text("Page \(page.id) · \(entries.count) of \(page.files.count) files in this page · \(scopeLabel)")
                        .font(.caption).foregroundStyle(.secondary)
                    Divider()
                    if entries.isEmpty {
                        ContentUnavailableView {
                            Label(page.files.isEmpty ? "No files in this batch" : "No files match this filter", systemImage: "folder")
                        } description: {
                            Text(info.more
                                ? "Directories and excluded entries can fill a batch. Continue discovery to reach more files, or change the current file filter."
                                : (info.complete ? "Discovery has finished. Try another retained page or change the file filter."
                                    : "Discovery finished with omissions. Files outside the discovered pages are not known to be absent."))
                        }.frame(maxWidth: .infinity, maxHeight: .infinity)
                    } else {
                        List(entries) { file in
                            Button { open(file, from: page) } label: {
                                HStack(alignment: .top) {
                                    Image(systemName: "doc.text").foregroundStyle(.secondary)
                                    Text(verbatim: file.display).font(.system(size: 12, design: .monospaced))
                                        .lineLimit(2).frame(maxWidth: .infinity, alignment: .leading)
                                    Text("\(file.observedBytes) bytes observed").font(.caption2).foregroundStyle(.secondary)
                                }.contentShape(Rectangle())
                            }.buttonStyle(.plain).disabled(!model.canOperate || readerTask != nil)
                                .accessibilityLabel("Open " + file.display)
                                .help("Capture the selected file now and read it independently of discovery")
                        }.listStyle(.inset).id("\(info.owner):\(page.id)")
                    }
                    HStack {
                        Button("Previous page") { if page.id > info.firstRetained { model.load(page: page.id - 1) } }
                            .disabled(!model.canOperate || page.id <= info.firstRetained)
                        Button("Next retained page") { if page.id < info.latest { model.load(page: page.id + 1) } }
                            .disabled(!model.canOperate || page.id >= info.latest)
                        Button("Latest page") { model.load(page: info.latest) }.disabled(!model.canOperate || page.id == info.latest)
                        Spacer()
                        Text("Retained pages \(info.firstRetained)…\(info.latest)").font(.caption).foregroundStyle(.secondary)
                    }.controlSize(.small)
                    Text("The latest four pages stay available as you continue; Refresh starts discovery again. Page order follows discovery, and the file filter applies to the displayed page.")
                        .font(.caption2).foregroundStyle(.secondary)
                } else {
                    Button("Reload latest page", action: model.reload).disabled(!model.canOperate)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            } else {
                VStack(spacing: 12) {
                    if model.busy {
                        ProgressView("Discovering the first project page…")
                        Button("Stop discovery", action: model.stop)
                    } else {
                        Text("The first project page is unavailable.").font(.headline)
                        Button("Reload accepted page", action: model.reload).disabled(model.closed)
                        Button("Refresh project", action: refresh)
                    }
                }.frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            Divider()
            Text(readerNotice.isEmpty ? model.notice : readerNotice)
                .font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            Text("Browsing and Quick Open can continue beyond 20,000 files. Text-atlas preparation retains its separate 20,000-file limit; workspace search reports its own coverage.")
                .font(.caption2).foregroundStyle(.secondary)
        }.padding(12).frame(maxWidth: .infinity, maxHeight: .infinity)
        .sheet(item: $capture) { capture in
            AtlasProjectCaptureView(model: model, origin: capture, canKeep: canKeep, keep: keep) {
                capture.reader.close(); self.capture = nil
            }
        }
        .onDisappear { readerTask?.cancel(); capture?.reader.close(); capture = nil }
    }
    private func open(_ file: AtlasProjectBrowserFile, from page: AtlasProjectBrowserPage) {
        guard readerTask == nil else { return }
        readerNotice = ""
        readerTask = Task {
            defer { readerTask = nil }
            do {
                let captured = try await model.openFile(file, from: page, readerTransport: .native,
                    search: .native, outline: .native, document: .native)
                if Task.isCancelled { captured.reader.close() }
                else { capture = captured }
            } catch {
                readerNotice = (error as? AtlasProjectBrowserError)?.message
                    ?? (error as? AtlasReaderError)?.message ?? "The selected file could not be opened."
            }
        }
    }
}

/// Keep one originating capture and one linked reader. A new target replaces
/// only the previous linked reader after acceptance; failed navigation leaves
/// both visible captures intact. Closing the target returns to the exact origin.
@MainActor private struct AtlasProjectCaptureView: View {
    @Bindable var model: AtlasProjectBrowserModel
    let origin: AtlasProjectBrowserCapture
    let canKeep: Bool
    let keep: (AtlasPagedReaderModel) -> Void
    let done: () -> Void
    @State private var linked: AtlasProjectBrowserCapture?
    @State private var task: Task<Void, Never>?
    @State private var operation = UUID()
    @State private var notice = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text(verbatim: origin.file.display).font(.headline).textSelection(.enabled)
                Spacer()
                if task != nil {
                    ProgressView().controlSize(.small)
                    Button("Stop opening link") { task?.cancel() }
                }
                Button("Done", action: done).keyboardShortcut(.cancelAction)
            }
            Text("The originating capture stays open while you follow links to files within this project, including files outside the retained discovery pages.")
                .font(.caption).foregroundStyle(.secondary)
            HSplitView {
                AtlasPagedSourceView(model: origin.reader, canKeep: canKeep, keep: keep,
                    openLocalLink: navigation(from: origin)).id(origin.id).frame(minWidth: 400)
                if let linked {
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            Text(verbatim: linked.file.display).font(.headline).textSelection(.enabled)
                            Spacer()
                            Button("Close linked file") { closeLinked() }
                        }
                        Text("New capture of the linked file. The originating capture is retained on the left.")
                            .font(.caption).foregroundStyle(.secondary)
                        if linked.fragment.status != .none {
                            Text(verbatim: linked.fragment.navigationDescription)
                                .font(.caption)
                                .foregroundStyle(linked.fragment.isFallback ? Color.orange : Color.secondary)
                                .textSelection(.enabled)
                        }
                        AtlasPagedSourceView(model: linked.reader, canKeep: canKeep, keep: keep,
                            openLocalLink: navigation(from: linked))
                    }.id(linked.id).frame(minWidth: 400)
                }
            }
            if !notice.isEmpty { Text(notice).font(.caption).foregroundStyle(.secondary).textSelection(.enabled) }
        }.padding(14).frame(minWidth: 850, idealWidth: linked == nil ? 1100 : 1400, minHeight: 600, idealHeight: 760)
            .onDisappear {
                operation = UUID(); task?.cancel(); task = nil
                linked?.reader.close(); linked = nil; origin.reader.close()
            }
    }
    private func navigation(from source: AtlasProjectBrowserCapture) -> AtlasDocumentPreviewModel.LocalNavigation? {
        guard task == nil, !model.closed else { return nil }
        return { target in open(target, from: source) }
    }
    private func open(_ target: AtlasReaderDocumentLinkTarget, from source: AtlasProjectBrowserCapture) {
        guard task == nil else { return }
        let ticket = UUID(); operation = ticket
        notice = "Opening a new capture of the linked file within this project…"
        task = Task {
            defer { if operation == ticket { task = nil } }
            do {
                let candidate = try await model.openDocumentLink(target, from: source, readerTransport: .native,
                    search: .native, outline: .native, document: .native)
                guard operation == ticket, !Task.isCancelled else { candidate.reader.close(); return }
                let previous = linked
                linked = candidate
                previous?.reader.close()
                notice = "Linked capture ready. Close it to return to the originating document."
            } catch {
                guard operation == ticket else { return }
                notice = (error as? AtlasProjectBrowserError)?.message
                    ?? (error as? AtlasReaderError)?.message ?? "The linked file could not be opened."
            }
        }
    }
    private func closeLinked() {
        operation = UUID(); task?.cancel(); task = nil
        linked?.reader.close(); linked = nil
        notice = "Returned to the retained originating document."
    }
}
