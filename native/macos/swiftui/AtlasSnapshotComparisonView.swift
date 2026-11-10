import SwiftUI
import AppKit

/// Explicit archive choices are native capabilities. Member display paths are
/// never used to open files. All comparison rows and reader imports come from
/// the shared Rust service and stay bound to its accepted archive pair.
@MainActor struct AtlasSnapshotComparisonView: View {
    @Bindable var desk: AtlasReadingDeskModel
    @State private var model = AtlasSnapshotComparisonModel(transport: .native)
    @State private var beforeChoice: Choice?
    @State private var afterChoice: Choice?
    @State private var panelNotice = ""
    @State private var readerTask: Task<Void, Never>?
    @State private var readerTicket = UUID()
    @State private var selection: AtlasSnapshotComparisonSelection?
    @State private var pageStarts: [UInt64] = []
    @State private var paginationScope: PaginationScope?
    @Environment(\.dismiss) private var dismiss

    private struct Choice {
        let path: String
        let access: AtlasSavedFileAccess
        var label: String { String(reflecting: path) }
    }
    private struct PaginationScope: Equatable {
        let owner: UInt64
        let generation: UInt64
        let filter: AtlasSnapshotComparisonFilter
    }
    private var canChoose: Bool { model.canOperate && readerTask == nil }
    private var canCompare: Bool {
        canChoose && beforeChoice != nil && afterChoice != nil
    }
    private var canRead: Bool { model.canOperate && readerTask == nil }

    var body: some View {
        VStack(spacing: 0) {
            header
            choices
            Divider()
            if let page = model.page {
                acceptedSummary(page)
                pageControls(page)
                Divider()
                changeList(page)
            } else {
                VStack(spacing: 14) {
                    Image(systemName: "doc.on.doc").font(.system(size: 42)).foregroundStyle(.secondary)
                    Text("Compare two saved project captures").font(.title2.weight(.semibold))
                    Text("Choose the before and after FCBS archives, then compare their captured files. Save captures from Saved Repository to review a project across observations.")
                        .multilineTextAlignment(.center).frame(maxWidth: 560)
                    if canCompare { Button("Compare captures", action: compare) }
                }.padding(24).frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            Divider()
            footer
        }
        .frame(minWidth: 980, idealWidth: 1180, minHeight: 640, idealHeight: 780)
        .sheet(item: $selection) { item in
            AtlasSnapshotComparisonSelectionView(selection: item, desk: desk)
        }
        .onChange(of: model.page?.id) { _, _ in rememberPage() }
        .onDisappear { close() }
    }

    private var header: some View {
        HStack(spacing: 10) {
            Label("Compare saved captures", systemImage: "arrow.left.arrow.right")
                .font(.title2.weight(.semibold))
            Spacer()
            if model.busy {
                ProgressView().controlSize(.small)
                Button("Cancel comparison", action: model.cancel)
            }
            if readerTask != nil {
                ProgressView().controlSize(.small)
                Button("Cancel opening", action: cancelReader)
            }
            Button("Done") { close(); dismiss() }.keyboardShortcut(.cancelAction)
        }.padding(14).background(.bar)
    }

    private var choices: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 16) {
                archiveChoice("Before", choice: beforeChoice, side: .before)
                archiveChoice("After", choice: afterChoice, side: .after)
                Button("Swap") {
                    let old = beforeChoice
                    beforeChoice = afterChoice
                    afterChoice = old
                }
                .disabled(!canChoose || beforeChoice == nil || afterChoice == nil)
                .help("Swap the selected archives; the accepted comparison changes only after Compare")
                Button(model.page == nil ? "Compare captures" : "Compare selected captures", action: compare)
                    .disabled(!canCompare).keyboardShortcut(.defaultAction)
            }
            Text("Paths align the saved observations. Definitive additions and removals require compatible capture policies and complete opposite-side discovery.")
                .font(.caption).foregroundStyle(.secondary)
        }.padding(14)
    }

    private func archiveChoice(_ title: String, choice: Choice?,
                               side: AtlasSnapshotComparisonSide) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(title).font(.headline)
                Button("Choose…") { choose(side) }.disabled(!canChoose)
                    .accessibilityLabel("Choose the \(title.lowercased()) saved capture")
            }
            Text(verbatim: choice?.label ?? "No archive selected")
                .font(.caption.monospaced()).lineLimit(2).truncationMode(.middle)
                .textSelection(.enabled)
        }.frame(maxWidth: .infinity, alignment: .leading)
    }

    private func acceptedSummary(_ page: AtlasSnapshotComparisonPage) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Loaded comparison").font(.headline)
            Text(verbatim: "Before: " + page.identity.before.path.display)
                .font(.caption).lineLimit(2).textSelection(.enabled)
            Text(verbatim: "After: " + page.identity.after.path.display)
                .font(.caption).lineLimit(2).textSelection(.enabled)
            HStack(spacing: 14) {
                Text("\(page.progress.changed) changed")
                Text("\(page.progress.added) added")
                Text("\(page.progress.removed) removed")
                Text("\(page.progress.unchanged) unchanged")
                Text("\(page.progress.uncertain) uncertain")
                    .foregroundStyle(page.progress.uncertain == 0 ? Color.secondary : Color.orange)
            }.font(.caption.weight(.semibold))
            if !page.identity.membershipComparable {
                Text("Saved membership is not fully comparable. “Only before” and “Only after” retain uncertainty about files outside a captured scope.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }.padding(.horizontal, 14).padding(.vertical, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func pageControls(_ page: AtlasSnapshotComparisonPage) -> some View {
        HStack(spacing: 10) {
            Picker("Files to show", selection: Binding(
                get: { model.filter }, set: { model.setFilter($0) })) {
                ForEach(AtlasSnapshotComparisonFilter.allCases, id: \.self) { filter in
                    Text(label(filter.rawValue)).tag(filter)
                }
            }.frame(width: 270).disabled(!canRead)
            Button("Previous") { previousPage(page) }
                .disabled(!canRead || previousStart(page) == nil)
            Button("Next") {
                if let next = page.next { model.loadPage(start: next) }
            }.disabled(!canRead || page.next == nil)
            Text(page.rows.isEmpty ? "No files on this page"
                 : "\(page.start + 1)–\(page.start + UInt64(page.rows.count)) of \(page.total)")
                .font(.caption).foregroundStyle(.secondary)
            Spacer()
        }.controlSize(.small).padding(.horizontal, 14).padding(.vertical, 8)
    }

    private func changeList(_ page: AtlasSnapshotComparisonPage) -> some View {
        List {
            ForEach(page.rows) { row in
                VStack(alignment: .leading, spacing: 7) {
                    HStack(alignment: .top, spacing: 12) {
                        Label(label(row.kind.rawValue), systemImage: symbol(row.kind.rawValue))
                            .font(.caption.weight(.semibold)).frame(width: 150, alignment: .leading)
                        Text(verbatim: row.path.display)
                            .font(.system(.body, design: .monospaced))
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                        Button("Before") { open(row, page: page, side: .before) }
                            .disabled(!canRead || !row.canOpen(.before))
                            .accessibilityLabel("Read before capture of \(row.path.display)")
                        Button("After") { open(row, page: page, side: .after) }
                            .disabled(!canRead || !row.canOpen(.after))
                            .accessibilityLabel("Read after capture of \(row.path.display)")
                        Button("Compare file") { open(row, page: page, side: nil) }
                            .disabled(!canRead || !row.canOpen(.before) || !row.canOpen(.after))
                            .accessibilityLabel("Compare exact before and after sources for \(row.path.display)")
                    }
                    Text(sideDescription(row, side: .before) + "  ·  " + sideDescription(row, side: .after))
                        .font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                    if row.kind.rawValue == "only-before" || row.kind.rawValue == "only-after" {
                        Text("Presence differs in the saved scopes; a filesystem addition or removal is not established.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }.padding(.vertical, 5)
            }
            if page.rows.isEmpty {
                Text(page.total == 0 ? "No files match this filter in the accepted comparison." : "No files on this page.")
                    .foregroundStyle(.secondary)
            }
        }.listStyle(.inset).disabled(model.busy)
    }

    private var footer: some View {
        VStack(alignment: .leading, spacing: 4) {
            if !panelNotice.isEmpty { Text(panelNotice) }
            if !model.notice.isEmpty { Text(model.notice) }
            if let progress = model.progress, model.busy {
                Text(progress.summary).foregroundStyle(.secondary)
            } else if model.page != nil {
                Text(model.coverage).foregroundStyle(.secondary)
            }
            Text("Opening a side verifies its archived bytes and creates an independent retained reader. File comparison reports exact or budget-limited original-byte ranges.")
                .foregroundStyle(.secondary)
        }.font(.caption).textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading).padding(12)
    }

    private func choose(_ side: AtlasSnapshotComparisonSide) {
        guard canChoose else { return }
        let panel = NSOpenPanel()
        panel.title = side == .before ? "Choose the before FCBS capture" : "Choose the after FCBS capture"
        panel.message = "Select an application-captured FCBS archive. Its saved members are compared without opening the working project."
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        let access = AtlasSavedFileAccess(selectedURL: url)
        guard let path = access.nativePath, path.utf8.count <= 4096 else {
            panelNotice = "The archive path must be an exact absolute UTF-8 path of at most 4096 bytes."
            return
        }
        let choice = Choice(path: path, access: access)
        if side == .before { beforeChoice = choice } else { afterChoice = choice }
        panelNotice = ""
    }

    private func compare() {
        guard canCompare, let beforeChoice, let afterChoice else { return }
        panelNotice = ""
        model.open(before: beforeChoice.path, after: afterChoice.path,
            beforeAccess: beforeChoice.access, afterAccess: afterChoice.access)
    }

    private func open(_ row: AtlasSnapshotComparisonRow, page: AtlasSnapshotComparisonPage,
                      side: AtlasSnapshotComparisonSide?) {
        guard canRead else { return }
        let ticket = UUID()
        readerTicket = ticket
        panelNotice = ""
        readerTask = Task {
            var before: AtlasSnapshotComparisonReaderCapture?
            var after: AtlasSnapshotComparisonReaderCapture?
            do {
                if side == nil || side == .before {
                    before = try await model.open(row, from: page, side: .before,
                        readerTransport: .native, search: .native, outline: .native, document: .native)
                }
                try Task.checkCancellation()
                if side == nil || side == .after {
                    after = try await model.open(row, from: page, side: .after,
                        readerTransport: .native, search: .native, outline: .native, document: .native)
                }
                guard readerTicket == ticket, !Task.isCancelled, !model.closed else {
                    before?.reader.close(); after?.reader.close()
                    return
                }
                selection = AtlasSnapshotComparisonSelection(path: row.path.display,
                    beforeArchive: page.identity.before.path.display,
                    afterArchive: page.identity.after.path.display, before: before, after: after)
                readerTask = nil
            } catch {
                before?.reader.close(); after?.reader.close()
                guard readerTicket == ticket, !Task.isCancelled else { return }
                readerTask = nil
                panelNotice = (error as? AtlasSnapshotComparisonError)?.message
                    ?? (error as? AtlasReaderError)?.message ?? "The saved source could not be opened."
            }
        }
    }

    private func cancelReader() {
        readerTicket = UUID()
        readerTask?.cancel()
        readerTask = nil
    }
    private func close() {
        cancelReader()
        selection?.close()
        selection = nil
        model.close()
        beforeChoice = nil
        afterChoice = nil
    }
    private func scope(_ page: AtlasSnapshotComparisonPage) -> PaginationScope {
        PaginationScope(owner: page.identity.owner, generation: page.generation, filter: page.filter)
    }
    private func rememberPage() {
        guard let page = model.page else { return }
        let nextScope = scope(page)
        if paginationScope != nextScope {
            paginationScope = nextScope
            pageStarts = []
        }
        if !pageStarts.contains(page.start), pageStarts.count < 131_072 {
            pageStarts.append(page.start)
        }
    }
    private func previousStart(_ page: AtlasSnapshotComparisonPage) -> UInt64? {
        guard paginationScope == scope(page) else { return nil }
        return pageStarts.filter { $0 < page.start }.max()
    }
    private func previousPage(_ page: AtlasSnapshotComparisonPage) {
        if let start = previousStart(page) { model.loadPage(start: start) }
    }
    private func sideDescription(_ row: AtlasSnapshotComparisonRow,
                                 side: AtlasSnapshotComparisonSide) -> String {
        let title = side == .before ? "Before" : "After"
        guard let member = side == .before ? row.before : row.after else { return title + ": absent from saved scope" }
        if member.captured, let bytes = member.bytes { return title + ": \(bytes) captured bytes" }
        return title + ": unavailable" + (member.unavailableReason.map { " (\($0))" } ?? "")
    }
    private func label(_ value: String) -> String {
        switch value {
        case "all": return "All files"
        case "differences": return "Differences and uncertainty"
        case "unchanged": return "Unchanged"
        case "changed": return "Changed"
        case "added": return "Added in saved scope"
        case "removed": return "Removed from saved scope"
        case "only-before": return "Only before · uncertain"
        case "only-after": return "Only after · uncertain"
        default: return "Unavailable"
        }
    }
    private func symbol(_ value: String) -> String {
        switch value {
        case "unchanged": return "equal.circle"
        case "changed": return "pencil.circle"
        case "added": return "plus.circle"
        case "removed": return "minus.circle"
        default: return "questionmark.circle"
        }
    }
}

@MainActor private final class AtlasSnapshotComparisonSelection: Identifiable {
    let id = UUID()
    let path: String
    let beforeArchive: String
    let afterArchive: String
    let before: AtlasSnapshotComparisonReaderCapture?
    let after: AtlasSnapshotComparisonReaderCapture?
    init(path: String, beforeArchive: String, afterArchive: String,
         before: AtlasSnapshotComparisonReaderCapture?, after: AtlasSnapshotComparisonReaderCapture?) {
        self.path = path; self.beforeArchive = beforeArchive; self.afterArchive = afterArchive
        self.before = before; self.after = after
    }
    func close() {
        before?.reader.close()
        after?.reader.close()
    }
}

@MainActor private struct AtlasSnapshotComparisonSelectionView: View {
    let selection: AtlasSnapshotComparisonSelection
    @Bindable var desk: AtlasReadingDeskModel
    @State private var showsDesk = false
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        Group {
            if let before = selection.before, let after = selection.after {
                AtlasReaderComparisonView(beforeLabel: selection.beforeArchive + " · " + selection.path,
                    afterLabel: selection.afterArchive + " · " + selection.path,
                    before: before.reader, after: after.reader,
                    title: "Compare saved file",
                    roleExplanation: "Before and after are the exact members from the selected saved captures. The ranges below use original bytes.",
                    canKeep: canKeep, keep: keep)
            } else if let capture = selection.before ?? selection.after {
                VStack(spacing: 0) {
                    HStack {
                        VStack(alignment: .leading, spacing: 4) {
                            Text(verbatim: selection.path).font(.headline).textSelection(.enabled)
                            Text(capture.evidenceLabel).font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        Button("Done") { dismiss() }.keyboardShortcut(.cancelAction)
                    }.padding(12).background(.bar)
                    AtlasPagedSourceView(model: capture.reader, canKeep: canKeep(capture.reader), keep: keep)
                }.frame(minWidth: 800, idealWidth: 1040, minHeight: 620, idealHeight: 760)
            }
        }
        .sheet(isPresented: $showsDesk) { AtlasReadingDeskView(model: desk) }
        .onDisappear { selection.close() }
    }

    private func canKeep(_ reader: AtlasPagedReaderModel) -> Bool {
        desk.canOperate && reader.canKeep && desk.readerDomain != nil
            && reader.retainedRegistryDomain == desk.readerDomain
    }
    private func keep(_ reader: AtlasPagedReaderModel) {
        guard canKeep(reader) else { return }
        desk.keep(reader: reader)
        showsDesk = true
    }
}
