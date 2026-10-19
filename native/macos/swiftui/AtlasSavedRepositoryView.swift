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
                    relationshipsPane.frame(minWidth: 620, maxWidth: .infinity, maxHeight: .infinity)
                }
            } else {
                VStack(spacing: 14) {
                    Image(systemName: "archivebox").font(.system(size: 42)).foregroundStyle(.secondary)
                    Text("Read a saved repository").font(.title2.weight(.semibold))
                    Text("Choose an FCBS archive to browse its captured files, read source and Markdown, and follow documentation links and backlinks.")
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
            Text("Source evidence is the original captured document. Link positions and URL fragments are not resolved. Member names never open working-tree paths.")
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
            Text("Keep in Desk preserves the current original-byte page. Add bookmarks or notes there and save a checkpoint to retain your reading work.")
                .font(.caption).foregroundStyle(.secondary).frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 12).padding(.bottom, 8)
        }
        .frame(minWidth: 980, idealWidth: 1280, minHeight: 680, idealHeight: 820)
        .onAppear { pins.bind(root: pinGroup) }
        .sheet(isPresented: $showsDesk) { AtlasReadingDeskView(model: desk) }
        .onDisappear { capture.reader.close() }
    }
    private func keep(_ reader: AtlasPagedReaderModel) {
        guard canKeep, reader.canPin, reader.retainedRegistryDomain == desk.readerDomain else { return }
        desk.keep(reader: reader); showsDesk = true
    }
}
