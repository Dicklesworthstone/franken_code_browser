import SwiftUI
import AppKit

/// A successful native open panel already supplies selected-file access. Adopt
/// it once, and release it only after the retained archive descriptor drains.
private final class AtlasSavedFileAccess {
    let url: URL
    init(selectedURL: URL) { url = selectedURL }
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
    deinit { url.stopAccessingSecurityScopedResource() }
}

/// A selected FCBS archive is a complete independent browsing entry point. The
/// list owns only bounded metadata pages; source reads happen through verified
/// empty-reader handoffs on the worker, never through displayed member names.
@MainActor struct AtlasSavedRepositoryView: View {
    @Bindable var model: AtlasSavedRepositoryModel
    @Bindable var desk: AtlasReadingDeskModel
    @Environment(\.dismiss) private var dismiss
    @State private var panelNotice = ""
    @State private var section: Section = .text
    private enum Section: String, CaseIterable, Identifiable {
        case text = "Text search", links = "Documentation links"
        var id: String { rawValue }
    }
    @State private var readerCapture: AtlasSavedReaderCapture?
    @State private var readerTask: Task<Void, Never>?
    @State private var pins = AtlasPinnedReaders<AtlasPagedReaderModel>(
        canFork: { $0.canPin }, fork: { try await $0.fork() }, retire: { $0.close() },
        failureMessage: { ($0 as? AtlasReaderError)?.message ?? "The saved capture could not be pinned." })

    var body: some View {
        VStack(spacing: 10) {
            header
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
                        if section == .text { searchPane } else { relationshipsPane }
                    }.frame(minWidth: 620, maxWidth: .infinity, maxHeight: .infinity)
                }
            } else {
                VStack(spacing: 14) {
                    Image(systemName: "archivebox").font(.system(size: 42)).foregroundStyle(.secondary)
                    Text("Read a saved repository").font(.title2.weight(.semibold))
                    Text("Choose an FCBS archive to search its captured text, read source and Markdown, and follow documentation links and backlinks.")
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
            AtlasSavedReaderView(capture: capture, desk: desk, pins: pins)
        }
        .onDisappear {
            readerTask?.cancel(); readerCapture?.reader.close(); pins.closeAll()
            model.cancel()
        }
    }

    private var header: some View {
        HStack {
            Label("Saved repository", systemImage: "archivebox").font(.title2.weight(.semibold))
            Spacer()
            Button("Open archive…", action: chooseArchive).disabled(!model.canOperate || readerTask != nil)
            Button("Close archive", action: model.closeArchive).disabled(!model.canOperate || model.info == nil)
            Button("Done") {
                readerTask?.cancel(); model.cancel(); dismiss()
            }.keyboardShortcut(.cancelAction)
        }
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
                    ? "Literal search is case sensitive and does not interpret query syntax or normalize Unicode. Member names never open working-tree paths."
                    : "Expression terms are exact and case sensitive. Required/excluded terms apply across each complete captured member; results identify primary-term occurrences. Member names never open working-tree paths.")
                : "Documentation evidence is the original captured document. Individual link positions and URL fragments are not resolved. Member names never open working-tree paths.")
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
            panelNotice = ""; model.selectMember(member, from: page.identity)
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
            if model.searchMode == .literal { literalSearchPane } else { expressionSearchPane }
        }
    }
    private var literalSearchPane: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                TextField("Literal text in all captured members", text: $model.searchInput)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { panelNotice = ""; model.search() }
                    .accessibilityLabel("Case-sensitive literal search across the saved archive")
                Button { panelNotice = ""; model.search() } label: { Label("Search archive", systemImage: "magnifyingglass") }
                    .disabled(!model.canOperate || !model.validSearchInput)
                Button("Clear") { panelNotice = ""; model.clearSearch() }
                    .disabled(!model.canOperate || (model.searchPage == nil && model.searchInput.isEmpty))
            }
            HStack {
                Text("Case-sensitive decoded literal · entire saved archive · up to 4,096 retained matches")
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
                    if !model.searchIsCurrent {
                        HStack {
                            Text("The draft differs from these results. Submit it or restore the accepted query before opening a match.")
                                .font(.caption).foregroundStyle(.orange)
                            Spacer()
                            Button("Use accepted query") { model.searchInput = page.needle; panelNotice = "" }
                                .controlSize(.small).disabled(!model.canOperate)
                        }
                    }
                }
                searchCoverage(page)
                Divider()
                if page.hits.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("No retained text matches.").font(.headline)
                        Text(page.complete
                            ? "The accepted literal has no matches in the complete saved manifest."
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
                Text("Case sensitive · 64 terms and filters maximum · up to 4,096 matches · 64 MiB matching-work budget")
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
                            Button("Use accepted expression") { model.searchInput = page.expression; panelNotice = "" }
                                .controlSize(.small).disabled(!model.canOperate)
                        }
                    }
                }
                expressionCoverage(page)
                Divider()
                if page.hits.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("No retained expression matches.").font(.headline)
                        Text(page.complete ? "The accepted expression has no matches in the complete captured scope."
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
    private func expressionCoverage(_ page: AtlasSavedExpressionPage) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Label(page.complete ? "Complete expression coverage" : "Partial expression coverage",
                systemImage: page.complete ? "checkmark.circle" : "exclamationmark.circle")
                .font(.caption.weight(.semibold)).foregroundStyle(page.complete ? Color.secondary : Color.orange)
            Text("\(page.retained) matches retained · \(page.scopeFiles) members in scope · \(page.filteredFiles) filtered by path/language")
                .font(.caption)
            Text("Unavailable: \(page.unavailable) · Unsupported decoding: \(page.unsupported) · Predicate rejections: \(page.predicateRejections)")
                .font(.caption).foregroundStyle(.secondary)
            Text("\(page.sourceBytes) source bytes read · \(page.scannedBytes) / \(page.scanLimit) matching bytes processed")
                .font(.caption).foregroundStyle(.secondary)
            if page.state == "work-limit" {
                Text("The matching-work budget was exhausted. Earlier matches remain valid; more may exist. Narrow path or language filters and search again.")
                    .font(.caption).foregroundStyle(.orange)
            } else if page.truncated {
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
            Text(page.truncated
                ? "\(page.retained) matches retained · at least \(page.seen) matches seen"
                : "\(page.retained) matches retained from scanned captures")
                .font(.caption)
            Text("Unavailable members: \(page.unavailable) · Unsupported decoding: \(page.unsupported) · \(page.sourceBytes) source bytes read")
                .font(.caption).foregroundStyle(.secondary)
            if page.truncated {
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
            if link.fragmentUnresolved {
                Text("URL fragment unresolved. A captured target opens at the start of its file.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            HStack {
                Text("Captured document evidence").font(.caption2).foregroundStyle(.secondary)
                Spacer()
                Button("Read source document") { openLink(link, from: page, side: .evidence) }
                Button("Read captured target") { openLink(link, from: page, side: .target) }
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
    private func openHit(_ hit: AtlasSavedHit, from page: AtlasSavedSearchPage) {
        launchReader {
            try await model.openHit(hit, from: page, readerTransport: .native,
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
    let capture: AtlasSavedReaderCapture
    @Bindable var desk: AtlasReadingDeskModel
    @Bindable var pins: AtlasPinnedReaders<AtlasPagedReaderModel>
    @Environment(\.dismiss) private var dismiss
    @State private var showsDesk = false
    @State private var matchNotice = ""
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
            if let match = capture.match { savedMatchActions(match) }
            HSplitView {
                AtlasPagedSourceView(model: capture.reader, canPin: pins.canAdd, pin: { reader in
                    pins.pin(reader, root: pinGroup, label: capture.member.path.display)
                }, canKeep: canKeep, keep: keep)
                .frame(minWidth: 420, maxWidth: .infinity, maxHeight: .infinity)
                if !pins.entries.isEmpty || pins.busy {
                    AtlasPinnedReadersView(readers: pins, canKeep: canKeep, keep: keep)
                        .frame(minWidth: 350, idealWidth: 480)
                }
            }
            Text(capture.match == nil
                ? "Keep in Desk preserves the current original-byte page. Add bookmarks or notes there and save a checkpoint to retain your reading work."
                : "Keep saved match preserves the exact range above. The reader toolbar's Keep in Desk preserves the current page. Add a note or bookmark and save a checkpoint in the Desk.")
                .font(.caption).foregroundStyle(.secondary).frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 12).padding(.bottom, 8)
        }
        .frame(minWidth: 980, idealWidth: 1280, minHeight: 680, idealHeight: 820)
        .onAppear { pins.bind(root: pinGroup) }
        .sheet(isPresented: $showsDesk) { AtlasReadingDeskView(model: desk) }
        .onDisappear { capture.reader.close() }
    }
    private func savedMatchActions(_ match: AtlasSavedMatchEvidence) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            if case .hit(_, let needle, _) = capture.origin {
                Text("Saved literal: \(needle)").font(.callout.monospaced()).lineLimit(2).textSelection(.enabled)
            } else if case .expressionHit(_, let expression, let primary, _) = capture.origin {
                Text("Saved expression: \(expression)").font(.callout.monospaced()).lineLimit(2).textSelection(.enabled)
                Text("Primary occurrence: \(primary)").font(.caption.monospaced()).lineLimit(2).textSelection(.enabled)
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
