import SwiftUI
import AppKit

@MainActor struct AtlasWorkspaceStreamView: View {
    let presentation: AtlasWorkspaceStreamPresentation
    let desk: AtlasReadingDeskModel
    @State private var model: AtlasWorkspaceStreamModel
    @State private var copyNotice = ""
    @State private var keptExcerpt: AtlasDeskStreamImport?
    @State private var priorPassages: Set<UInt64> = []
    @State private var observedDraft: AtlasDeskObservedNoteDraft?
    @Environment(\.dismiss) private var dismiss
    @FocusState private var queryFocused: Bool

    init(presentation: AtlasWorkspaceStreamPresentation, desk: AtlasReadingDeskModel) {
        self.presentation = presentation; self.desk = desk; _model = State(initialValue: presentation.model)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label("Search large project files", systemImage: "doc.text.magnifyingglass").font(.headline)
                Spacer()
                Button(presentation.source.pathScope.isVisible ? "Refresh file catalog" : "Reset search",
                    action: model.refreshCatalog).disabled(!model.canRefresh)
                    .help("Release this search's results and prepare a fresh discovery within the current search scope")
                Button("Done") { presentation.close(); dismiss() }.keyboardShortcut(.cancelAction)
            }
            Text(verbatim: "Project: " + String(reflecting: presentation.source.root))
                .font(.system(.caption, design: .monospaced)).textSelection(.enabled).lineLimit(2)
            Text(verbatim: "Search scope: " + presentation.source.scopeDescription)
                .font(.callout).textSelection(.enabled).lineLimit(3)
            Text("Search text, expressions or whole identifiers in the selected location and file types, including large files. UTF-8 and BOM-declared UTF-16 are recognized automatically. No per-file byte cap applies; the total byte budget limits the scan.")
                .font(.caption).foregroundStyle(.secondary)
            Text(presentation.source.pathScope.isVisible
                ? "This sheet keeps the displayed files and file-type selection fixed. Discovery uses the same file or folder focus and ignore policy as the atlas; missing requested paths are reported as incomplete coverage."
                : "This sheet keeps the sidebar's location and file-type selection fixed. Discovery continues beyond the atlas file limit under the selected exclusion policy. The total file count remains unknown until discovery finishes.")
                .font(.caption).foregroundStyle(.secondary)
            if model.supportsDiscoveryPolicy {
                HStack(alignment: .top, spacing: 10) {
                    Toggle("Include ignored files", isOn: Binding(
                        get: { model.discoveryPolicy.includesExcluded },
                        set: { model.discoveryPolicy = $0 ? .includeExcluded : .repositoryRules }))
                        .toggleStyle(.checkbox)
                        .help("Apply this discovery policy to the next search. Existing results become read-only; the atlas and other searches are unchanged.")
                    Text(model.discoveryPolicy.includesExcluded
                        ? "Includes ignored and generated files within the selected location and file types. These files consume the same total scan budget."
                        : "Applies repository ignore rules and default exclusions. Excluded source does not consume the scan budget.")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
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
                Toggle("Match case", isOn: Binding(get: { model.caseSensitive },
                    set: { model.caseSensitive = $0 })).toggleStyle(.checkbox)
                    .help("When off, use Unicode lowercase matching while retaining the original spelling and byte ranges")
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
                Text("Occurrences match one complete identifier with the selected case policy, including comments and literals. ASCII letters, digits, _ and $ are token characters; an identifier cannot start with an ASCII digit. Non-ASCII runs use conservative boundaries. These are text candidates, without compiler resolution.")
                    .font(.caption).foregroundStyle(.secondary)
            } else if model.mode == .expression {
                Text("Use literal terms, quoted phrases, -excluded terms, path: and lang: filters. The first positive literal supplies matches and highlights. Every required or excluded term is checked through the end of the same file observation before that file supplies hits. Regular expressions are unavailable.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            HStack {
                if let catalog = model.catalog {
                    Text(catalog.progressive
                        ? "Progressive project discovery · each search observes current files again"
                        : "\(catalog.files) catalogued files · \(catalog.discoveryComplete ? "discovery complete within this scope" : "discovery incomplete")")
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
                    if progress.summary.discoveryMore || progress.summary.pendingCataloguedFiles > 0 {
                        ProgressView("Discovering and searching project files…")
                        Text("\(progress.summary.cataloguedFiles) files discovered so far; \(progress.summary.pendingCataloguedFiles) await scope admission.")
                            .font(.caption).foregroundStyle(.secondary)
                    } else {
                        ProgressView(value: Double(progress.summary.examinedFiles),
                            total: Double(max(1, progress.summary.searchedFiles)))
                    }
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
            if let keptExcerpt {
                HStack {
                    Text(desk.notice).font(.caption).textSelection(.enabled)
                    Spacer()
                    Button("Read kept excerpt or add note…") {
                        if let target = desk.keptStreamExcerpt(keptExcerpt, excluding: priorPassages) {
                            observedDraft = AtlasDeskObservedNoteDraft(target: target)
                        }
                    }.disabled(desk.keptStreamExcerpt(keptExcerpt, excluding: priorPassages) == nil)
                }
            }
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
            AtlasObservedReaderView(presentation: context, desk: desk)
        }
        .sheet(item: $observedDraft) { draft in
            AtlasDeskObservedPassageView(model: desk, draft: draft)
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
                Text(report.summary.identity.progressive
                    ? "Each window discovers and scans current files again. Files and discovery order may have changed; the new results replace the previous observation."
                    : "Each window scans current files again. Files may have changed; the new results replace the previous observation.")
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
            if let context = hit.context, let parts = context.parts {
                (Text(verbatim: parts.before) + Text(verbatim: parts.match).bold().underline()
                    + Text(verbatim: parts.after))
                    .font(.system(.caption, design: .monospaced)).lineLimit(3)
            }
            if !hit.complete {
                Text("Partial file observation · \(hit.state) · \(hit.consistency)")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder private var selectedMatch: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let witness = model.witness, let report = model.report, report.admits(witness.target) {
                Text(witness.context == nil ? "Retained literal" : "Retained search excerpt")
                    .font(.subheadline.weight(.semibold))
                Text(verbatim: witness.target.hit.displayPath).font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled).lineLimit(3)
                Text("Original bytes \(witness.target.hit.range.start)..<\(witness.target.hit.range.end) · \(witness.target.hit.encoding.label)")
                    .font(.caption).foregroundStyle(.secondary)
                if let context = witness.context {
                    Text("The match and this bounded excerpt were retained from the same source read. Selecting or copying them does not reopen the file.")
                        .font(.caption).foregroundStyle(.secondary)
                    Text("Excerpt bytes \(context.originalRange.start)..<\(context.originalRange.end) · "
                        + (context.complete ? "requested excerpt complete" : "partial excerpt; the scan stopped before all following bytes were available"))
                        .font(.caption).foregroundStyle(.secondary)
                    if let textRange = context.textRange, context.displayText != nil,
                       textRange != context.originalRange {
                        Text("Displayed text covers bytes \(textRange.start)..<\(textRange.end). Edge bytes remain available in the original hex.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                } else {
                    Text("Only this exact matching text and its original bytes were retained. Surrounding source and a whole-file snapshot were not captured.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                if let expression = report.summary.expression {
                    Text(verbatim: "Expression: " + expression.query).font(.caption).textSelection(.enabled)
                    Text("This is a primary-literal match from a file observation that satisfied the expression. Other predicates were checked across that same file observation.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                ScrollView([.horizontal, .vertical]) {
                    VStack(alignment: .leading, spacing: 8) {
                        if let context = witness.context, let parts = context.parts {
                            (Text(verbatim: parts.before) + Text(verbatim: parts.match).bold().underline()
                                + Text(verbatim: parts.after))
                                .font(.system(.body, design: .monospaced)).textSelection(.enabled)
                        } else {
                            Text(verbatim: witness.text).font(.system(.body, design: .monospaced))
                                .textSelection(.enabled)
                            if let context = witness.context {
                                Text("A lossless text excerpt is unavailable at these captured boundaries. The retained original bytes are shown below.")
                                    .font(.caption).foregroundStyle(.secondary)
                                Text(verbatim: context.originalHex).font(.system(.caption, design: .monospaced))
                                    .textSelection(.enabled)
                            }
                        }
                    }.frame(maxWidth: .infinity, alignment: .topLeading)
                }.frame(minHeight: 100, maxHeight: .infinity)
                HStack {
                    Button("Copy match") { copy(witness, hex: false) }
                    Button("Copy original hex") { copy(witness, hex: true) }
                }
                if let context = witness.context {
                    HStack {
                        Button("Copy excerpt") { copyExcerpt(witness, hex: false) }
                            .disabled(context.displayText == nil)
                        Button("Copy excerpt hex") { copyExcerpt(witness, hex: true) }
                    }
                }
                Button("Keep captured excerpt") {
                    let previous = Set(desk.state?.observedPassages.map(\.id) ?? [])
                    if let target = desk.keepStreamExcerpt(reader: model) {
                        priorPassages = previous; keptExcerpt = target
                    }
                }.disabled(!desk.canKeepStreamExcerpt(reader: model))
                    .help("Keep the lossless captured excerpt with its original byte locations, then read it or add a note. Unseen source remains unknown; no file is reopened.")
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
                    description: Text("Select a result to inspect its captured match and any retained surrounding excerpt. Current context is a separate explicit read."))
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

    private func copyExcerpt(_ witness: AtlasWorkspaceStreamWitness, hex: Bool) {
        guard !model.closed, model.witness?.target == witness.target,
              model.report?.admits(witness.target) == true, let context = witness.context,
              let contents = hex ? context.originalHex : context.displayText else { return }
        NSPasteboard.general.clearContents()
        if NSPasteboard.general.setString(contents, forType: .string) {
            copyNotice = hex ? "Copied the retained excerpt's original bytes as hex; no file was reread."
                : "Copied the displayed retained excerpt with its original line endings; no file was reread."
        } else { copyNotice = "The pasteboard did not accept the retained excerpt." }
    }
}
