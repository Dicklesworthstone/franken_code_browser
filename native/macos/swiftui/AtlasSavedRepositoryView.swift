import SwiftUI
import AppKit

/// A successful native open/save panel supplies selected-file access. Adopt it
/// once, and release it only after the writer or archive descriptor drains.
final class AtlasSavedFileAccess {
    let url: URL
    private let accessing: Bool
    init(selectedURL: URL) {
        url = selectedURL
        accessing = selectedURL.startAccessingSecurityScopedResource()
    }
    var nativePath: String? {
        guard url.isFileURL else { return nil }
        return url.withUnsafeFileSystemRepresentation { pointer in
            guard let pointer else { return nil }
            var bytes: [UInt8] = []
            for index in 0...16_384 {
                let byte = UInt8(bitPattern: pointer[index])
                if byte == 0 {
                    guard bytes.first == 47 else { return nil }
                    return String(bytes: bytes, encoding: .utf8)
                }
                guard index < 16_384 else { return nil }
                bytes.append(byte)
            }
            return nil
        }
    }
    deinit { if accessing { url.stopAccessingSecurityScopedResource() } }
}

/// A selected FCBS archive is a complete independent browsing entry point. The
/// list owns only bounded metadata pages; source reads happen through verified
/// empty-reader handoffs on the worker, never through displayed member names.
@MainActor struct AtlasSavedRepositoryView: View {
    @Bindable var model: AtlasSavedRepositoryModel
    @Bindable var desk: AtlasReadingDeskModel
    @Bindable var exporter: AtlasSnapshotExportModel
    var currentSource: @MainActor () -> AtlasSnapshotExportSource? = { nil }
    @Environment(\.dismiss) private var dismiss
    @State private var panelNotice = ""
    @State private var section: Section = .text
    private enum Section: String, CaseIterable, Identifiable {
        case text = "Search", links = "Documentation links", imports = "Imports"
        var id: String { rawValue }
    }
    @State private var readerCapture: AtlasSavedReaderCapture?
    @State private var readerTask: Task<Void, Never>?
    @State private var showsComparison = false
    @State private var pins = AtlasPinnedReaders<AtlasPagedReaderModel>(
        canFork: { $0.canPin }, fork: { try await $0.fork() }, retire: { $0.close() },
        failureMessage: { ($0 as? AtlasReaderError)?.message ?? "The saved capture could not be pinned." })

    var body: some View {
        VStack(spacing: 10) {
            header
            exportPane
            if let info = model.info {
                HStack {
                    Text(info.path.display).lineLimit(1).truncationMode(.middle).textSelection(.enabled)
                    Spacer()
                    Text("\(info.identity.captured) / \(info.identity.members) members captured")
                    Text(info.identity.discoveryComplete ? "Discovery complete" : "Discovery incomplete")
                        .foregroundStyle(info.identity.discoveryComplete ? Color.secondary : Color.orange)
                }.font(.caption)
                HSplitView {
                    membersPane.frame(minWidth: 270, idealWidth: 320, maxWidth: 430)
                    VStack(alignment: .leading, spacing: 12) {
                        Picker("Saved repository activity", selection: $section) {
                            ForEach(Section.allCases) { Text($0.rawValue).tag($0) }
                        }.pickerStyle(.segmented).padding(.leading, 8)
                        if section == .text { searchPane }
                        else if section == .imports { importsPane }
                        else { relationshipsPane }
                    }.frame(minWidth: 620, maxWidth: .infinity, maxHeight: .infinity)
                        .onChange(of: section) { _, next in
                            if next == .imports, model.imports == nil { model.loadImports(direction: model.importDirection) }
                            if next == .links, model.relationships == nil { model.loadRelationships(direction: model.direction) }
                        }
                }
            } else {
                VStack(spacing: 14) {
                    Image(systemName: "archivebox").font(.system(size: 42)).foregroundStyle(.secondary)
                    Text("Save and read a repository").font(.title2.weight(.semibold))
                    Text("Choose an FCBS archive to search captured text, symbols, headings and identifier occurrences, read source and Markdown, and follow documentation links or source imports.")
                        .multilineTextAlignment(.center).frame(maxWidth: 540)
                    Button("Open saved repository…", action: chooseArchive).disabled(!model.canOperate)
                    Text("The archive is read only. Unavailable members remain visible, with their omission reason.")
                        .font(.caption).foregroundStyle(.secondary)
                }.frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            footer
        }
        .padding(16).frame(minWidth: 1020, idealWidth: 1320, minHeight: 660, idealHeight: 800)
        .sheet(item: $readerCapture) { capture in
            AtlasSavedReaderView(capture: capture, archive: model, desk: desk, pins: pins)
        }
        .sheet(isPresented: $showsComparison) { AtlasSnapshotComparisonView(desk: desk) }
        .onDisappear {
            readerTask?.cancel(); readerCapture?.reader.close(); pins.closeAll()
            model.cancel(); exporter.cancel()
        }
    }

    private var header: some View {
        HStack {
            Label("Saved repository", systemImage: "archivebox").font(.title2.weight(.semibold))
            Spacer()
            Button("Open archive…", action: chooseArchive).disabled(!model.canOperate || readerTask != nil)
            Button("Close archive", action: model.closeArchive).disabled(!model.canOperate || model.info == nil)
            Button("Compare captures…") { showsComparison = true }.disabled(!model.canOperate || readerTask != nil)
            Button("Done") {
                readerTask?.cancel(); model.cancel(); exporter.cancel(); dismiss()
            }.keyboardShortcut(.cancelAction)
        }
    }
    private var exportPane: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Button { saveCurrentProject() } label: {
                    Label("Save current project…", systemImage: "square.and.arrow.down")
                }
                .disabled(exporter.busy || currentSource() == nil)
                .help("Capture current files under the project's read grant into a new source-containing FCBS archive")
                if exporter.busy {
                    ProgressView().controlSize(.small)
                    Text(exporter.cancellationRequested ? "Waiting for save outcome…" : "Capturing and saving…")
                        .font(.caption)
                    Button("Cancel save", action: exporter.cancel).disabled(exporter.cancellationRequested)
                }
                Spacer()
                if exporter.canOpenSavedArchive {
                    Button("Open saved archive") {
                        panelNotice = ""
                        exporter.openSavedArchive { path, digest, access in
                            model.open(path: path, accessLease: access, expectedDigest: digest)
                        }
                    }.disabled(!model.canOperate || readerTask != nil)
                }
            }
            if let request = exporter.request {
                Text("Source: \(String(reflecting: request.root))")
                    .font(.caption).lineLimit(2).textSelection(.enabled)
                Text("Destination: \(String(reflecting: request.destination))")
                    .font(.caption).lineLimit(2).textSelection(.enabled)
            }
            Text(exporter.notice).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            Text("Captures current on-disk observations using repository ignore rules: up to 20,000 known files, 1 MiB per file and 64 MiB of source. Source and filenames are stored as plaintext. The archive may be partial; existing files are never overwritten.")
                .font(.caption2).foregroundStyle(.secondary)
            if currentSource() == nil {
                Text("Open a project to enable Save current project. Existing archives can be opened independently.")
                    .font(.caption2).foregroundStyle(.secondary)
            }
        }.padding(10).background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
    }
    private var footer: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack {
                if model.busy {
                    ProgressView().controlSize(.small)
                    Button("Cancel") { readerTask?.cancel(); model.cancel() }
                }
                Text(panelNotice.isEmpty ? model.notice : panelNotice)
                    .font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                Spacer()
            }
            Text(section == .text
                ? (model.searchMode == .literal
                    ? "Match case compares literal text exactly. Turn it off to compare Unicode lowercase text; neither mode normalizes Unicode or interprets query syntax. Member names never open working-tree paths."
                    : (model.searchMode == .expression
                        ? "Expression terms are exact and case sensitive. Required/excluded terms apply across each complete captured member; results identify primary-term occurrences. Member names never open working-tree paths."
                        : "Structural results retain the shared extractor's evidence level and original source coordinates. All searches use this saved archive; member names never open working-tree paths."))
                : "Documentation evidence is the original captured document. Qualified Markdown headings and source-line fragments open their captured byte range; unresolved destinations show why. Member names never open working-tree paths.")
                .font(.caption2).foregroundStyle(.secondary)
        }.frame(maxWidth: .infinity, alignment: .leading)
    }
    private var membersPane: some View {
        VStack(alignment: .leading, spacing: 9) {
            Text("Saved members").font(.headline)
            if let page = model.members {
                Text("Members \(page.start)..<\(page.start + UInt64(page.items.count)) · metadata only")
                    .font(.caption).foregroundStyle(.secondary)
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 4) {
                        ForEach(page.items) { member in memberRow(member, in: page) }
                    }
                }.frame(maxHeight: .infinity)
                HStack {
                    Button("Previous") { model.loadMembers(start: page.start >= 64 ? page.start - 64 : 0) }
                        .disabled(!model.canOperate || page.start == 0)
                    Spacer()
                    Button("Next") { if let next = page.next { model.loadMembers(start: next) } }
                        .disabled(!model.canOperate || page.next == nil)
                }.controlSize(.small)
                HStack {
                    TextField("Member ordinal", text: $model.memberOffsetInput)
                        .onSubmit { model.goToMemberPage() }
                    Button("Go", action: model.goToMemberPage)
                }.controlSize(.small).disabled(!model.canOperate)
            }
            Divider()
            if let member = model.selectedMember, let identity = model.info?.identity {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Selected member").font(.caption.weight(.semibold))
                    Text(member.path.display).font(.callout).lineLimit(3).textSelection(.enabled)
                    Text(member.captured
                        ? (member.observedBytes == 0 ? "Captured empty file" : "\(member.observedBytes) original bytes captured")
                        : "Source unavailable: \(member.reason ?? "UNKNOWN")")
                        .font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                    Button {
                        openMember(member, from: identity)
                    } label: { Label("Read captured member", systemImage: "doc.text") }
                    .disabled(!model.canOperate || !member.captured || readerTask != nil)
                    if !member.captured {
                        Text("Recorded size: \(member.observedBytes) bytes. This archive contains no readable source for the member.")
                            .font(.caption2).foregroundStyle(.secondary)
                    }
                }
            }
        }.padding(.trailing, 8)
    }
    private func memberRow(_ member: AtlasSavedMember, in page: AtlasSavedMembers) -> some View {
        Button {
            panelNotice = ""; model.selectMember(member, from: page.identity, forImports: section == .imports)
        } label: {
            HStack(alignment: .top, spacing: 7) {
                Image(systemName: member.captured ? "doc.text" : "doc.badge.ellipsis")
                    .foregroundStyle(member.captured ? Color.primary : Color.secondary)
                VStack(alignment: .leading, spacing: 2) {
                    Text(member.path.display).lineLimit(2).truncationMode(.middle)
                    Text(member.captured ? "\(member.observedBytes) captured bytes" : "Unavailable · \(member.reason ?? "UNKNOWN")")
                        .font(.caption2).foregroundStyle(.secondary)
                }
                Spacer(minLength: 2)
                if model.selectedMember?.id == member.id {
                    Image(systemName: "checkmark.circle.fill").foregroundStyle(Color.accentColor)
                }
            }.frame(maxWidth: .infinity, alignment: .leading).padding(6)
                .background(model.selectedMember?.id == member.id ? Color.accentColor.opacity(0.09) : Color.clear)
                .clipShape(RoundedRectangle(cornerRadius: 6))
        }.buttonStyle(.plain).disabled(!model.canOperate)
        .accessibilityLabel("\(member.path.display), \(member.captured ? "captured" : "unavailable")")
    }
    private var searchPane: some View {
        VStack(alignment: .leading, spacing: 10) {
            Picker("Saved search mode", selection: $model.searchMode) {
                ForEach(AtlasSavedSearchMode.allCases) { mode in Text(mode.label).tag(mode) }
            }.pickerStyle(.segmented).disabled(!model.canOperate).padding(.leading, 8)
            if model.searchMode == .literal { literalSearchPane }
            else if model.searchMode == .expression { expressionSearchPane }
            else { structureSearchPane }
        }
    }
    private var literalSearchPane: some View {
        VStack(alignment: .leading, spacing: 10) {
            indexPane
            HStack {
                TextField("Literal text in all captured members", text: $model.searchInput)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { panelNotice = ""; model.search() }
                    .accessibilityLabel("Literal search across the saved archive")
                Button { panelNotice = ""; model.search() } label: { Label("Search archive", systemImage: "magnifyingglass") }
                    .disabled(!model.canSearch || !model.validSearchInput)
                Button("Clear") { panelNotice = ""; model.clearSearch() }
                    .disabled(!model.canOperate || (model.searchPage == nil && model.searchInput.isEmpty))
            }
            HStack {
                Toggle("Match case", isOn: $model.searchCaseSensitive)
                    .toggleStyle(.checkbox).disabled(!model.canOperate)
                    .help("Turn off to compare Unicode lowercase text. This does not use full case folding or Unicode normalization.")
                Toggle("Count all matches", isOn: $model.countAllMatches)
                    .toggleStyle(.checkbox).disabled(!model.canOperate)
                Text("Entire saved archive · up to 4,096 retained matches")
                    .font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("Reload results") { panelNotice = ""; model.reloadSearchResults() }
                    .controlSize(.small).disabled(!model.canOperate)
            }
            if !model.searchInput.isEmpty && !model.validSearchInput {
                Text("The literal must contain 1–1024 UTF-8 bytes and no NUL character.")
                    .font(.caption).foregroundStyle(.orange)
            }
            if let page = model.searchPage {
                VStack(alignment: .leading, spacing: 5) {
                    Text("Accepted query").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                    Text(page.needle).font(.body.monospaced()).lineLimit(3).textSelection(.enabled)
                    Text(page.matchingDescription).font(.caption).foregroundStyle(.secondary)
                    if !model.searchIsCurrent {
                        HStack {
                            Text("The draft differs from these results. Submit it or restore the accepted query before opening a match.")
                                .font(.caption).foregroundStyle(.orange)
                            Spacer()
                            Button("Use accepted query") {
                                model.searchInput = page.needle; model.countAllMatches = page.counting.all
                                model.searchCaseSensitive = page.caseSensitive; panelNotice = ""
                            }
                                .controlSize(.small).disabled(!model.canOperate)
                        }
                    }
                }
                searchCoverage(page)
                Divider()
                if page.hits.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("No retained text matches.").font(.headline)
                        Text(page.seen > 0 ? "Matches were counted without retaining rows. Search again with a positive retained-match limit to open them."
                            : page.complete ? "The accepted literal has no matches in the complete saved manifest."
                            : "Coverage is partial. An empty result cannot establish that the literal is absent from the repository.")
                            .foregroundStyle(.secondary)
                    }.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading).padding(.top, 10)
                } else {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 8) {
                            ForEach(page.hits) { hit in searchHitRow(hit, in: page) }
                        }
                    }.id("saved-query:\(page.identity.owner):\(page.generation)").frame(maxHeight: .infinity)
                }
                HStack {
                    Button("Previous matches") {
                        model.loadSearchResults(start: page.start >= 64 ? page.start - 64 : 0, from: page)
                    }.disabled(!model.canOperate || !model.searchIsCurrent || page.start == 0)
                    Spacer()
                    Text("\(page.start)..<\(page.start + UInt64(page.hits.count)) of \(page.retained) retained")
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button("Next matches") {
                        if let next = page.next { model.loadSearchResults(start: next, from: page) }
                    }.disabled(!model.canOperate || !model.searchIsCurrent || page.next == nil)
                }.controlSize(.small)
            } else {
                VStack(alignment: .leading, spacing: 10) {
                    Text(model.busy ? "Waiting for saved archive work…" : "Find text across your saved repository.")
                        .font(.headline)
                    Text("Enter an identifier, message or phrase and choose Search archive. Results name exact captured byte ranges, including supported UTF-16 source.")
                    Text("Search reads saved source on a worker. Unavailable files, unsupported decoding and a retained-match limit are reported explicitly.")
                        .font(.caption).foregroundStyle(.secondary)
                }.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading).padding(.top, 12)
            }
        }.padding(.leading, 8)
    }
    private var indexPane: some View {
        DisclosureGroup("Saved search index") {
            VStack(alignment: .leading, spacing: 6) {
                if !model.indexStateKnown {
                    Text("Index acceptance is unconfirmed. Reload status before changing the index or starting a new literal search.")
                        .font(.caption).foregroundStyle(.orange)
                }
                if let index = model.index {
                    Text("\(index.indexed) indexed members · \(index.uncovered) uncovered · \(index.unavailable) unavailable")
                        .font(.caption.weight(.semibold))
                    if let path = model.indexPath {
                        Text(path).font(.caption).lineLimit(2).truncationMode(.middle).textSelection(.enabled)
                    }
                    Text("Last checked: \(index.pageBytes) posting bytes read · \(index.pageLoads) page loads · \(index.cacheHits) cache hits · 64 KiB page cache")
                        .font(.caption2).foregroundStyle(.secondary)
                    Text("Trusted page-manifest SHA-256: \(index.pin)")
                        .font(.caption2.monospaced()).textSelection(.enabled)
                } else if model.indexStateKnown {
                    Text("No index attached. Literal searches scan captured source directly.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                TextField("Trusted page-manifest SHA-256 from the index build receipt", text: $model.indexPinInput)
                    .textFieldStyle(.roundedBorder).font(.caption.monospaced())
                    .disabled(!model.canChangeIndex)
                    .accessibilityLabel("Trusted index page-manifest SHA-256 from its build receipt")
                HStack {
                    Button("Choose FCBD index…", action: chooseIndex)
                        .disabled(!model.canChangeIndex || !AtlasSavedWire.digest(model.indexPinInput) || readerTask != nil)
                    Button("Detach index") { panelNotice = ""; model.detachIndex() }
                        .disabled(!model.canChangeIndex || model.index == nil)
                    Spacer()
                    Button("Reload index status") { panelNotice = ""; model.reloadIndex() }
                        .disabled(!model.canOperate)
                }.controlSize(.small)
                Text("Use the digest from a trusted index build receipt, not a whole-file checksum. Posting pages are checked when read; exact source verification and direct scans cover candidates, uncovered members and short literals. Unicode lowercase and expression searches scan source directly.")
                    .font(.caption2).foregroundStyle(.secondary)
            }.padding(.top, 5)
        }.font(.callout)
    }
    private var expressionSearchPane: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                TextField("\"exact phrase\" required -excluded path:src/ lang:rust", text: $model.searchInput)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { panelNotice = ""; model.search() }
                    .accessibilityLabel("Expression search with required terms, exclusions, path and language filters")
                Button { panelNotice = ""; model.search() } label: { Label("Search archive", systemImage: "magnifyingglass") }
                    .disabled(!model.canOperate || !model.validSearchInput)
                Button("Clear") { panelNotice = ""; model.clearSearch() }
                    .disabled(!model.canOperate || (model.expressionPage == nil && model.searchInput.isEmpty))
            }
            Text("The first positive term names matches. Other terms must appear in the same captured member; -terms must be absent. Quote phrases and paths containing spaces. Use path:, -path:, lang: or -lang: to filter members.")
                .font(.caption).foregroundStyle(.secondary)
            HStack {
                Toggle("Count all matches", isOn: $model.countAllMatches)
                    .toggleStyle(.checkbox).disabled(!model.canOperate)
                Text("Case sensitive · 64 terms and filters maximum · up to 4,096 retained rows · 64 MiB matching-work budget")
                    .font(.caption2).foregroundStyle(.secondary)
                Spacer()
                Button("Reload results") { panelNotice = ""; model.reloadSearchResults() }
                    .controlSize(.small).disabled(!model.canOperate)
            }
            if !model.searchInput.isEmpty && !model.validSearchInput {
                Text("The expression must contain 1–1024 UTF-8 bytes and no NUL character.")
                    .font(.caption).foregroundStyle(.orange)
            }
            if let page = model.expressionPage {
                VStack(alignment: .leading, spacing: 5) {
                    Text("Accepted expression").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                    Text(page.expression).font(.body.monospaced()).lineLimit(3).textSelection(.enabled)
                    Text("Primary occurrence: \(page.primaryNeedle) · \(page.required) required · \(page.excluded) excluded terms")
                        .font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                    if !model.expressionIsCurrent {
                        HStack {
                            Text("The draft differs from these results. Submit it or restore the accepted expression before opening a match.")
                                .font(.caption).foregroundStyle(.orange)
                            Spacer()
                            Button("Use accepted expression") {
                                model.searchInput = page.expression; model.countAllMatches = page.counting.all; panelNotice = ""
                            }
                                .controlSize(.small).disabled(!model.canOperate)
                        }
                    }
                }
                expressionCoverage(page)
                Divider()
                if page.hits.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("No retained expression matches.").font(.headline)
                        Text(page.seen > 0 ? "Matches were counted without retaining rows. Search again with a positive retained-match limit to open them."
                            : page.complete ? "The accepted expression has no matches in the complete captured scope."
                            : "Coverage is partial. An empty result does not establish that the expression has no matches.")
                            .foregroundStyle(.secondary)
                    }.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading).padding(.top, 10)
                } else {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 8) {
                            ForEach(page.hits) { hit in
                                HStack(alignment: .top, spacing: 10) {
                                    VStack(alignment: .leading, spacing: 4) {
                                        Text(hit.path.display).font(.callout.weight(.semibold)).lineLimit(2).textSelection(.enabled)
                                        Text("Occurrence \(hit.id) · original bytes \(hit.start)..<\(hit.end)")
                                            .font(.caption).foregroundStyle(.secondary)
                                    }
                                    Spacer()
                                    Button("Read match") { openExpressionHit(hit, from: page) }
                                        .disabled(!model.canOperate || !model.expressionIsCurrent || readerTask != nil)
                                }.padding(10).frame(maxWidth: .infinity, alignment: .leading)
                                    .background(Color.primary.opacity(0.035)).clipShape(RoundedRectangle(cornerRadius: 7))
                            }
                        }
                    }.id("saved-expression:\(page.identity.owner):\(page.generation)").frame(maxHeight: .infinity)
                }
                HStack {
                    Button("Previous matches") {
                        model.loadExpressionResults(start: page.start >= 64 ? page.start - 64 : 0, from: page)
                    }.disabled(!model.canOperate || !model.expressionIsCurrent || page.start == 0)
                    Spacer()
                    Text("\(page.start)..<\(page.start + UInt64(page.hits.count)) of \(page.retained) retained")
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button("Next matches") {
                        if let next = page.next { model.loadExpressionResults(start: next, from: page) }
                    }.disabled(!model.canOperate || !model.expressionIsCurrent || page.next == nil)
                }.controlSize(.small)
            } else {
                VStack(alignment: .leading, spacing: 10) {
                    Text(model.busy ? "Waiting for saved archive work…" : "Combine source terms with member filters.").font(.headline)
                    Text("Example: \"pub fn\" Result -unsafe path:src/ lang:rust").font(.callout.monospaced()).textSelection(.enabled)
                    Text("Finds each ‘pub fn’ occurrence in Rust members under src/ that also contain ‘Result’ and do not contain ‘unsafe’. Literal mode treats the entire input as one exact phrase.")
                        .font(.caption).foregroundStyle(.secondary)
                }.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading).padding(.top, 12)
            }
        }.padding(.leading, 8)
    }
    private var structureSearchPane: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                TextField(model.searchMode == .headings ? "Heading title in captured Markdown" : "Name in captured source",
                    text: $model.searchInput)
                    .textFieldStyle(.roundedBorder).onSubmit { panelNotice = ""; model.search() }
                    .accessibilityLabel("\(model.searchMode.label) search across the saved archive")
                Button { panelNotice = ""; model.search() } label: { Label("Search archive", systemImage: "magnifyingglass") }
                    .disabled(!model.canOperate || !model.validSearchInput)
                Button("Clear") { panelNotice = ""; model.clearSearch() }
                    .disabled(!model.canOperate || (model.structurePage == nil && model.searchInput.isEmpty))
            }
            HStack {
                if model.searchMode.supportsNameMatching {
                    Picker("Name matching", selection: $model.searchNameMode) {
                        ForEach(AtlasSearchNameMode.allCases) { Text($0.label).tag($0) }
                    }.frame(maxWidth: 270).disabled(!model.canOperate)
                } else { Text("One exact identifier token").font(.caption) }
                Spacer()
                Text("Case sensitive · entire saved archive").font(.caption).foregroundStyle(.secondary)
                Button("Reload results") { panelNotice = ""; model.reloadSearchResults() }
                    .controlSize(.small).disabled(!model.canOperate)
            }
            Text(structureExplanation).font(.caption).foregroundStyle(.secondary)
            if !model.searchInput.isEmpty && !model.validSearchInput {
                Text("Enter 1–1024 UTF-8 bytes without NUL. Occurrences require one identifier token.")
                    .font(.caption).foregroundStyle(.orange)
            }
            if let page = model.structurePage {
                VStack(alignment: .leading, spacing: 5) {
                    Text("Accepted \(page.mode.label.lowercased()) query · \(page.nameMode.label.lowercased())")
                        .font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                    Text(page.needle).font(.body.monospaced()).lineLimit(3).textSelection(.enabled)
                    if !model.structureIsCurrent {
                        HStack {
                            Text("The draft, mode or name matching differs from these results. Submit it or restore the accepted query to open a result.")
                                .font(.caption).foregroundStyle(.orange)
                            Spacer()
                            Button("Use accepted query") {
                                model.searchMode = page.mode; model.searchNameMode = page.nameMode
                                model.searchInput = page.needle; panelNotice = ""
                            }.controlSize(.small).disabled(!model.canOperate)
                        }
                    }
                }
                structureCoverage(page)
                Divider()
                if page.hits.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("No retained \(page.mode.structure?.resultLabel ?? "results").").font(.headline)
                        Text(page.complete ? "The accepted extractor found no matching results at its stated evidence level."
                            : "Coverage is partial. Empty results do not establish that the name is absent from the repository.")
                            .foregroundStyle(.secondary)
                    }.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading).padding(.top, 10)
                } else {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 8) {
                            ForEach(page.hits) { item in
                                HStack(alignment: .top, spacing: 10) {
                                    VStack(alignment: .leading, spacing: 4) {
                                        Text(item.structure.name).font(.callout.monospaced().weight(.semibold))
                                            .lineLimit(2).textSelection(.enabled)
                                        Text(item.hit.path.display).font(.caption).lineLimit(2).textSelection(.enabled)
                                        Text(item.structure.label).font(.caption).foregroundStyle(.secondary)
                                        Text("Original bytes \(item.hit.start)..<\(item.hit.end)")
                                            .font(.caption2).foregroundStyle(.secondary)
                                    }
                                    Spacer()
                                    Button("Read source") { openStructureHit(item, from: page) }
                                        .disabled(!model.canOperate || !model.structureIsCurrent || readerTask != nil)
                                }.padding(10).frame(maxWidth: .infinity, alignment: .leading)
                                    .background(Color.primary.opacity(0.035)).clipShape(RoundedRectangle(cornerRadius: 7))
                            }
                        }
                    }.id("saved-structure:\(page.identity.owner):\(page.generation)").frame(maxHeight: .infinity)
                }
                HStack {
                    Button("Previous results") { model.loadStructureResults(start: page.start >= 64 ? page.start - 64 : 0, from: page) }
                        .disabled(!model.canOperate || !model.structureIsCurrent || page.start == 0)
                    Spacer()
                    Text("\(page.start)..<\(page.start + UInt64(page.hits.count)) of \(page.retained) retained")
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button("Next results") { if let next = page.next { model.loadStructureResults(start: next, from: page) } }
                        .disabled(!model.canOperate || !model.structureIsCurrent || page.next == nil)
                }.controlSize(.small)
            } else {
                VStack(alignment: .leading, spacing: 10) {
                    Text(model.busy ? "Waiting for saved archive work…" : "Navigate the structure of your saved repository.").font(.headline)
                    Text("Search symbols, Markdown heading titles or exact identifier tokens. Open a result to read its verified captured source; that reader remains available after the archive closes.")
                    Text("Up to 4,096 results and 64 MiB of aggregate source reads. Per-member limits: symbols 64 KiB, headings 256 KiB, occurrences 512 KiB. Coverage counts expose omitted and unsupported members.")
                        .font(.caption).foregroundStyle(.secondary)
                }.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading).padding(.top, 12)
            }
        }.padding(.leading, 8)
    }
    private var structureExplanation: String {
        switch model.searchMode {
        case .symbols:
            return "Heuristic declarations from Rust, Python, JavaScript, TypeScript, Go and C/C++. Qualified names use the shared outline engine; results do not claim compiler resolution."
        case .headings:
            return "Markdown titles and duplicate-aware slugs come from FrankenMarkdown. Only qualified original source ranges can open a result."
        case .occurrences:
            return "Whole-token text occurrences include comments and string literals. These are spelling matches; they do not establish a semantic reference or call graph."
        case .literal, .expression: return ""
        }
    }
    private func structureCoverage(_ page: AtlasSavedStructurePage) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Label(page.complete ? "Complete extraction processing" : "Partial extraction coverage",
                systemImage: page.complete ? "checkmark.circle" : "exclamationmark.circle")
                .font(.caption.weight(.semibold)).foregroundStyle(page.complete ? Color.secondary : Color.orange)
            Text("\(page.retained) results retained · \(page.visited) / \(page.identity.members) members visited · \(page.searched) extracted")
                .font(.caption)
            Text("Unavailable: \(page.unavailable) · Unsupported language or decoding: \(page.unsupported) · Source size limit: \(page.sourceLimited)")
                .font(.caption).foregroundStyle(.secondary)
            Text("Extractor limits: \(page.extractionLimited) · Metadata omitted: \(page.metadataOmitted) · \(page.sourceBytes) / \(page.sourceLimit) source bytes read")
                .font(.caption).foregroundStyle(.secondary)
            if page.state == "work-limit" {
                Text("The aggregate source budget stopped extraction. Earlier results remain valid; later members were not examined.")
                    .font(.caption).foregroundStyle(.orange)
            } else if page.truncated {
                Text("At least \(page.seen) matches were seen. The retained-result limit stopped extraction.")
                    .font(.caption).foregroundStyle(.orange)
            }
            if !page.identity.discoveryComplete {
                Text("Archive discovery was incomplete. Files outside its manifest have no search coverage.")
                    .font(.caption).foregroundStyle(.orange)
            }
        }
    }
    private func expressionCoverage(_ page: AtlasSavedExpressionPage) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Label(page.complete ? "Complete expression coverage" : "Partial expression coverage",
                systemImage: page.complete ? "checkmark.circle" : "exclamationmark.circle")
                .font(.caption.weight(.semibold)).foregroundStyle(page.complete ? Color.secondary : Color.orange)
            Text(page.counting.summary(seen: page.seen, retained: page.retained))
                .font(.caption)
            Text("\(page.scopeFiles) members in scope · \(page.filteredFiles) filtered by path/language")
                .font(.caption)
            Text("Unavailable: \(page.unavailable) · Unsupported decoding: \(page.unsupported) · Predicate rejections: \(page.predicateRejections)")
                .font(.caption).foregroundStyle(.secondary)
            Text("\(page.sourceBytes) source bytes read · \(page.scannedBytes) / \(page.scanLimit) matching bytes processed")
                .font(.caption).foregroundStyle(.secondary)
            if page.state == "work-limit" {
                Text("The matching-work budget was exhausted. Earlier matches remain valid; more may exist. Narrow path or language filters and search again.")
                    .font(.caption).foregroundStyle(.orange)
            } else if page.truncated && !page.counting.all {
                Text("At least \(page.seen) matches were seen. The retained-match limit stopped the search.")
                    .font(.caption).foregroundStyle(.orange)
            }
            if !page.identity.discoveryComplete {
                Text("Archive discovery was incomplete; files outside the saved manifest remain unknown.")
                    .font(.caption).foregroundStyle(.orange)
            }
        }.textSelection(.enabled)
    }
    private func searchCoverage(_ page: AtlasSavedSearchPage) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Label(page.complete ? "Complete text-search coverage" : "Partial text-search coverage",
                    systemImage: page.complete ? "checkmark.circle" : "exclamationmark.circle")
                    .foregroundStyle(page.complete ? Color.secondary : Color.orange)
                Spacer()
                Text("Query generation \(page.generation)").foregroundStyle(.secondary)
            }.font(.caption.weight(.semibold))
            Text(page.counting.summary(seen: page.seen, retained: page.retained))
                .font(.caption)
            Text("Unavailable members: \(page.unavailable) · Unsupported decoding: \(page.unsupported) · \(page.sourceBytes) source bytes read")
                .font(.caption).foregroundStyle(.secondary)
            if let generation = page.indexGeneration {
                Text("Query used index generation \(generation): \(page.indexEliminatedFiles) members eliminated · \(page.fallbackFiles) direct-scan fallbacks · \(page.postingEntriesVisited) postings visited")
                    .font(.caption2).foregroundStyle(.secondary)
            } else {
                Text("This query scanned saved source without an index.").font(.caption2).foregroundStyle(.secondary)
            }
            if page.truncated && !page.counting.all {
                Text("The retained-match limit stopped the search. More matches may exist, and the seen count is a lower bound.")
                    .font(.caption).foregroundStyle(.orange)
            }
            if !page.identity.discoveryComplete {
                Text("Archive discovery was incomplete; files outside the saved manifest remain unknown.")
                    .font(.caption).foregroundStyle(.orange)
            }
        }.textSelection(.enabled)
    }
    private func searchHitRow(_ hit: AtlasSavedHit, in page: AtlasSavedSearchPage) -> some View {
        HStack(alignment: .top, spacing: 10) {
            VStack(alignment: .leading, spacing: 4) {
                Text(hit.path.display).font(.callout.weight(.semibold)).lineLimit(2).textSelection(.enabled)
                Text("Occurrence \(hit.id) · original bytes \(hit.start)..<\(hit.end) · \(hit.end - hit.start) source bytes")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            Button("Read match") { openHit(hit, from: page) }
                .disabled(!model.canOperate || !model.searchIsCurrent || readerTask != nil)
        }.padding(10).frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.primary.opacity(0.035)).clipShape(RoundedRectangle(cornerRadius: 7))
    }
    private var importsPane: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Source imports").font(.headline)
                Spacer()
                Button(model.importGeneration == nil ? "Build imports" : "Rebuild imports") {
                    panelNotice = ""; model.prepareImports()
                }
                Button("Reload", action: model.reloadImports)
                Button("Clear", action: model.clearImports).disabled(model.importGeneration == nil)
            }.disabled(!model.canOperate)
            Picker("Import direction", selection: Binding(get: { model.importDirection }, set: { model.loadImports(direction: $0) })) {
                Text("All imports").tag(AtlasSavedDirection.all)
                Text("Outgoing").tag(AtlasSavedDirection.outgoing).disabled(model.selectedMember == nil)
                Text("Imported by").tag(AtlasSavedDirection.incoming).disabled(model.selectedMember == nil)
            }.pickerStyle(.segmented).disabled(!model.canOperate || model.importGeneration == nil)
            if model.importDirection != .all, let member = model.selectedMember {
                Text("\(model.importDirection == .incoming ? "Imports targeting" : "Imports declared in") \(member.path.display)")
                    .font(.caption).lineLimit(2).textSelection(.enabled)
            }
            Text("Rust modules/use, JavaScript and TypeScript imports/re-exports, and Python imports. Targets are candidates in this saved manifest; compiler and package resolution are not established.")
                .font(.caption).foregroundStyle(.secondary)
            if let page = model.imports, model.importsAreCurrent {
                VStack(alignment: .leading, spacing: 4) {
                    Text(page.complete ? "Lexical coverage complete within the saved manifest" : "Partial lexical import coverage")
                        .font(.caption.weight(.semibold)).foregroundStyle(page.complete ? Color.secondary : Color.orange)
                    Text("\(page.scanned) / \(page.sources) eligible members parsed · \(page.retained) imports retained · \(page.sourceBytes) source bytes read")
                        .font(.caption)
                    if !page.complete {
                        Text("Unavailable: \(page.unavailable) · Unsupported: \(page.unsupported) · Source limit: \(page.sourceLimited) · Diagnostics: \(page.diagnostics)")
                            .font(.caption).foregroundStyle(.secondary)
                        if page.truncated { Text("Import retention was truncated; additional relationships may be omitted.").font(.caption).foregroundStyle(.orange) }
                        if !page.identity.discoveryComplete { Text("Archive discovery was incomplete; files outside its manifest are unknown.").font(.caption).foregroundStyle(.orange) }
                    }
                }.textSelection(.enabled)
                Divider()
                if page.links.isEmpty {
                    Text(page.complete ? "No matching lexical imports in the parsed saved members."
                        : "No matching retained imports. Partial coverage cannot establish that this member has no imports or incoming dependencies.")
                        .foregroundStyle(.secondary).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                } else {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 10) {
                            ForEach(page.links) { link in importRow(link, in: page) }
                        }
                    }.frame(maxHeight: .infinity)
                }
                HStack {
                    Button("Previous imports") { model.loadImports(direction: page.direction, start: page.start >= 64 ? page.start - 64 : 0) }
                        .disabled(!model.canOperate || page.start == 0)
                    Spacer()
                    Text("\(page.start)..<\(page.start + UInt64(page.links.count)) of \(page.matching)").font(.caption)
                    Spacer()
                    Button("Next imports") { if let next = page.next { model.loadImports(direction: page.direction, start: next) } }
                        .disabled(!model.canOperate || page.next == nil)
                }.controlSize(.small)
            } else {
                VStack(alignment: .leading, spacing: 10) {
                    Text(model.busy ? "Preparing saved import metadata…" : "Build imports to inspect source dependencies.").font(.headline)
                    Text("Select a saved member to inspect its outgoing statements or incoming catalog candidates. Read the exact statement or open a captured target without a working-tree checkout.")
                    Text("Each build admits up to 4,096 statements and 32 MiB of source, with a 128 KiB limit per source member. Unavailable, unsupported and limited sources remain visible in coverage.")
                        .font(.caption).foregroundStyle(.secondary)
                }.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading).padding(.top, 12)
            }
        }.padding(.leading, 8)
    }
    private func importRow(_ link: AtlasSavedImport, in page: AtlasSavedImports) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(verbatim: link.sourcePath.display).font(.caption.weight(.semibold)).lineLimit(2)
            Text(verbatim: link.destination.debugDescription).font(.callout).lineLimit(4).textSelection(.enabled)
            Text("\(link.label) · original bytes \(link.evidence.start)..<\(link.evidence.end)").font(.caption).foregroundStyle(.secondary)
            Text(verbatim: link.resolutionLabel).font(.caption).foregroundStyle(.secondary)
            if let target = link.targetPath { Text(verbatim: target.display).font(.caption).lineLimit(2).textSelection(.enabled) }
            HStack {
                Text(verbatim: link.resolutionBasis).font(.caption2).foregroundStyle(.secondary)
                Spacer()
                Button("Read statement") { openImport(link, from: page, side: .evidence) }
                Button("Open target candidate") { openImport(link, from: page, side: .target) }.disabled(!link.canOpenTarget)
            }.controlSize(.small).disabled(!model.canOperate || readerTask != nil)
        }.padding(10).frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.primary.opacity(0.035)).clipShape(RoundedRectangle(cornerRadius: 7))
    }
    private var relationshipsPane: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Documentation links").font(.headline)
                Spacer()
                Button(model.relationshipGeneration == nil ? "Build links and backlinks" : "Rebuild links") {
                    panelNotice = ""; model.prepareRelationships()
                }.disabled(!model.canOperate)
                Button("Reload links") { panelNotice = ""; model.reloadRelationships() }
                    .disabled(!model.canOperate)
            }
            Picker("Link direction", selection: Binding(get: { model.direction }, set: { next in
                panelNotice = ""; model.loadRelationships(direction: next)
            })) {
                ForEach(AtlasSavedDirection.allCases) { direction in
                    Text(direction.label).tag(direction)
                        .disabled(direction != .all && model.selectedMember == nil)
                }
            }.pickerStyle(.segmented).disabled(!model.canOperate || model.relationshipGeneration == nil)
            if model.direction != .all, let member = model.selectedMember {
                Text("\(model.direction == .incoming ? "Links to" : "Links from") \(member.path.display)")
                    .font(.caption).lineLimit(2).textSelection(.enabled)
            }
            if let page = model.relationships {
                coverage(page)
                Divider()
                if page.links.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("No matching retained links.").font(.headline)
                        Text(page.complete
                            ? "No matching destinations were found in the Markdown documents parsed from this saved manifest."
                            : "Coverage is partial. An empty result cannot establish that a member has no links or backlinks.")
                            .font(.callout).foregroundStyle(.secondary)
                    }.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading).padding(.top, 10)
                } else {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 10) {
                            ForEach(page.links) { link in linkRow(link, in: page) }
                        }
                    }.frame(maxHeight: .infinity)
                }
                HStack {
                    Button("Previous links") {
                        model.loadRelationships(direction: page.direction, start: page.start >= 64 ? page.start - 64 : 0)
                    }.disabled(!model.canOperate || page.start == 0)
                    Spacer()
                    Text("\(page.start)..<\(page.start + UInt64(page.links.count)) of \(page.matching)")
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button("Next links") {
                        if let next = page.next { model.loadRelationships(direction: page.direction, start: next) }
                    }.disabled(!model.canOperate || page.next == nil)
                }.controlSize(.small)
            } else {
                VStack(alignment: .leading, spacing: 10) {
                    Text(model.busy ? "Preparing saved relationship metadata…" : "Build documentation links to follow this saved repository.")
                        .font(.headline)
                    Text("The build reads captured Markdown and resolves exact saved member names. Select a member to view its outgoing links or backlinks.")
                    Text("Each build admits up to 4,096 links and 32 MiB of Markdown source, with a 256 KiB limit per document. Omitted and unavailable documents are reported.")
                        .font(.caption).foregroundStyle(.secondary)
                }.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading).padding(.top, 12)
            }
        }.padding(.leading, 8)
    }
    private func coverage(_ page: AtlasSavedRelationships) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Label(page.complete ? "Complete for the parsed saved manifest" : "Partial documentation coverage",
                    systemImage: page.complete ? "checkmark.circle" : "exclamationmark.circle")
                    .foregroundStyle(page.complete ? Color.secondary : Color.orange)
                Spacer()
                Text("Generation \(page.generation)").foregroundStyle(.secondary)
            }.font(.caption.weight(.semibold))
            Text("\(page.scanned) / \(page.markdown) Markdown members parsed · \(page.retained) links retained · \(page.sourceBytes) source bytes read")
                .font(.caption)
            if !page.complete {
                Text("Unavailable: \(page.unavailable) · Unsupported: \(page.unsupported) · Source limit: \(page.sourceLimited) · Parser diagnostics: \(page.diagnostics)")
                    .font(.caption).foregroundStyle(.secondary)
                if page.truncated {
                    Text("Link retention was truncated (\(page.seen) links seen). Additional links may be omitted.")
                        .font(.caption).foregroundStyle(.orange)
                }
                if !page.identity.discoveryComplete {
                    Text("Archive discovery was incomplete; files outside its saved manifest are unknown.")
                        .font(.caption).foregroundStyle(.orange)
                }
            }
        }.textSelection(.enabled)
    }
    private func linkRow(_ link: AtlasSavedLink, in page: AtlasSavedRelationships) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(link.sourcePath.display).font(.caption.weight(.semibold)).lineLimit(2)
            Text(link.destination.isEmpty ? "(empty link destination)" : link.destination)
                .font(.callout).lineLimit(4).textSelection(.enabled)
            HStack(alignment: .top) {
                Text(resolutionLabel(link)).font(.caption).foregroundStyle(.secondary)
                if let target = link.targetPath {
                    Text(target.display).font(.caption).lineLimit(2).textSelection(.enabled)
                }
            }
            if link.fragmentResolution != .none {
                Text(link.targetNavigationDescription)
                    .font(.caption).foregroundStyle(.secondary)
            }
            HStack {
                Text("Captured document evidence").font(.caption2).foregroundStyle(.secondary)
                Spacer()
                Button("Read source document") { openLink(link, from: page, side: .evidence) }
                Button(link.targetActionLabel) { openLink(link, from: page, side: .target) }
                    .disabled(!link.canOpenTarget)
            }.controlSize(.small).disabled(!model.canOperate || readerTask != nil)
        }.padding(10).frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.primary.opacity(0.035)).clipShape(RoundedRectangle(cornerRadius: 7))
    }
    private func resolutionLabel(_ link: AtlasSavedLink) -> String {
        switch link.resolution {
        case "captured": return "Captured target"
        case "unavailable": return "Target recorded without source bytes"
        case "missing": return "Target absent from the saved manifest"
        case "external": return "External destination retained as text"
        default: return "Unresolved destination"
        }
    }

    private func saveCurrentProject() {
        guard !exporter.busy, let source = currentSource() else { return }
        let panel = NSSavePanel()
        panel.title = "Save the current project as an FCBS archive"
        panel.prompt = "Save archive"
        panel.nameFieldStringValue = "Repository.fcbs"
        panel.message = "Captures current files from \(String(reflecting: source.root)), including plaintext source and filenames. Repository ignore rules are respected. Limits: 20,000 known files, 1 MiB per file, 64 MiB source. Omitted or unavailable files remain explicit; discovery may be incomplete. Choose a new filename."
        panel.canCreateDirectories = true
        guard panel.runModal() == .OK, let url = panel.url else { return }
        let access = AtlasSavedFileAccess(selectedURL: url)
        guard currentSource()?.identity == source.identity else {
            panelNotice = "The project or its read grant changed while choosing a destination. Review the current project and choose Save again. No export was started."
            return
        }
        guard let path = access.nativePath else {
            panelNotice = "The selected destination cannot be represented exactly by the native save interface. No export was started."
            return
        }
        panelNotice = ""
        exporter.save(source: source, destination: path, destinationAccess: access)
    }
    private func chooseArchive() {
        guard model.canOperate, readerTask == nil else { return }
        let panel = NSOpenPanel()
        panel.title = "Open an FCBS saved repository"
        panel.message = "Read captured members and documentation links from the selected archive. Saved member names never grant access to working-tree files."
        panel.canChooseFiles = true; panel.canChooseDirectories = false; panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        let access = AtlasSavedFileAccess(selectedURL: url)
        guard let path = access.nativePath else {
            panelNotice = "The selected archive filename cannot be represented exactly by the native open interface."; return
        }
        panelNotice = ""; model.open(path: path, accessLease: access)
    }
    private func chooseIndex() {
        guard model.canChangeIndex, AtlasSavedWire.digest(model.indexPinInput), readerTask == nil else { return }
        let pin = model.indexPinInput
        let panel = NSOpenPanel()
        panel.title = "Attach an FCBD saved search index"
        panel.message = "Select the demand-paged index built for this exact saved archive. Its page-manifest digest must match the trusted build receipt entered above."
        panel.canChooseFiles = true; panel.canChooseDirectories = false; panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        let access = AtlasSavedFileAccess(selectedURL: url)
        guard let path = access.nativePath else {
            panelNotice = "The selected index filename cannot be represented exactly by the native open interface."; return
        }
        panelNotice = ""; model.attachIndex(path: path, pin: pin, accessLease: access)
    }
    private func openMember(_ member: AtlasSavedMember, from identity: AtlasSavedIdentity) {
        launchReader {
            try await model.openMember(member, from: identity, readerTransport: .native,
                search: .native, outline: .native, document: .native)
        }
    }
    private func openLink(_ link: AtlasSavedLink, from page: AtlasSavedRelationships, side: AtlasSavedLinkSide) {
        launchReader {
            try await model.openLink(link, from: page, side: side, readerTransport: .native,
                search: .native, outline: .native, document: .native)
        }
    }
    private func openImport(_ link: AtlasSavedImport, from page: AtlasSavedImports, side: AtlasSavedLinkSide) {
        launchReader {
            try await model.openImport(link, from: page, side: side, readerTransport: .native,
                search: .native, outline: .native, document: .native)
        }
    }
    private func openHit(_ hit: AtlasSavedHit, from page: AtlasSavedSearchPage) {
        launchReader {
            try await model.openHit(hit, from: page, readerTransport: .native,
                search: .native, outline: .native, document: .native)
        }
    }
    private func openStructureHit(_ item: AtlasSavedStructureHit, from page: AtlasSavedStructurePage) {
        launchReader {
            try await model.openStructureHit(item, from: page, readerTransport: .native,
                search: .native, outline: .native, document: .native)
        }
    }
    private func openExpressionHit(_ hit: AtlasSavedHit, from page: AtlasSavedExpressionPage) {
        launchReader {
            try await model.openExpressionHit(hit, from: page, readerTransport: .native,
                search: .native, outline: .native, document: .native)
        }
    }
    private func launchReader(_ open: @escaping @MainActor () async throws -> AtlasSavedReaderCapture) {
        guard model.canOperate, readerTask == nil else { return }
        panelNotice = ""
        readerTask = Task {
            do {
                let capture = try await open()
                if Task.isCancelled { capture.reader.close() }
                else { readerCapture = capture }
            } catch {
                if !Task.isCancelled {
                    panelNotice = (error as? AtlasSavedError)?.message
                        ?? (error as? AtlasReaderError)?.message ?? "The captured member could not be opened."
                }
            }
            readerTask = nil
        }
    }
}

@MainActor private struct AtlasSavedReaderView: View {
    @Bindable var archive: AtlasSavedRepositoryModel
    @Bindable var desk: AtlasReadingDeskModel
    @Bindable var pins: AtlasPinnedReaders<AtlasPagedReaderModel>
    @Environment(\.dismiss) private var dismiss
    @State private var showsDesk = false
    @State private var matchNotice = ""
    @State private var history: AtlasSavedReaderHistory
    @State private var linkTask: Task<Void, Never>?
    @State private var linkOperation = UUID()
    @State private var linkNotice = ""

    init(capture: AtlasSavedReaderCapture, archive: AtlasSavedRepositoryModel, desk: AtlasReadingDeskModel,
         pins: AtlasPinnedReaders<AtlasPagedReaderModel>) {
        self.archive = archive; self.desk = desk; self.pins = pins
        _history = State(initialValue: AtlasSavedReaderHistory(capture))
    }
    private var capture: AtlasSavedReaderCapture { history.current }
    private var pinGroup: String { "saved-repository:\(capture.owner)" }
    private var canKeep: Bool {
        desk.canOperate && desk.readerDomain != nil && capture.reader.retainedRegistryDomain == desk.readerDomain
    }

    var body: some View {
        VStack(spacing: 8) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 3) {
                    Text(capture.member.path.display).font(.headline).lineLimit(2).textSelection(.enabled)
                    Text(capture.evidenceLabel).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                }
                Spacer()
                Button("Reading Desk") { showsDesk = true }
                Button("Done") { dismiss() }.keyboardShortcut(.cancelAction)
            }.controlSize(.small).padding(.horizontal, 12).padding(.top, 12)
            HStack {
                Button { history.back(); matchNotice = ""; linkNotice = "" } label: {
                    Label("Back", systemImage: "chevron.left")
                }.disabled(!history.canBack || linkTask != nil)
                Button { history.forward(); matchNotice = ""; linkNotice = "" } label: {
                    Label("Forward", systemImage: "chevron.right")
                }.disabled(!history.canForward || linkTask != nil)
                Text("Saved reader \(history.position + 1) of \(history.entries.count)")
                    .font(.caption).foregroundStyle(.secondary)
                Spacer()
                if linkTask != nil {
                    ProgressView().controlSize(.small)
                    Button("Cancel link") { linkTask?.cancel(); linkNotice = "Cancel requested; waiting for entered archive work." }
                }
            }.controlSize(.small).padding(.horizontal, 12)
            if !linkNotice.isEmpty {
                Text(verbatim: linkNotice).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 12)
            }
            if let match = capture.match { savedMatchActions(match) }
            else if case .importLink(_, let link, .evidence) = capture.origin {
                HStack {
                    Text("Exact lexical statement in this captured source. Use the reader selection tools to inspect its original bytes.")
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button("Return to statement") {
                        capture.reader.offsetInput = String(link.evidence.contextStart)
                        capture.reader.goToByte()
                    }.disabled(!capture.reader.canPin)
                }.controlSize(.small).padding(.horizontal, 12)
            }
            else if case .structureHit(_, _, _, _, let item) = capture.origin {
                HStack {
                    Text("This result spans more than the bounded selection witness. The reader starts near its original source range.")
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button("Return to result") {
                        capture.reader.offsetInput = String(item.hit.start > 132 ? item.hit.start - 132 : 0)
                        capture.reader.goToByte()
                    }.disabled(!capture.reader.canPin)
                }.controlSize(.small).padding(.horizontal, 12)
            }
            HSplitView {
                AtlasPagedSourceView(model: capture.reader, canPin: pins.canAdd, pin: { reader in
                    pins.pin(reader, root: pinGroup, label: capture.member.path.display)
                }, canKeep: canKeep, keep: keep, openLocalLink: localNavigation)
                .id(capture.id)
                .frame(minWidth: 420, maxWidth: .infinity, maxHeight: .infinity)
                if !pins.entries.isEmpty || pins.busy {
                    AtlasPinnedReadersView(readers: pins, canKeep: canKeep, keep: keep)
                        .frame(minWidth: 350, idealWidth: 480)
                }
            }
            Text(capture.archiveIdentity != nil && archive.info?.identity == capture.archiveIdentity
                ? "Markdown local links follow captured members in this archive. The first link may prepare its bounded link inventory. Back and Forward retain up to four readers; pin a source or keep it in the Desk to preserve it longer."
                : "The originating archive is closed or replaced. Retained readers, Back, Forward, pinning and the Desk remain available; new local-link navigation is disabled.")
                .font(.caption).foregroundStyle(.secondary).frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 12)
            Text(capture.match == nil
                ? "Keep in Desk preserves the current original-byte page. Add bookmarks or notes there and save a checkpoint to retain your reading work."
                : "Keep saved match preserves the exact range above. The reader toolbar's Keep in Desk preserves the current page. Add a note or bookmark and save a checkpoint in the Desk.")
                .font(.caption).foregroundStyle(.secondary).frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 12).padding(.bottom, 8)
        }
        .frame(minWidth: 980, idealWidth: 1280, minHeight: 680, idealHeight: 820)
        .onAppear { pins.bind(root: pinGroup) }
        .sheet(isPresented: $showsDesk) { AtlasReadingDeskView(model: desk) }
        .onDisappear {
            linkOperation = UUID(); linkTask?.cancel(); linkTask = nil
            history.close()
        }
    }
    private var localNavigation: AtlasDocumentPreviewModel.LocalNavigation? {
        guard linkTask == nil, archive.canFollowDocumentLinks(from: capture) else { return nil }
        let source = capture
        return { target in openLink(target, from: source) }
    }
    private func openLink(_ target: AtlasReaderDocumentLinkTarget, from source: AtlasSavedReaderCapture) {
        guard linkTask == nil, history.current.id == source.id,
              archive.canFollowDocumentLinks(from: source) else { return }
        let ticket = UUID(); linkOperation = ticket
        linkNotice = "Resolving this captured occurrence within the saved archive…"
        linkTask = Task {
            defer { if linkOperation == ticket { linkTask = nil } }
            do {
                let candidate = try await archive.openDocumentLink(target, from: source,
                    readerTransport: .native, search: .native, outline: .native, document: .native)
                guard linkOperation == ticket, !Task.isCancelled, history.current.id == source.id,
                      archive.canFollowDocumentLinks(from: source), history.accept(candidate) else {
                    candidate.reader.close(); return
                }
                matchNotice = ""
                linkNotice = "Opened captured source. Back restores the previous reader and its source page."
            } catch {
                guard linkOperation == ticket else { return }
                linkNotice = Task.isCancelled ? "Link navigation canceled. The originating captured reader remains available."
                    : ((error as? AtlasSavedError)?.message ?? (error as? AtlasReaderError)?.message
                        ?? "The saved link could not be opened. The originating reader remains available.")
            }
        }
    }
    private func savedMatchActions(_ match: AtlasSavedMatchEvidence) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            if case .hit(_, let needle, _, let caseSensitive) = capture.origin {
                Text("Saved literal: \(needle)").font(.callout.monospaced()).lineLimit(2).textSelection(.enabled)
                Text(caseSensitive ? "Match case" : "Unicode lowercase matching").font(.caption).foregroundStyle(.secondary)
            } else if case .expressionHit(_, let expression, let primary, _) = capture.origin {
                Text("Saved expression: \(expression)").font(.callout.monospaced()).lineLimit(2).textSelection(.enabled)
                Text("Primary occurrence: \(primary)").font(.caption.monospaced()).lineLimit(2).textSelection(.enabled)
            } else if case .structureHit(_, _, _, _, let item) = capture.origin {
                Text(item.structure.name).font(.callout.monospaced()).lineLimit(2).textSelection(.enabled)
                Text(item.structure.label).font(.caption).foregroundStyle(.secondary)
            }
            HStack {
                Text("Exact saved match · \(match.end - match.start) original bytes")
                    .font(.caption.weight(.semibold))
                Spacer()
                Button("Return to saved match") {
                    capture.reader.offsetInput = String(match.contextStart); capture.reader.goToByte()
                }.disabled(!capture.canUseMatch)
                Button("Copy match bytes (hex)") {
                    NSPasteboard.general.clearContents()
                    matchNotice = NSPasteboard.general.setString(match.originalHex, forType: .string)
                        ? "Copied the exact saved match bytes as hexadecimal." : "The pasteboard did not accept the match bytes."
                }
                Button("Keep saved match in Desk", action: keepSavedMatch)
                    .disabled(!canKeep || !capture.canUseMatch)
            }.controlSize(.small)
            Text("Original bytes (hex): \(String(match.originalHex.prefix(256)))\(match.originalHex.count > 256 ? "…" : "")")
                .font(.caption.monospaced()).foregroundStyle(.secondary).lineLimit(3).textSelection(.enabled)
            if !matchNotice.isEmpty { Text(matchNotice).font(.caption).foregroundStyle(.secondary) }
        }.padding(.horizontal, 12)
    }
    private func keepSavedMatch() {
        guard canKeep, capture.canUseMatch, let match = capture.match,
              let domain = capture.reader.retainedRegistryDomain, domain == desk.readerDomain else { return }
        desk.keepCapture(start: match.start, end: match.end, sourceDomain: domain) { work in
            try await capture.withMatchReader(work)
        }
        showsDesk = true
    }
    private func keep(_ reader: AtlasPagedReaderModel) {
        guard canKeep, reader.canPin, reader.retainedRegistryDomain == desk.readerDomain else { return }
        desk.keep(reader: reader); showsDesk = true
    }
}
