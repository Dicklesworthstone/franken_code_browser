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
    @State private var model: AtlasProjectLinksModel
    @State private var pathInput: String
    @State private var reader: AtlasProjectLinkReaderCapture?
    @State private var readerTask: Task<Void, Never>?
    @State private var readerTicket = UUID()
    @State private var readerNotice = ""
    @State private var paginationScope: PaginationScope?
    @State private var pageStarts: [UInt64] = []
    @State private var pins = AtlasPinnedReaders<AtlasPagedReaderModel>(
        canFork: { $0.canPin }, fork: { try await $0.fork() }, retire: { $0.close() },
        failureMessage: { ($0 as? AtlasReaderError)?.message ?? "The retained relationship source could not be pinned." })
    @Environment(\.dismiss) private var dismiss

    private struct PaginationScope: Equatable {
        let owner, index, generation: UInt64
        let direction: AtlasProjectLinkDirection
        let projection: AtlasProjectRelationshipProjection
        let path: [UInt8]?
        init(_ page: AtlasProjectLinksPage) {
            owner = page.basis.owner; index = page.basis.index; generation = page.generation
            direction = page.direction; projection = page.projection; path = page.path?.bytes
        }
    }

    init(presentation: AtlasProjectLinksPresentation) {
        self.presentation = presentation
        _model = State(initialValue: presentation.model)
        _pathInput = State(initialValue: presentation.initialPath.flatMap { String(bytes: $0, encoding: .utf8) } ?? "")
    }
    private var canOperate: Bool { model.canOperate && readerTask == nil }
    private var pinGroup: String { "project-links:\(presentation.id.uuidString)" }
    private var previousPageStart: UInt64? {
        guard let page = model.page, paginationScope == PaginationScope(page),
              let current = pageStarts.firstIndex(of: page.start), current > 0 else { return nil }
        return pageStarts[current - 1]
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            header
            filters
            if let progress = model.captureProgress, model.busy {
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
                if model.busy || readerTask != nil {
                    ProgressView().controlSize(.small)
                    Button("Cancel") { readerNotice = ""; cancelReader(); model.cancel() }
                }
                Text(readerNotice.isEmpty ? model.notice : readerNotice)
                    .font(.caption).textSelection(.enabled)
                Spacer()
            }
            Text(model.projection.explanation)
                .font(.caption2).foregroundStyle(.secondary)
        }
        .padding(14).frame(minWidth: 1080, idealWidth: 1320, minHeight: 700, idealHeight: 840)
        .onAppear { pins.bind(root: pinGroup) }
        .onChange(of: model.selectedPath) { _, path in
            pathInput = path.flatMap { String(bytes: $0, encoding: .utf8) } ?? ""
        }
        .onChange(of: model.page?.id) { _, _ in rememberPage() }
        .onDisappear { close() }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack {
                Label("Project relationships", systemImage: "link").font(.title2.weight(.semibold))
                Spacer()
                Button(model.hasPreparedSources ? "Rebuild \(model.projection.noun)" : "Capture and build \(model.projection.noun)") {
                    readerNotice = ""; model.prepare()
                }.disabled(!canOperate)
                Button("Refresh sources") { readerNotice = ""; model.prepare(refresh: true) }
                    .disabled(!canOperate || !model.hasPreparedSources)
                    .help("Capture current project sources again. Already opened readers and pins keep their previous captures.")
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
                        readerNotice = ""; if let previousPageStart { model.loadPage(start: previousPageStart) }
                    }
                        .disabled(!canOperate || previousPageStart == nil)
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
                        : "Import extraction examines retained Rust module/use declarations and JavaScript/TypeScript static imports and re-exports, including JSX/TSX. It admits up to 128 KiB per source and 4,096 statements within the bounded project capture. Package loading and compiler resolution are not performed.")
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
                    Text(verbatim: capture.path.display).font(.headline).lineLimit(2).textSelection(.enabled)
                    Spacer()
                    Button("Close reader") { readerNotice = ""; cancelReader(); reader?.reader.close(); reader = nil }
                }.controlSize(.small)
                Text(capture.evidenceLabel).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                HSplitView {
                    AtlasPagedSourceView(model: capture.reader, canPin: pins.canAdd, pin: { source in
                        pins.pin(source, root: pinGroup, label: capture.path.display)
                    })
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
    private func rememberPage() {
        guard let page = model.page else { paginationScope = nil; pageStarts = []; return }
        let scope = PaginationScope(page)
        if paginationScope != scope {
            paginationScope = scope; pageStarts = [page.start]
        } else if let position = pageStarts.firstIndex(of: page.start) {
            pageStarts = Array(pageStarts.prefix(position + 1))
        } else if pageStarts.count < 4096 {
            // Response-byte limits can shorten pages. Remember actual starts
            // instead of subtracting the requested page size from an offset.
            pageStarts.append(page.start)
        }
    }
    private func open(_ link: AtlasProjectLink, page: AtlasProjectLinksPage, side: AtlasProjectLinkSide) {
        guard canOperate, !page.building else { return }
        readerNotice = "Opening the retained \(side == .evidence ? "source document" : "target")…"
        let ticket = UUID(); readerTicket = ticket
        readerTask = Task { @MainActor in
            do {
                let opened = try await model.open(link, from: page, side: side,
                    readerTransport: .native, search: .native, outline: .native, document: .native)
                guard readerTicket == ticket, !Task.isCancelled, !model.closed else { opened.reader.close(); return }
                reader?.reader.close(); reader = opened
                readerNotice = "Opened the retained source without rereading the project file."
            } catch {
                guard readerTicket == ticket, !Task.isCancelled, !model.closed else { return }
                readerNotice = (error as? AtlasProjectLinksError)?.message ?? (error as? AtlasReaderError)?.message
                    ?? "The relationship could not open its captured source. Rebuild links to refresh the available evidence."
            }
            if readerTicket == ticket { readerTask = nil }
        }
    }
    private func cancelReader() {
        readerTicket = UUID(); readerTask?.cancel(); readerTask = nil
    }
    private func close() {
        cancelReader(); reader?.reader.close(); reader = nil; pins.closeAll(); presentation.close()
    }
}
