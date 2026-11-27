import SwiftUI

/// The project owner revokes this surface before replacing its root or grant.
/// Its independent readers and pins retain the exact captures they opened.
@MainActor final class AtlasProjectLinksPresentation: Identifiable {
    let id = UUID()
    let root: String
    let initialPath: [UInt8]?
    let model: AtlasProjectLinksModel
    init(root: String, initialPath: [UInt8]?, direction: AtlasProjectLinkDirection,
         projection: AtlasProjectRelationshipProjection = .documentation, access: AnyObject?) {
        self.root = root; self.initialPath = initialPath
        model = AtlasProjectLinksModel(root: root, initialPath: initialPath,
            direction: direction, projection: projection, access: access, transport: .native)
    }
    func close() { model.close() }
}

@MainActor struct AtlasProjectLinksView: View {
    let presentation: AtlasProjectLinksPresentation
    let desk: AtlasReadingDeskModel
    @State private var model: AtlasProjectLinksModel
    @State private var pathInput: String
    @State private var history = AtlasDocumentLinkHistory<AtlasProjectLinkReaderCapture>(
        retire: { $0.reader.close() },
        failureMessage: { ($0 as? AtlasProjectLinksError)?.message ?? ($0 as? AtlasReaderError)?.message
            ?? "The linked capture could not be opened. Your retained reading position is unchanged." })
    @State private var readerNotice = ""
    @State private var showsNeighborhood = true
    @State private var pins = AtlasPinnedReaders<AtlasPagedReaderModel>(
        canFork: { $0.canPin }, fork: { try await $0.fork() }, retire: { $0.close() },
        failureMessage: { ($0 as? AtlasReaderError)?.message ?? "The retained relationship source could not be pinned." })
    @Environment(\.dismiss) private var dismiss

    init(presentation: AtlasProjectLinksPresentation, desk: AtlasReadingDeskModel) {
        self.presentation = presentation; self.desk = desk
        _model = State(initialValue: presentation.model)
        _pathInput = State(initialValue: presentation.initialPath.flatMap { String(bytes: $0, encoding: .utf8) } ?? "")
    }
    private var canOperate: Bool { model.canOperate && !history.busy }
    private var prepareTitle: String {
        model.hasAcceptedProjection ? "Reload view" : (model.hasPreparedSources
            ? "Build \(model.projection.noun)" : "Capture and build \(model.projection.noun)")
    }
    private var reader: AtlasProjectLinkReaderCapture? { history.current }
    private var pinGroup: String { "project-links:\(presentation.id.uuidString)" }
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            filters
            if let progress = model.captureProgress, model.busy {
                Text(progress.summary).font(.caption).foregroundStyle(.secondary)
            }
            if let progress = model.relationshipProgress, model.busy {
                Label("Preparing \(progress.projection.noun)…", systemImage: "doc.text.magnifyingglass")
                    .font(.caption.weight(.semibold))
                Text(progress.summary).font(.caption).foregroundStyle(.secondary)
            }
            if let page = model.page {
                Label(page.building ? "Building \(page.projection.noun)…"
                    : (page.complete ? "\(page.projection.label) inventory complete" : "Partial \(page.projection.label.lowercased()) coverage"),
                    systemImage: page.building ? "doc.text.magnifyingglass"
                        : (page.complete ? "checkmark.circle" : "exclamationmark.circle"))
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(page.complete ? Color.secondary : Color.orange)
                Text(page.summary).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            }
            Divider()
            HSplitView {
                results.frame(minWidth: 360, idealWidth: 440, maxWidth: .infinity, maxHeight: .infinity)
                readerPane.frame(minWidth: 440, idealWidth: 630, maxWidth: .infinity, maxHeight: .infinity)
            }
            Divider()
            HStack {
                if model.busy || history.busy {
                    ProgressView().controlSize(.small)
                    Button("Cancel") { readerNotice = ""; history.cancel(); model.cancel() }
                }
                Text(readerNotice.isEmpty ? (history.notice.isEmpty ? model.notice : history.notice) : readerNotice)
                    .font(.caption).textSelection(.enabled)
                Spacer()
            }
            Text(model.projection.explanation)
                .font(.caption2).foregroundStyle(.secondary)
        }
        .padding(14).frame(minWidth: 1080, idealWidth: 1320, minHeight: 700, idealHeight: 840)
        .onAppear {
            pins.bind(root: pinGroup)
            history.bind(root: presentation.root, accessIdentity: ObjectIdentifier(presentation))
        }
        .onChange(of: model.selectedPath) { _, path in
            pathInput = path.flatMap { String(bytes: $0, encoding: .utf8) } ?? ""
        }
        .onDisappear { close() }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack {
                Label("Project relationships", systemImage: "link").font(.title2.weight(.semibold))
                Spacer()
                Button(prepareTitle) {
                    readerNotice = ""; model.prepare()
                }.disabled(!canOperate)
                    .help("Build an unprepared inventory, or reload its accepted view from the same captured bytes. Use Refresh sources to include file edits.")
                Button("Refresh sources") { readerNotice = ""; model.prepare(refresh: true) }
                    .disabled(!canOperate || !model.hasPreparedSources)
                    .help("Capture current project sources again. The current view stays available if refresh fails or is canceled. Opened readers and pins keep their previous captures.")
                Button("Done") { close(); dismiss() }.keyboardShortcut(.cancelAction)
            }
            Text(verbatim: presentation.root).font(.caption.monospaced()).lineLimit(2).textSelection(.enabled)
            Text("Capture the project once, then inspect documentation or imports within those retained bytes. Refresh sources to include edits.")
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    private var filters: some View {
        VStack(alignment: .leading, spacing: 6) {
            Picker("Relationship evidence", selection: Binding(get: { model.projection }, set: { value in
                readerNotice = ""; model.selectProjection(value)
            })) {
                ForEach(model.availableProjections) { projection in
                    Text(projection.label).tag(projection)
                }
            }
            .pickerStyle(.segmented).frame(maxWidth: 400).disabled(!canOperate)
            HStack {
                Button {
                    readerNotice = ""; model.goBack()
                } label: { Label("Back", systemImage: "chevron.left") }
                    .accessibilityLabel("Back in relationship history")
                    .help("Return to the previous relationship filter and results page.")
                    .disabled(!canOperate || !model.canGoBack)
                Button {
                    readerNotice = ""; model.goForward()
                } label: { Label("Forward", systemImage: "chevron.right") }
                    .accessibilityLabel("Forward in relationship history")
                    .help("Revisit the next relationship filter and results page.")
                    .disabled(!canOperate || !model.canGoForward)
                TextField("Exact project-relative path, such as src/lib.rs", text: $pathInput)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { selectTypedPath() }
                    .accessibilityLabel("Exact project-relative path for relationships")
                Button("Use path", action: selectTypedPath)
                    .disabled(!canOperate || pathInput.isEmpty)
                Button("Whole project") { readerNotice = ""; model.selectPath(nil) }
                    .disabled(!canOperate || model.selectedPath == nil)
            }
            HStack {
                Picker("Relationship direction", selection: Binding(get: { model.direction }, set: { value in
                    readerNotice = ""; model.filter(direction: value)
                })) {
                    ForEach([AtlasProjectLinkDirection.all, .outgoing, .incoming], id: \.rawValue) { direction in
                        Text(model.projection.directionLabel(direction)).tag(direction)
                            .disabled(direction != .all && model.selectedPath == nil)
                    }
                }.pickerStyle(.segmented).frame(maxWidth: 400).disabled(!canOperate)
                Spacer()
                Text(model.selectedPath == nil ? "All project \(model.projection.noun)" : "Exact selected file")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if let path = model.selectedPath, String(bytes: path, encoding: .utf8) == nil {
                Text("The selected filename contains non-UTF-8 bytes. Its original bytes remain the filter; entering a new path replaces it.")
                    .font(.caption2).foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder private var results: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let page = model.page {
                if let path = page.path {
                    Text(verbatim: path.display).font(.caption.monospaced()).lineLimit(2).textSelection(.enabled)
                }
                if !page.building, page.projection == model.projection, page.direction == model.direction,
                   page.path?.bytes == model.selectedPath, let neighborhood = page.neighborhood {
                    DisclosureGroup("Direct neighborhood", isExpanded: $showsNeighborhood) {
                        AtlasProjectLinksMapView(neighborhood: neighborhood, projection: page.projection) { path in
                            guard canOperate, model.page?.id == page.id else { return }
                            readerNotice = ""; model.selectPath(path, direction: .all)
                        }
                        .disabled(!canOperate)
                        .padding(.top, 6)
                    }
                    .font(.caption.weight(.semibold))
                }
                if page.building || page.projection != model.projection || page.direction != model.direction || page.path?.bytes != model.selectedPath {
                    ContentUnavailableView("Preparing \(model.projection.noun)…", systemImage: "doc.text.magnifyingglass",
                        description: Text("The captured project inventory is being built. This file and direction filter will be applied when it is ready."))
                } else if page.links.isEmpty {
                    ContentUnavailableView("No retained \(page.projection.noun) in this scope",
                        systemImage: "doc.text.magnifyingglass", description: Text(page.complete
                            ? (page.projection == .documentation ? "No parsed Markdown links match this file and direction." : "No import statements in the supported lexical inventory match this file and direction.")
                            : "Coverage is partial. Uncaptured, unsupported, or unparsed sources may contain additional relationships."))
                } else {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 8) {
                            ForEach(page.links) { link in linkRow(link, page: page) }
                        }
                    }
                }
                HStack {
                    Button("Previous") {
                        readerNotice = ""; if let previousPageStart = model.previousPageStart { model.loadPage(start: previousPageStart) }
                    }
                        .disabled(!canOperate || model.previousPageStart == nil)
                    Spacer()
                    Text("\(page.start + UInt64(page.links.isEmpty ? 0 : 1))–\(page.start + UInt64(page.links.count)) of \(page.total)")
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button("Next") { readerNotice = ""; if let next = page.next { model.loadPage(start: next) } }
                        .disabled(!canOperate || page.next == nil)
                }.controlSize(.small)
            } else {
                VStack(alignment: .leading, spacing: 12) {
                    Text(model.busy ? "Capturing project sources…" : (model.projection == .imports
                        ? "Follow imports through the captured project" : "Find the documentation behind a source file"))
                        .font(.headline)
                    Text(model.projection == .documentation
                        ? "Build documentation links, then choose Backlinks for a source path or Outgoing for a Markdown document."
                        : "Build import candidates, then choose Outgoing for declarations in a file or Imported by for catalog candidates targeting it. Read each exact statement before following its target.")
                    Text(model.projection == .documentation
                        ? "Capture admits up to 4,096 files, 1 MiB per file and 32 MiB total. Link extraction reads retained Markdown up to 256 KiB per document and keeps at most 4,096 links. Omitted documents remain visible in coverage."
                        : "Follow captured Rust, JavaScript/TypeScript, Python, C/C++, Go and Java import evidence. Go uses captured go.mod metadata; Java uses conventional source paths. Build constraints, classpaths and compiler bindings are not established. Up to 1 MiB of original bytes per source and 4,096 occurrences are admitted. Targets remain catalog candidates, without package loading or compiler resolution.")
                        .font(.caption).foregroundStyle(.secondary)
                    Text("File filters and visible-map selection do not narrow this project capture.")
                        .font(.caption).foregroundStyle(.secondary)
                }.padding(.top, 12).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
        }.padding(.trailing, 8)
    }

    private func linkRow(_ link: AtlasProjectLink, page: AtlasProjectLinksPage) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(verbatim: link.source.path.display).font(.caption.weight(.semibold)).lineLimit(2)
            Text(verbatim: link.destination.isEmpty ? "(empty destination)" : link.destination)
                .font(.callout).lineLimit(4).textSelection(.enabled)
            Text(link.label).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            if link.projection == .imports {
                Text(link.evidenceLabel).font(.caption2).foregroundStyle(.secondary).textSelection(.enabled)
            }
            if let path = link.targetPath {
                Text(verbatim: path.display).font(.caption.monospaced()).lineLimit(2).textSelection(.enabled)
            }
            HStack {
                Button(link.projection == .imports ? "Read import evidence" : "Read source document") { open(link, page: page, side: .evidence) }
                Button(link.projection == .imports ? "Read target candidate" : "Read captured target") { open(link, page: page, side: .target) }
                    .disabled(!link.canOpenTarget)
            }
            HStack {
                Button(link.projection == .imports ? "Imports from source" : "Links from source") {
                    readerNotice = ""; model.selectPath(link.source.path.bytes, direction: .outgoing)
                }
                if let path = link.targetPath {
                    Button(link.projection == .imports ? "Imported by" : "Backlinks to target") {
                        readerNotice = ""; model.selectPath(path.bytes, direction: .incoming)
                    }
                }
            }
            .font(.caption)
        }
        .controlSize(.small).disabled(!canOperate || page.building)
        .padding(10).frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.secondary.opacity(0.06), in: RoundedRectangle(cornerRadius: 7))
    }

    @ViewBuilder private var readerPane: some View {
        if let capture = reader {
            VStack(alignment: .leading, spacing: 7) {
                HStack {
                    Button { readerNotice = ""; history.back() } label: { Label("Back", systemImage: "chevron.left") }
                        .disabled(!history.canBack || !model.canOperate)
                    Button { readerNotice = ""; history.forward() } label: { Label("Forward", systemImage: "chevron.right") }
                        .disabled(!history.canForward || !model.canOperate)
                    Text(verbatim: capture.path.display).font(.headline).lineLimit(2).textSelection(.enabled)
                    Spacer()
                    Button("Close reader") {
                        readerNotice = ""; history.close()
                        history.bind(root: presentation.root, accessIdentity: ObjectIdentifier(presentation))
                    }
                }.controlSize(.small)
                Text(capture.evidenceLabel).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                HSplitView {
                    AtlasPagedSourceView(model: capture.reader, canPin: pins.canAdd, pin: { source in
                        pins.pin(source, root: pinGroup, label: capture.path.display)
                    }, canKeep: desk.canOperate, keep: keep,
                        openLocalLink: localNavigation(from: capture))
                    .id(capture.id)
                    .frame(minWidth: 380, maxWidth: .infinity, maxHeight: .infinity)
                    if !pins.entries.isEmpty || pins.busy {
                        AtlasPinnedReadersView(readers: pins).frame(minWidth: 330, idealWidth: 430)
                    }
                }
            }.padding(.leading, 8)
        } else if !pins.entries.isEmpty || pins.busy {
            AtlasPinnedReadersView(readers: pins)
        } else {
            ContentUnavailableView("Read the captured evidence", systemImage: "doc.text",
                description: Text("Open a source statement, document, or captured target. Pin one side to keep its evidence beside the next source."))
        }
    }

    private func selectTypedPath() {
        guard canOperate, !pathInput.isEmpty else { return }
        readerNotice = ""; model.selectPath(Array(pathInput.utf8))
    }
    private func open(_ link: AtlasProjectLink, page: AtlasProjectLinksPage, side: AtlasProjectLinkSide) {
        guard canOperate, !page.building else { return }
        readerNotice = ""
        history.follow {
            try await model.open(link, from: page, side: side,
                readerTransport: .native, search: .native, outline: .native, document: .native)
        }
    }

    private func localNavigation(from source: AtlasProjectLinkReaderCapture) -> AtlasDocumentPreviewModel.LocalNavigation? {
        guard canOperate, reader?.id == source.id, model.canFollowDocumentLinks(from: source) else { return nil }
        return { target in
            guard canOperate, reader?.id == source.id, model.canFollowDocumentLinks(from: source) else { return }
            readerNotice = ""
            history.follow {
                try await model.openDocumentLink(target, from: source,
                    readerTransport: .native, search: .native, outline: .native, document: .native)
            }
        }
    }

    private func keep(_ source: AtlasPagedReaderModel) {
        guard desk.canOperate, source.canKeep, let domain = desk.readerDomain,
              source.retainedRegistryDomain == domain else { return }
        desk.keep(reader: source)
        readerNotice = "Keeping the exact selection or current source window in the Reading Desk."
    }
    private func close() {
        history.close(); pins.closeAll(); presentation.close()
    }
}
