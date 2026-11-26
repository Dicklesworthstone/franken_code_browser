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
    let canReadRanges: Bool
    let readRanges: (AtlasProjectBrowserFile, AtlasProjectBrowserPage) throws -> Void
    var searchProject: ((AtlasReaderSelectionQuery) -> Void)? = nil
    var canChangeDiscoveryPolicy = true
    var changeDiscoveryPolicy: ((AtlasDiscoveryPolicy) -> Void)? = nil
    var canFocusDirectory = false
    var focusDirectory: ((AtlasProjectBrowserFile, AtlasProjectBrowserPage) throws -> Void)? = nil
    var canFocusFile = false
    var focusFile: ((AtlasProjectBrowserFile, AtlasProjectBrowserPage) throws -> Void)? = nil
    var canBrowseDirectory = false
    var browseDirectory: ((AtlasDirectoryFocus, AtlasProjectBrowserDirectoryMove) -> Bool)? = nil
    @State private var capture: AtlasProjectBrowserCapture?
    @State private var readerTask: Task<Void, Never>?
    @State private var readerNotice = ""
    @State private var directoryInput = ""

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
                Button("Refresh project", action: refresh).disabled(model.busy)
            }
            if preparing { Text(progress).font(.caption).foregroundStyle(.secondary) }
            if model.supportsDirectoryBrowsing, browseDirectory != nil {
                directoryControls
            }
            if let changeDiscoveryPolicy, model.supportsDiscoveryPolicy {
                Toggle("Include ignored and generated files", isOn: Binding(
                    get: { model.discoveryPolicy.includesExcluded },
                    set: { changeDiscoveryPolicy($0 ? .includeExcluded : .repositoryRules) }))
                    .toggleStyle(.checkbox)
                    .disabled(!canChangeDiscoveryPolicy || !model.canOperate || preparing || readerTask != nil)
                    .help("Refresh Project Files and the atlas with the selected discovery policy. Existing readers keep their captured bytes.")
                if model.discoveryPolicy.includesExcluded {
                    Text("Project Files and focused atlas scopes include ignored and generated files. Quick Open and large-file search offer their own Include ignored control.")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            if let info = model.info {
                HStack {
                    Text(info.supportsDirectoryRows
                        ? "\(info.discovered) files discovered · \(info.cataloguedDirectories) subfolders"
                        : "\(info.discovered) files discovered").font(.callout.weight(.semibold))
                    Text(info.more ? (info.paused ? "Paused" : "More to discover")
                        : (info.complete ? (model.directory.isProject ? "Discovery complete" : "Folder discovery complete")
                            : "Discovery incomplete"))
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
                    Text(info.supportsDirectoryRows
                        ? "Page \(page.id) · \(page.directories.count) folders · \(entries.count) of \(page.files.count) files · \(scopeLabel)"
                        : "Page \(page.id) · \(entries.count) of \(page.files.count) files in this page · \(scopeLabel)")
                        .font(.caption).foregroundStyle(.secondary)
                    Divider()
                    if entries.isEmpty && page.directories.isEmpty {
                        ContentUnavailableView {
                            Label(page.files.isEmpty
                                ? (info.supportsDirectoryRows ? "No files or folders in this batch" : "No files in this batch")
                                : "No files match this filter", systemImage: "folder")
                        } description: {
                            Text(info.more
                                ? (info.supportsDirectoryRows
                                    ? "Continue discovery to reach more files and immediate subfolders, or change the current file filter. Nested directories and excluded entries can fill a batch."
                                    : "Directories and excluded entries can fill a batch. Continue discovery to reach more files, or change the current file filter.")
                                : (info.complete ? "Discovery has finished. Try another retained page or change the file filter."
                                    : "Discovery finished with omissions. Files outside the discovered pages are not known to be absent."))
                        }.frame(maxWidth: .infinity, maxHeight: .infinity)
                    } else {
                        List {
                            if !page.directories.isEmpty {
                                Section("Folders in this directory") {
                                    ForEach(page.directories) { folder in directoryRow(folder, from: page) }
                                }
                            }
                            if !entries.isEmpty {
                                Section("Files in this page") {
                                    ForEach(entries) { file in fileRow(file, from: page) }
                                }
                            }
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
                    Text(info.supportsDirectoryRows
                        ? "Folders are immediate children of the current directory; files can be anywhere below it. The latest four pages stay available. Refresh starts discovery again in this folder. Page order follows discovery; the file filter leaves folder navigation visible."
                        : "The latest four pages stay available as you continue; Refresh starts discovery again in the current folder. Page order follows discovery, and the file filter applies to the displayed page.")
                        .font(.caption2).foregroundStyle(.secondary)
                    Text("Click a filename to retain a complete capture up to 32 MiB. Read ranges opens the current file in bounded observations; earlier captures keep their original bytes.")
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
            Text("Browse a folder directly or continue beyond 20,000 files. Folder navigation keeps the text atlas and captured readers. Focus this file builds an exact single-file atlas. Workspace search reports its own coverage.")
                .font(.caption2).foregroundStyle(.secondary)
        }.padding(12).frame(maxWidth: .infinity, maxHeight: .infinity)
        .sheet(item: $capture) { capture in
            AtlasProjectCaptureView(model: model, origin: capture, canKeep: canKeep, keep: keep,
                searchProject: searchProject) {
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
        .onChange(of: model.directory, initial: true) { _, directory in
            directoryInput = String(bytes: directory.rawPath, encoding: .utf8) ?? ""
        }
    }
    private var canNavigateDirectory: Bool {
        canBrowseDirectory && model.canOperate && readerTask == nil && !preparing
    }
    private func directoryRow(_ folder: AtlasProjectBrowserDirectory, from page: AtlasProjectBrowserPage) -> some View {
        Button { browse(folder, from: page) } label: {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: "folder").foregroundStyle(.secondary)
                Text(verbatim: folder.display).font(.system(size: 12, design: .monospaced))
                    .lineLimit(2).frame(maxWidth: .infinity, alignment: .leading)
                if folder.withinBrowseLimits {
                    Image(systemName: "chevron.right").foregroundStyle(.secondary)
                } else {
                    Text("Browsing depth limit").font(.caption2).foregroundStyle(.orange)
                }
            }.contentShape(Rectangle())
        }.buttonStyle(.plain)
            .disabled(!canNavigateDirectory || browseDirectory == nil || !folder.withinBrowseLimits)
            .accessibilityLabel("Browse folder " + folder.display)
            .help(folder.withinBrowseLimits
                ? "Browse this folder under the same project, keeping open readers and the text atlas."
                : "This folder is visible at the 32-component discovery boundary. Browsing supports up to 31 components from the project root.")
    }
    private func fileRow(_ file: AtlasProjectBrowserFile, from page: AtlasProjectBrowserPage) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Button { open(file, from: page) } label: {
                HStack(alignment: .top) {
                    Image(systemName: "doc.text").foregroundStyle(.secondary)
                    Text(verbatim: file.display).font(.system(size: 12, design: .monospaced))
                        .lineLimit(2).frame(maxWidth: .infinity, alignment: .leading)
                    VStack(alignment: .trailing, spacing: 2) {
                        Text("\(file.observedBytes) bytes observed")
                        if file.observedBytes > AtlasReaderLimits.captureBytes {
                            Text("Read ranges recommended").foregroundStyle(.orange)
                        }
                    }.font(.caption2).foregroundStyle(.secondary)
                }.contentShape(Rectangle())
            }.buttonStyle(.plain).disabled(!model.canOperate || readerTask != nil)
                .accessibilityLabel("Open " + file.display)
                .help("Capture up to 32 MiB now and read it independently of discovery. Choose Read ranges for larger files.")
            Button("Read ranges") { observe(file, from: page) }
                .buttonStyle(.bordered).controlSize(.small)
                .disabled(!canReadRanges || !model.canOperate || readerTask != nil)
                .accessibilityLabel("Read current ranges of " + file.display)
                .help("Open bounded observations of this current project file, including files larger than 32 MiB")
        }.contextMenu {
            if browseDirectory != nil {
                Button("Browse containing folder") { browseContainingFolder(file, from: page) }
                    .disabled(!canNavigateDirectory
                        || (try? AtlasDirectoryFocus.containingFile(rawPath: file.rawPath)) == model.directory)
                    .help("Start bounded discovery directly in this folder, keeping the original project and open readers.")
            }
            if focusFile != nil {
                Button("Focus this file") { focusExactFile(file, from: page) }
                    .disabled(!canFocusFile || !model.canOperate || readerTask != nil)
                    .help("Build a single-file atlas, including a file beyond a large folder's atlas limit.")
            }
            if focusDirectory != nil {
                Button("Focus containing folder") { focus(file, from: page) }
                    .disabled(!canFocusDirectory || !model.canOperate || readerTask != nil)
                    .help("Focus the text atlas on this file's containing folder.")
            }
        }
    }
    private var inputDirectory: AtlasDirectoryFocus? {
        let raw = Array(directoryInput.utf8)
        guard raw.count <= AtlasDirectoryFocus.maximumPathBytes,
              raw.split(separator: 47).count <= AtlasProjectBrowserScope.maximumDirectoryComponents else { return nil }
        return try? AtlasDirectoryFocus(rawPath: raw)
    }
    private var directoryControls: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Button("Back") { if let directory = model.previousDirectories.last { navigate(directory, move: .back) } }
                    .disabled(!canNavigateDirectory || model.previousDirectories.isEmpty)
                Button("Forward") { if let directory = model.followingDirectories.last { navigate(directory, move: .forward) } }
                    .disabled(!canNavigateDirectory || model.followingDirectories.isEmpty)
                Button("Up") { if let parent = model.directory.parent { navigate(parent) } }
                    .disabled(!canNavigateDirectory || model.directory.isProject)
                Button("Whole project") { navigate(.project) }
                    .disabled(!canNavigateDirectory || model.directory.isProject)
                Spacer(minLength: 0)
            }.controlSize(.small)
            ScrollView(.horizontal) {
                HStack(spacing: 6) {
                    ForEach(model.directoryBreadcrumbs) { crumb in
                        if !crumb.directory.isProject {
                            Image(systemName: "chevron.right").font(.caption2).foregroundStyle(.secondary)
                        }
                        Button { navigate(crumb.directory) } label: {
                            Text(verbatim: crumb.title).font(.system(size: 12, design: .monospaced))
                        }.buttonStyle(.plain)
                            .disabled(!canNavigateDirectory || crumb.directory == model.directory)
                            .accessibilityLabel("Browse " + crumb.directory.display)
                            .help(crumb.directory.display)
                    }
                }.padding(.vertical, 2)
            }.scrollIndicators(.hidden)
            HStack {
                TextField("Folder relative to project root", text: Binding(get: { directoryInput }, set: { value in
                    // Keep one extra byte so oversized input is rejected, never
                    // silently opened as a truncated but valid directory.
                    directoryInput = String(decoding: value.utf8.prefix(AtlasDirectoryFocus.maximumPathBytes + 1), as: UTF8.self)
                })).textFieldStyle(.roundedBorder)
                    .disabled(!canNavigateDirectory)
                    .onSubmit { if let directory = inputDirectory { navigate(directory) } }
                    .accessibilityLabel("Project Files folder relative to project root")
                Button("Browse folder") { if let directory = inputDirectory { navigate(directory) } }
                    .disabled(!canNavigateDirectory || inputDirectory == nil || inputDirectory == model.directory)
            }
            if inputDirectory == nil {
                Text("Use a relative folder path with no empty, . or .. components, up to 2,048 bytes and 31 folders.")
                    .font(.caption2).foregroundStyle(.orange)
            } else if !model.directory.isProject {
                Text("Discovery covers this folder and its descendants. Paths stay relative to the project root; a complete folder does not establish whole-project coverage.")
                    .font(.caption2).foregroundStyle(.secondary)
            }
        }
    }
    private func navigate(_ directory: AtlasDirectoryFocus, move: AtlasProjectBrowserDirectoryMove = .visit) {
        guard canNavigateDirectory, directory != model.directory, let browseDirectory else { return }
        readerNotice = browseDirectory(directory, move) ? ""
            : "The folder could not be opened from this project page. Choose the folder again."
    }
    private func browseContainingFolder(_ file: AtlasProjectBrowserFile, from page: AtlasProjectBrowserPage) {
        guard canNavigateDirectory, let accepted = model.currentFile(file, from: page) else { return }
        do { navigate(try AtlasDirectoryFocus.containingFile(rawPath: accepted.rawPath)) }
        catch { readerNotice = "This containing folder exceeds the directory browsing limit." }
    }
    private func browse(_ folder: AtlasProjectBrowserDirectory, from page: AtlasProjectBrowserPage) {
        guard canNavigateDirectory, let accepted = model.currentDirectory(folder, from: page) else { return }
        do { navigate(try AtlasDirectoryFocus(rawPath: accepted.rawPath)) }
        catch { readerNotice = "This folder exceeds the directory browsing limit." }
    }
    private func focus(_ file: AtlasProjectBrowserFile, from page: AtlasProjectBrowserPage) {
        guard canFocusDirectory, readerTask == nil, let focusDirectory else { return }
        do {
            try focusDirectory(file, page)
            readerNotice = "Preparing the containing folder's atlas."
        } catch {
            readerNotice = (error as? AtlasProjectBrowserError)?.message
                ?? "This folder cannot be focused. Choose a current file page or a shallower folder."
        }
    }
    private func focusExactFile(_ file: AtlasProjectBrowserFile, from page: AtlasProjectBrowserPage) {
        guard canFocusFile, readerTask == nil, let focusFile else { return }
        do {
            try focusFile(file, page)
            readerNotice = "Preparing this file's atlas."
        } catch {
            readerNotice = (error as? AtlasProjectBrowserError)?.message
                ?? "This file cannot be focused. Choose a current file page with a path within the atlas limit."
        }
    }
    private func observe(_ file: AtlasProjectBrowserFile, from page: AtlasProjectBrowserPage) {
        guard canReadRanges, readerTask == nil else { return }
        do {
            try readRanges(file, page)
            readerNotice = "Opening the current file in the Observed Range Reader. Each range is a separate observation."
        } catch {
            readerNotice = (error as? AtlasProjectBrowserError)?.message
                ?? (error as? AtlasObservedReaderError)?.message ?? "This current file is unavailable. Choose it again from Project Files."
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
                readerNotice = (error as? AtlasProjectBrowserError)?.captureMessage
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
    let searchProject: ((AtlasReaderSelectionQuery) -> Void)?
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
                    openLocalLink: navigation(from: origin), searchProject: selectionSearch)
                    .id(origin.id).frame(minWidth: 400)
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
                openLocalLink: navigation(from: capture), searchProject: selectionSearch)
        }
    }
    private var selectionSearch: ((AtlasReaderSelectionQuery) -> Void)? {
        guard let searchProject else { return nil }
        return { query in
            searchProject(query)
            // Reveal the project's results instead of leaving them behind a
            // modal reader. The query owns its bounded selected spelling.
            done()
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
