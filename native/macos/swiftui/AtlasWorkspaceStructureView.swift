import SwiftUI

/// A progressive project query owns its immutable scope and retained sources.
/// Result rows carry the accepted page identity into their activation callback.
@MainActor struct AtlasWorkspaceStructureView: View {
    let presentation: AtlasWorkspaceStructurePresentation
    let desk: AtlasReadingDeskModel
    @State private var model: AtlasWorkspaceStructureModel
    @State private var pins = AtlasPinnedReaders<AtlasPagedReaderModel>(
        canFork: { $0.canPin }, fork: { try await $0.fork() }, retire: { $0.close() },
        failureMessage: { ($0 as? AtlasReaderError)?.message ?? "The structural search source could not be pinned." })
    @State private var keepingSource = false
    @Environment(\.dismiss) private var dismiss
    @FocusState private var queryFocused: Bool

    init(presentation: AtlasWorkspaceStructurePresentation, desk: AtlasReadingDeskModel) {
        self.presentation = presentation; self.desk = desk
        _model = State(initialValue: presentation.model)
    }
    private var pinGroup: String { "workspace-structure:\(presentation.id.uuidString)" }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label("Project symbols and headings", systemImage: "list.bullet.rectangle")
                    .font(.headline)
                Spacer()
                Button("Done") { close(); dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            Text(verbatim: "Project: " + String(reflecting: presentation.source.root))
                .font(.system(.caption, design: .monospaced)).textSelection(.enabled).lineLimit(2)
            Text(verbatim: "Search scope: " + presentation.source.scopeDescription)
                .font(.callout).textSelection(.enabled).lineLimit(3)
            Text(model.discoveryPolicy.includesExcluded
                ? "Includes ignored files within this fixed location and file-type scope."
                : "Applies repository ignore rules within this fixed location and file-type scope.")
                .font(.caption).foregroundStyle(.secondary)
            queryControls
            Text("Discovery continues beyond the atlas file-count limit. Turn off Match case to compare names using Unicode lowercase. Symbols use complete files up to 64 KiB; Markdown headings use complete files up to 256 KiB. Larger or unsupported files remain explicit incomplete coverage.")
                .font(.caption).foregroundStyle(.secondary)
            if let page = model.page {
                coverage(page)
            }
            HSplitView {
                results.frame(minWidth: 300, idealWidth: 390, maxWidth: 480, maxHeight: .infinity)
                reader.frame(minWidth: 520, maxWidth: .infinity, maxHeight: .infinity)
            }
            Text(verbatim: model.notice).font(.callout).foregroundStyle(.secondary)
                .textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
            if keepingSource {
                Text(verbatim: desk.notice).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            }
        }
        .padding(16).frame(minWidth: 1040, idealWidth: 1400, minHeight: 720, idealHeight: 820)
        .onAppear { pins.bind(root: pinGroup); presentation.start(); queryFocused = true }
        .onChange(of: model.closed) { _, closed in if closed { pins.closeAll() } }
        .onDisappear { close() }
    }

    private var queryControls: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Picker("Search mode", selection: $model.mode) {
                    ForEach(AtlasWorkspaceStructureMode.allCases) { mode in Text(mode.label).tag(mode) }
                }.frame(width: 160)
                Picker("Name matching", selection: $model.nameMode) {
                    ForEach(AtlasSearchNameMode.allCases) { mode in Text(mode.label).tag(mode) }
                }.frame(width: 170)
                TextField(model.mode == .symbols ? "Declaration name" : "Markdown heading title", text: $model.queryText)
                    .textFieldStyle(.roundedBorder).focused($queryFocused)
                    .onSubmit { if model.canSearch { model.search() } }
                    .accessibilityLabel("Project structure name query")
                Button(model.running ? "Restart search" : "Search", action: model.search)
                    .disabled(!model.canSearch).keyboardShortcut(.defaultAction)
            }
            HStack(spacing: 8) {
                Toggle("Match case", isOn: $model.caseSensitive)
                    .toggleStyle(.checkbox)
                    .help("When off, names use Unicode scalar lowercase. Original source spelling is preserved; full case folding and canonical normalization are not applied.")
                Picker("Total source bytes", selection: $model.scanBytes) {
                    Text("256 MiB").tag(UInt64(256 * 1024 * 1024))
                    Text("1 GiB").tag(UInt64(1024 * 1024 * 1024))
                    Text("4 GiB").tag(UInt64(4 * 1024 * 1024 * 1024))
                }.frame(width: 240)
                    .help("The total read allowance for one query, separate from per-file analysis and retained-result limits")
                if model.busy { ProgressView().controlSize(.small).accessibilityLabel("Searching project structure") }
                if model.running || model.pausing {
                    Button(model.pausing ? "Pausing…" : "Pause", action: model.pause)
                        .disabled(!model.canPause)
                        .help("Pause after the current bounded operation to browse results; Continue uses the same search")
                } else if model.page?.summary.more == true {
                    Button("Continue", action: model.resume).disabled(!model.canContinue)
                }
                if model.page?.summary.more == true || model.running || model.stopping {
                    Button(model.stopping ? "Stopping…" : "Stop and keep results", action: model.stopAndKeep)
                        .disabled(!model.canStopAndKeep)
                        .help("End the current observation with its accepted results and explicit partial coverage")
                }
                if model.busy { Button("Cancel", action: model.cancel) }
                Spacer()
            }
        }
    }

    private func coverage(_ page: AtlasWorkspaceStructurePage) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(verbatim: "Accepted " + page.summary.request.mode.label.lowercased() + " query: "
                + String(reflecting: page.summary.request.needle)
                + (page.summary.request.caseSensitive ? " · Match case" : " · Unicode lowercase"))
                .font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            if !model.authorized {
                Label("These results belong to the earlier query. Search again to activate results for the current inputs.",
                    systemImage: "info.circle")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Text(verbatim: page.summary.coverage).font(.callout).textSelection(.enabled)
            DisclosureGroup("Coverage details") {
                ScrollView {
                VStack(alignment: .leading, spacing: 4) {
                    Text(verbatim: page.summary.limitations)
                    Text("At most 256 result rows and 32 MiB of matching source are retained. Counts continue through the selected scan allowance after row storage fills.")
                    if !page.diagnostics.isEmpty {
                        Text(verbatim: page.diagnostics.joined(separator: "\n"))
                            .font(.system(.caption, design: .monospaced))
                    }
                    if page.diagnosticsTruncated { Text("Additional diagnostics were omitted from this bounded list.") }
                }.frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled)
                }.frame(maxHeight: 150)
            }.font(.caption).foregroundStyle(.secondary)
        }
    }

    private var results: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let page = model.page {
                if page.rows.isEmpty {
                    ContentUnavailableView {
                        Label(page.summary.more ? "Discovering matching names" : "No retained matches", systemImage: "magnifyingglass")
                    } description: {
                        Text(page.summary.more ? "Results appear as complete files are analyzed."
                            : "Check the coverage report before treating this as no matches in the project.")
                    }.frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    if model.running { Text("Pause to open or page through accepted results.").font(.caption).foregroundStyle(.secondary) }
                    List {
                        ForEach(page.rows) { hit in
                            let target = page.target(hit)
                            Button {
                                if let target { model.open(target) }
                            } label: {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(verbatim: hit.name).font(.body.weight(.semibold)).lineLimit(2)
                                    Text(verbatim: hit.displayPath).font(.system(.caption, design: .monospaced))
                                        .lineLimit(2).truncationMode(.middle)
                                    Text(hit.headingSlug == nil ? "\(hit.language) · \(hit.kind) · heuristic candidate" : "Markdown heading")
                                        .font(.caption).foregroundStyle(.secondary)
                                    Text("Candidate \(hit.occurrence) · original bytes \(hit.range.start)..<\(hit.range.end)")
                                        .font(.caption2).foregroundStyle(.secondary)
                                }.frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
                            }.buttonStyle(.plain).disabled(!model.canSelect || target == nil)
                                .accessibilityLabel("Read \(hit.name) in \(hit.displayPath)")
                        }
                    }.id(page.id).listStyle(.inset)
                }
                if page.summary.retainedHits > 0 {
                    Text("Retained results \(page.offset + (page.rows.isEmpty ? 0 : 1))–\(page.offset + UInt64(page.rows.count)) of \(page.summary.retainedHits)")
                        .font(.caption).foregroundStyle(.secondary)
                }
                HStack {
                    Button("Previous page", action: model.previousPage).disabled(!model.canPreviousPage)
                    Spacer()
                    Button("Next page", action: model.nextPage).disabled(!model.canNextPage)
                }.controlSize(.small)
                HStack {
                    Button("Earlier matches…", action: model.earlierWindow).disabled(!model.canEarlierWindow)
                    Spacer()
                    Button("Later matches…", action: model.laterWindow).disabled(!model.canLaterWindow)
                }.controlSize(.small)
                    .help("Start a new observation of current files for another bounded result window. Files and counts may have changed.")
                if page.summary.request.startAfter > 0 || model.canLaterWindow {
                    Text("Earlier/later matches starts a new scan of current files. Opened captures keep their original bytes.")
                        .font(.caption).foregroundStyle(.secondary)
                }
            } else {
                ContentUnavailableView {
                    Label("Find a declaration or heading", systemImage: "list.bullet.rectangle")
                } description: {
                    Text("Enter a name, choose a matching rule, then Search. Discovery and extraction use the selected project scope.")
                }.frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
    }

    @ViewBuilder private var reader: some View {
        if let capture = model.currentReader {
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Button(action: model.previousReader) { Label("Back", systemImage: "chevron.left") }
                        .disabled(!model.canPreviousReader)
                    Button(action: model.nextReader) { Label("Forward", systemImage: "chevron.right") }
                        .disabled(!model.canNextReader)
                    Text(verbatim: capture.hit.displayPath).font(.system(.callout, design: .monospaced))
                        .lineLimit(2).textSelection(.enabled)
                    Spacer()
                    Button("Close history", action: model.closeReader)
                        .help("Close the retained reading history. Pinned readers and Reading Desk copies stay open.")
                }.controlSize(.small)
                Text("\(model.readerHistorySummary) · candidate original bytes \(capture.hit.range.start)..<\(capture.hit.range.end)")
                    .font(.caption).foregroundStyle(.secondary)
                HSplitView {
                    AtlasPagedSourceView(model: capture.reader, canPin: !model.closed && pins.canAdd,
                        pin: { source in pins.pin(source, root: pinGroup, label: capture.hit.displayPath) },
                        canKeep: desk.canOperate, keep: keep)
                        .id(capture.id).frame(minWidth: 330, maxWidth: .infinity, maxHeight: .infinity)
                    if !pins.entries.isEmpty || pins.busy {
                        AtlasPinnedReadersView(readers: pins, canKeep: desk.canOperate, keep: keep)
                            .frame(minWidth: 330, idealWidth: 430)
                    }
                }
                Text("Back and Forward keep the last three sources. Pin sources to compare them. Keep sends your exact selection or current source window to the Reading Desk.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        } else if !pins.entries.isEmpty || pins.busy {
            AtlasPinnedReadersView(readers: pins, canKeep: desk.canOperate, keep: keep)
        } else {
            ContentUnavailableView {
                Label("Read the captured source", systemImage: "doc.text")
            } description: {
                Text("Pause or finish the search, then choose a result. The reader opens the exact captured file used for that match.")
            }.frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
    private func keep(_ source: AtlasPagedReaderModel) {
        guard !model.closed, desk.canOperate, source.canKeep, let domain = desk.readerDomain,
              source.retainedRegistryDomain == domain else { return }
        keepingSource = true
        desk.keep(reader: source)
    }
    private func close() {
        pins.closeAll(); presentation.close()
    }
}
