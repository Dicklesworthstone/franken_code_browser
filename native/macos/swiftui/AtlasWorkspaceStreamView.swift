import SwiftUI
import AppKit

@MainActor struct AtlasWorkspaceStreamView: View {
    let presentation: AtlasWorkspaceStreamPresentation
    @State private var model: AtlasWorkspaceStreamModel
    @State private var copyNotice = ""
    @Environment(\.dismiss) private var dismiss
    @FocusState private var queryFocused: Bool

    init(presentation: AtlasWorkspaceStreamPresentation) {
        self.presentation = presentation; _model = State(initialValue: presentation.model)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label("Search large project files", systemImage: "doc.text.magnifyingglass").font(.headline)
                Spacer()
                Button("Refresh file catalog", action: model.refreshCatalog).disabled(!model.canRefresh)
                    .help("Discover the current project again, including files added since this catalog was opened")
                Button("Done") { presentation.close(); dismiss() }.keyboardShortcut(.cancelAction)
            }
            Text(verbatim: "Project: " + String(reflecting: presentation.source.root))
                .font(.system(.caption, design: .monospaced)).textSelection(.enabled).lineLimit(2)
            Text("Exact, case-sensitive text across all catalogued files, including large files. UTF-8 and BOM-declared UTF-16 are recognized automatically. No per-file byte cap applies; the total byte budget limits the scan.")
                .font(.caption).foregroundStyle(.secondary)
            Text("Discovery uses the product's file exclusions and stops at 20,000 files. This scope is independent of the visible map, current zoom, and filters.")
                .font(.caption).foregroundStyle(.secondary)
            HStack(spacing: 8) {
                TextField("Exact text to find", text: $model.queryText).textFieldStyle(.roundedBorder)
                    .focused($queryFocused).onSubmit { if model.canSearch { model.search() } }
                    .accessibilityLabel("Exact text to find in all catalogued files")
                Picker("Total bytes", selection: $model.scanBytes) {
                    Text("256 MiB").tag(UInt64(256 * 1024 * 1024))
                    Text("1 GiB").tag(UInt64(1024 * 1024 * 1024))
                    Text("4 GiB").tag(UInt64(4 * 1024 * 1024 * 1024))
                }.frame(width: 200)
                Button(model.searching ? "Restart search" : "Search", action: model.search)
                    .disabled(!model.canSearch).keyboardShortcut(.defaultAction)
                if model.busy {
                    ProgressView().controlSize(.small)
                    if model.canStopAndKeep || model.stopping {
                        Button(model.stopping ? "Stopping…" : "Stop and keep matches", action: model.stopAndKeep)
                            .disabled(!model.canStopAndKeep)
                            .help("Finish with matches from finalized file observations; omit matches in the unfinished file")
                    }
                    Button(model.searching ? "Cancel and discard" : "Cancel", action: model.cancel)
                }
            }
            HStack {
                if let catalog = model.catalog {
                    Text("\(catalog.files) catalogued files · \(catalog.discoveryComplete ? "discovery complete under product exclusions" : "discovery incomplete")")
                        .font(.caption).foregroundStyle(.secondary)
                } else {
                    Text(model.opening ? "Discovering project files…" : "No accepted file catalog")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                Text("Up to 256 matches · literal text, not regular expressions")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if let progress = model.progress {
                VStack(alignment: .leading, spacing: 4) {
                    ProgressView(value: Double(progress.summary.examinedFiles),
                        total: Double(max(1, progress.summary.cataloguedFiles)))
                    Text("Examined \(progress.summary.examinedFiles) of \(progress.summary.cataloguedFiles) files · read \(progress.summary.payloadBytes) bytes in \(progress.summary.readCalls) calls")
                        .font(.caption).foregroundStyle(.secondary)
                    Text("Matches from finalized files appear below. The unfinished file contributes byte progress; its matches remain pending.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                .accessibilityElement(children: .combine)
            }
            Divider()
            HSplitView {
                results.frame(minWidth: 340, idealWidth: 450, maxWidth: .infinity, maxHeight: .infinity)
                selectedMatch.frame(minWidth: 340, idealWidth: 480, maxWidth: .infinity, maxHeight: .infinity)
            }
            Divider()
            Text(model.notice).font(.caption).textSelection(.enabled)
            if !copyNotice.isEmpty { Text(copyNotice).font(.caption).foregroundStyle(.secondary) }
            if let coverage = model.coverage {
                Text(coverage).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            }
        }
        .padding(14).frame(minWidth: 920, idealWidth: 1140, minHeight: 700, idealHeight: 840)
        .onAppear { presentation.start(); queryFocused = true }
        .onDisappear { presentation.close() }
        .onChange(of: model.queryText) { _, _ in copyNotice = "" }
        .onChange(of: model.scanBytes) { _, _ in copyNotice = "" }
        .onChange(of: model.witness?.target) { _, _ in copyNotice = "" }
        .sheet(item: Binding(get: { model.currentContext }, set: { if $0 == nil { model.closeContext() } })) { context in
            AtlasObservedReaderView(presentation: context)
        }
    }

    @ViewBuilder private var results: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(model.progress?.countLabel ?? model.report?.summary.countLabel ?? "Results")
                    .font(.subheadline.weight(.semibold))
                Spacer()
                Button("Clear", action: model.clear).disabled(model.report == nil)
            }
            if let progress = model.progress {
                Text("Preview while searching. Stop and keep matches, or wait for completion, to select a retained result.")
                    .font(.caption).foregroundStyle(.secondary)
                if progress.page.hits.isEmpty {
                    ContentUnavailableView("No finalized-file matches yet", systemImage: "magnifyingglass",
                        description: Text("The search is still running. Unfinished and unexamined contents may contain matches."))
                } else {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 6) {
                            ForEach(progress.page.hits) { hit in
                                hitDescription(hit)
                                    .frame(maxWidth: .infinity, alignment: .leading).padding(8)
                                    .background(Color.secondary.opacity(0.06))
                                    .clipShape(RoundedRectangle(cornerRadius: 5))
                                    .accessibilityElement(children: .combine)
                            }
                        }
                    }
                    if progress.summary.retainedHits > UInt64(progress.page.hits.count) {
                        Text("Showing the first \(progress.page.hits.count) matches. All \(progress.summary.retainedHits) retained matches become available when the scan stops.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
            } else if let report = model.report {
                if !model.reportAuthorized {
                    Text("Previous results are read-only. Search again to select or open them.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                if report.hits.isEmpty {
                    ContentUnavailableView("No retained matches", systemImage: "magnifyingglass",
                        description: Text(report.summary.complete
                            ? "No matches were found in the observed contents of all catalogued files."
                            : "This partial search cannot establish that the unsearched or unavailable contents have no matches."))
                } else {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 6) {
                            ForEach(report.hits) { hit in
                                Button { model.select(report.target(for: hit)) } label: {
                                    hitDescription(hit)
                                    .frame(maxWidth: .infinity, alignment: .leading).padding(8)
                                    .background(model.witness?.target == report.target(for: hit)
                                        ? Color.accentColor.opacity(0.16) : Color.secondary.opacity(0.06))
                                    .clipShape(RoundedRectangle(cornerRadius: 5))
                                }
                                .buttonStyle(.plain).disabled(!model.canSelect)
                                .accessibilityLabel("Match \(hit.index + 1), \(hit.displayPath), original bytes \(hit.range.start) to \(hit.range.end)")
                            }
                        }
                    }
                }
            } else {
                ContentUnavailableView(model.busy ? "Searching…" : "Search the project catalog",
                    systemImage: "doc.text.magnifyingglass",
                    description: Text("Enter exact text and press Search. Matching finalized files appear during the scan; stop and keep them when you have enough."))
            }
            Spacer(minLength: 0)
        }.padding(.trailing, 8)
    }

    private func hitDescription(_ hit: AtlasWorkspaceStreamHit) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(verbatim: hit.displayPath).lineLimit(2).font(.system(.body, design: .monospaced))
            Text("Bytes \(hit.range.start)..<\(hit.range.end) · \(hit.encoding.label)")
                .font(.caption).foregroundStyle(.secondary)
            if !hit.complete {
                Text("Partial file observation · \(hit.state) · \(hit.consistency)")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder private var selectedMatch: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let witness = model.witness, let report = model.report, report.admits(witness.target) {
                Text("Retained literal").font(.subheadline.weight(.semibold))
                Text(verbatim: witness.target.hit.displayPath).font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled).lineLimit(3)
                Text("Original bytes \(witness.target.hit.range.start)..<\(witness.target.hit.range.end) · \(witness.target.hit.encoding.label)")
                    .font(.caption).foregroundStyle(.secondary)
                Text("Only this exact matching text and its original bytes were retained. Surrounding source and a whole-file snapshot were not captured.")
                    .font(.caption).foregroundStyle(.secondary)
                ScrollView([.horizontal, .vertical]) {
                    Text(verbatim: witness.text).font(.system(.body, design: .monospaced)).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .topLeading)
                }.frame(minHeight: 100, maxHeight: .infinity)
                HStack {
                    Button("Copy match") { copy(witness, hex: false) }
                    Button("Copy original hex") { copy(witness, hex: true) }
                }
                Button("Read current context") { model.readCurrentContext(transport: .native) }
                    .disabled(!model.canReadCurrentContext)
                Text("Explicitly opens the current canonical pathname under this project's access grant. The retained literal must match the newly read original bytes before a highlight is applied.")
                    .font(.caption).foregroundStyle(.secondary)
            } else {
                ContentUnavailableView("Select a retained match", systemImage: "text.magnifyingglass",
                    description: Text("Selecting a result reads its retained literal without reopening the file. Current context is a separate explicit read."))
                Spacer(minLength: 0)
            }
        }.padding(.leading, 10)
    }

    private func copy(_ witness: AtlasWorkspaceStreamWitness, hex: Bool) {
        guard !model.closed, model.witness?.target == witness.target,
              model.report?.admits(witness.target) == true else { return }
        NSPasteboard.general.clearContents()
        if NSPasteboard.general.setString(hex ? witness.originalHex : witness.text, forType: .string) {
            copyNotice = hex ? "Copied exact retained original bytes as hex; no file was reread."
                : "Copied exact retained matching text; no file was reread."
        } else { copyNotice = "The pasteboard did not accept the retained match." }
    }
}
