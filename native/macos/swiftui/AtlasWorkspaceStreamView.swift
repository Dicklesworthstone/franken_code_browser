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
            Text(verbatim: "Search scope: " + presentation.source.scopeDescription)
                .font(.callout).textSelection(.enabled).lineLimit(3)
            Text("Search text, expressions or whole identifiers in the selected location and file types, including large files. UTF-8 and BOM-declared UTF-16 are recognized automatically. No per-file byte cap applies; the total byte budget limits the scan.")
                .font(.caption).foregroundStyle(.secondary)
            Text("This sheet keeps the sidebar's location and file-type selection fixed. Return to the sidebar and reopen this sheet to choose another scope. Discovery uses product exclusions and stops at 20,000 files; excluded files do not consume the scan budget.")
                .font(.caption).foregroundStyle(.secondary)
            HStack(spacing: 8) {
                Picker("Search mode", selection: $model.mode) {
                    ForEach(AtlasWorkspaceStreamMode.allCases) { mode in Text(mode.label).tag(mode) }
                }.frame(width: 180)
                TextField(model.mode.queryPrompt, text: $model.queryText).textFieldStyle(.roundedBorder)
                    .focused($queryFocused).onSubmit { if model.canSearch { model.search() } }
                    .accessibilityLabel(model.mode.label + " query in the selected search scope")
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
            HStack(spacing: 8) {
                Toggle("Match case", isOn: Binding(get: { model.mode == .occurrences || model.caseSensitive },
                    set: { model.caseSensitive = $0 })).toggleStyle(.checkbox)
                    .disabled(model.mode == .occurrences)
                    .help(model.mode == .occurrences ? "Whole identifiers always match exact case."
                        : "When off, use Unicode lowercase matching while retaining the original spelling and byte ranges")
                Toggle("Count all matches", isOn: Binding(get: { model.effectiveCountAllMatches },
                    set: { model.countAllMatches = $0 })).toggleStyle(.checkbox)
                    .disabled(model.mode == .expression)
                    .help(model.mode == .expression
                        ? "Expressions must finish each file observation to verify required and excluded terms. Counting stays on within the byte and read-call limits."
                        : "Continue counting beyond the retained result window, within the selected byte and read-call limits")
                Text(model.mode == .expression
                    ? "Expressions finish each file observation and count primary matches; each scan retains up to 256 results. Stop remains available."
                    : model.effectiveCountAllMatches
                        ? "Scan past the result limit; keep up to 256 results per scan. Counts are exact only when the search completes."
                        : "The search stops when the result limit is reached.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if model.mode == .occurrences {
                Text("Occurrences match one complete identifier with exact case, including comments and literals. ASCII letters, digits, _ and $ are token characters; an identifier cannot start with an ASCII digit. Non-ASCII runs use conservative boundaries. These are text candidates, without compiler resolution.")
                    .font(.caption).foregroundStyle(.secondary)
            } else if model.mode == .expression {
                Text("Use literal terms, quoted phrases, -excluded terms, path: and lang: filters. The first positive literal supplies matches and highlights. Every required or excluded term is checked through the end of the same file observation before that file supplies hits. Regular expressions are unavailable.")
                    .font(.caption).foregroundStyle(.secondary)
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
                Text(model.mode == .occurrences
                    ? "Up to 256 retained results · whole-identifier text candidates"
                    : model.mode == .expression
                        ? "Up to 256 retained results · primary literal matches"
                        : "Up to 256 retained results · literal text, not regular expressions")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if let progress = model.progress {
                VStack(alignment: .leading, spacing: 4) {
                    ProgressView(value: Double(progress.summary.examinedFiles),
                        total: Double(max(1, progress.summary.searchedFiles)))
                    Text("Examined \(progress.summary.examinedFiles) of \(progress.summary.searchedFiles) search-eligible files · \(progress.summary.excludedFiles) excluded by location/type · read \(progress.summary.payloadBytes) bytes in \(progress.summary.readCalls) calls")
                        .font(.caption).foregroundStyle(.secondary)
                    if let expression = progress.summary.expression {
                        Text("Expression path/language filters excluded \(expression.excludedFiles) of \(progress.summary.eligibleFiles) location/type-eligible files.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
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
        .onChange(of: model.countAllMatches) { _, _ in copyNotice = "" }
        .onChange(of: model.caseSensitive) { _, _ in copyNotice = "" }
        .onChange(of: model.mode) { _, _ in copyNotice = "" }
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
            if let report = model.report,
               report.summary.pageHasMoreMatches || report.summary.request.pageStartAfter > 0 {
                HStack {
                    Button("Scan earlier matches", action: model.scanEarlier).disabled(!model.canScanEarlier)
                    Button("Scan later matches", action: model.scanLater).disabled(!model.canScanLater)
                }
                Text("Each window scans current files again. Files may have changed; the new results replace the previous observation.")
                    .font(.caption).foregroundStyle(.secondary)
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
                        Text("Showing \(progress.page.hits.count) matches from this window. All \(progress.summary.retainedHits) retained matches become available when the scan stops.")
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
                            ? "No matches were found in the observed contents of the eligible files in this scope."
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
                    description: Text("Enter text and press Search. Matching finalized files appear during the scan; stop and keep them when you have enough."))
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
                if let expression = report.summary.expression {
                    Text(verbatim: "Expression: " + expression.query).font(.caption).textSelection(.enabled)
                    Text("This is a primary-literal match from a file observation that satisfied the expression. Other predicate text was not retained.")
                        .font(.caption).foregroundStyle(.secondary)
                }
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
                if report.summary.expression != nil {
                    Text("Current context rechecks only these primary literal bytes. It does not recheck the expression's other required or excluded terms against the current file.")
                        .font(.caption).foregroundStyle(.secondary)
                }
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
