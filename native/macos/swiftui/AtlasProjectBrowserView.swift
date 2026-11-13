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
        .onChange(of: ObjectIdentifier(model)) { _, _ in
            // The host keeps this view's identity only for an accepted refresh
            // under the same root grant. Keep completed captures; an old row's
            // in-flight open cannot be transferred to the replacement catalog.
            readerTask?.cancel()
            readerNotice = ""
        }
    }
    private func open(_ file: AtlasProjectBrowserFile, from page: AtlasProjectBrowserPage) {
        guard readerTask == nil else { return }
        let browser = model
        readerNotice = ""
        readerTask = Task {
            defer { readerTask = nil }
            do {
                let captured = try await browser.openFile(file, from: page, readerTransport: .native,
                    search: .native, outline: .native, document: .native)
                if Task.isCancelled || browser.closed { captured.reader.close() }
                else { capture = captured }
            } catch {
                guard !Task.isCancelled, !browser.closed else { return }
                readerNotice = (error as? AtlasProjectBrowserError)?.message
                    ?? (error as? AtlasReaderError)?.message ?? "The selected file could not be opened."
            }
        }
    }
}

/// The originating project capture stays on the left while the shared bounded
/// history retains linked readers on the right. Both their source owners and
/// their mounted views survive Back/Forward, including after a catalog refresh.
@MainActor private struct AtlasProjectCaptureView: View {
    @Bindable var model: AtlasProjectBrowserModel
    let origin: AtlasProjectBrowserCapture
    let canKeep: Bool
    let keep: (AtlasPagedReaderModel) -> Void
    let done: () -> Void
    @State private var history = AtlasDocumentLinkHistory<AtlasProjectBrowserCapture>(
        retire: { $0.reader.close() },
        failureMessage: { ($0 as? AtlasProjectBrowserError)?.message
            ?? ($0 as? AtlasReaderError)?.message ?? "The linked file could not be opened." })

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text(verbatim: origin.file.display).font(.headline).textSelection(.enabled)
                Spacer()
                if history.busy {
                    ProgressView().controlSize(.small)
                    Button("Stop opening link", action: history.cancel).disabled(history.canceling)
                }
                Button("Done", action: done).keyboardShortcut(.cancelAction)
            }
            Text("The originating capture stays open while you follow links to files within this project, including files outside the retained discovery pages.")
                .font(.caption).foregroundStyle(.secondary)
            if origin.catalogOwner != model.info?.owner {
                Text("Project refreshed. This source keeps its captured bytes and position. Following a link first verifies the originating capture against the current project file.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            HSplitView {
                AtlasPagedSourceView(model: origin.reader, canKeep: canKeep, keep: keep,
                    openLocalLink: navigation(from: origin)).id(origin.id).frame(minWidth: 400)
                if history.current != nil { linkedReaders.frame(minWidth: 400) }
            }
            if !history.notice.isEmpty {
                Text(history.notice).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            }
        }.padding(14).frame(minWidth: 850, idealWidth: history.current == nil ? 1100 : 1400, minHeight: 600, idealHeight: 760)
            .onAppear { projectChanged() }
            .onDisappear {
                history.close(); origin.reader.close()
            }
            .onChange(of: ObjectIdentifier(model)) { _, _ in projectChanged() }
            .onChange(of: model.rootAccessIdentity) { _, _ in projectChanged() }
            .onChange(of: model.closed) { _, _ in projectChanged() }
    }
    private var linkedReaders: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Button("Back", action: history.back).disabled(!history.canBack)
                Button("Forward", action: history.forward).disabled(!history.canForward)
                Text("\(history.position + 1) of \(history.captures.count)").font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("Return to source", action: history.close)
            }
            Text("Back and Forward keep your place in up to three linked sources. The originating capture stays on the left.")
                .font(.caption).foregroundStyle(.secondary)
            ZStack(alignment: .topLeading) {
                ForEach(history.captures) { capture in
                    linkedDestination(capture)
                        .opacity(history.current === capture ? 1 : 0)
                        .allowsHitTesting(history.current === capture)
                        .disabled(history.current !== capture)
                        .accessibilityHidden(history.current !== capture)
                }
            }
        }.background(AtlasDocumentLinkFocus(capture: history.current?.id)
            .allowsHitTesting(false).accessibilityHidden(true))
    }
    private func linkedDestination(_ capture: AtlasProjectBrowserCapture) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(verbatim: capture.file.display).font(.headline).textSelection(.enabled)
            if capture.fragment.status != .none {
                Text(verbatim: capture.fragment.navigationDescription).font(.caption)
                    .foregroundStyle(capture.fragment.isFallback ? Color.orange : Color.secondary)
                    .textSelection(.enabled)
            }
            AtlasPagedSourceView(model: capture.reader, canKeep: canKeep, keep: keep,
                openLocalLink: navigation(from: capture))
        }
    }
    private func navigation(from source: AtlasProjectBrowserCapture) -> AtlasDocumentPreviewModel.LocalNavigation? {
        guard !history.busy, model.canOperate,
              source.catalogOwner == model.info?.owner || model.canOpenVerifiedDocumentLinks else { return nil }
        return { target in open(target, from: source) }
    }
    private func open(_ target: AtlasReaderDocumentLinkTarget, from source: AtlasProjectBrowserCapture) {
        guard !history.busy, model.canOperate,
              source === origin || history.current === source else { return }
        let browser = model
        history.bind(root: browser.root, accessIdentity: browser.rootAccessIdentity)
        history.follow {
            try await browser.openRetainedDocumentLink(target, from: source, readerTransport: .native,
                search: .native, outline: .native, document: .native)
        }
    }
    private func projectChanged() {
        guard !model.closed, model.info != nil else { history.close(); return }
        if history.bind(root: model.root, accessIdentity: model.rootAccessIdentity) {
            history.catalogRefreshed()
        }
    }
}
