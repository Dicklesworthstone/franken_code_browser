import SwiftUI
import AppKit

/// Nonspatial reading of the shared engine's retained byte/line windows. The
/// enclosing app gives each file activation a fresh view identity and revokes
/// it when project/search origin changes. Native selections are resolved through
/// the retained Rust source map before exact copying or Reading Desk actions.
@MainActor struct AtlasPagedSourceView: View {
    let root: String
    let path: String
    let accessLease: AnyObject?
    private let ownsModel: Bool
    private let pin: ((AtlasPagedReaderModel) -> Void)?
    private let canPin: Bool
    private let keep: ((AtlasPagedReaderModel) -> Void)?
    private let canKeep: Bool
    private let openLocalLink: AtlasDocumentPreviewModel.LocalNavigation?
    private let project: AtlasProjectBrowserModel?
    @State private var model: AtlasPagedReaderModel
    @State private var readerCommands = AtlasReaderCommands()
    @State private var copyNotice = ""
    @State private var showsOutline = false
    @State private var showsReferences = false
    @State private var showsDocument = false
    @State private var showsSplitPreview = false
    @State private var wrapsLines = false
    @State private var showsLineNumbers = true
    @State private var showsWhitespace = false
    @State private var linkHistory = AtlasDocumentLinkHistory<AtlasProjectBrowserCapture>(
        retire: { $0.reader.close() },
        failureMessage: { ($0 as? AtlasProjectBrowserError)?.message
            ?? ($0 as? AtlasReaderError)?.message ?? "The linked file could not be opened." })
    @State private var showsLinked = false

    init(root: String, path: String, accessLease: AnyObject?, captured: AtlasSearchCapturedHit? = nil,
         supplied: AtlasSuppliedReaderSource? = nil,
         canPin: Bool = false, pin: ((AtlasPagedReaderModel) -> Void)? = nil,
         canKeep: Bool = false, keep: ((AtlasPagedReaderModel) -> Void)? = nil,
         openLocalLink: AtlasDocumentPreviewModel.LocalNavigation? = nil,
         project: AtlasProjectBrowserModel? = nil) {
        self.root = root
        self.path = path
        self.accessLease = accessLease
        ownsModel = true
        self.canPin = canPin
        self.pin = pin
        self.canKeep = canKeep
        self.keep = keep
        self.openLocalLink = openLocalLink
        self.project = project
        _model = State(initialValue: AtlasPagedReaderModel(transport: .native, search: .native,
            outline: .native, document: .native, captured: captured, supplied: supplied))
    }

    /// A pin is already open and owned by the enclosing native workspace.
    /// Temporary tab/view disappearance neither reopens nor closes its capture.
    init(model: AtlasPagedReaderModel, canPin: Bool = false, pin: ((AtlasPagedReaderModel) -> Void)? = nil,
         canKeep: Bool = false,
         keep: ((AtlasPagedReaderModel) -> Void)? = nil,
         openLocalLink: AtlasDocumentPreviewModel.LocalNavigation? = nil,
         project: AtlasProjectBrowserModel? = nil) {
        root = ""; path = ""; accessLease = nil
        ownsModel = false; self.canPin = canPin; self.pin = pin
        self.canKeep = canKeep; self.keep = keep
        self.openLocalLink = openLocalLink
        self.project = project
        _model = State(initialValue: model)
    }

    var body: some View {
        VStack(spacing: 4) {
            controls
            findControls
            if !linkHistory.notice.isEmpty {
                HStack {
                    Text(linkHistory.notice).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                    Spacer()
                    if linkHistory.busy {
                        ProgressView().controlSize(.small)
                        Button("Cancel link", action: cancelLink)
                    }
                }.padding(.horizontal, 10)
            }
            HSplitView {
                // This child retains its structural identity when the preview
                // is inserted or removed: toggling does not reopen the source.
                sourcePane.frame(minWidth: 380, maxWidth: .infinity, maxHeight: .infinity)
                if showsSplitPreview {
                    AtlasDocumentPreviewView(model: model.documentPreview, embedded: true,
                        openSource: openPreviewSource, openLocalLink: localNavigation, closePreview: { showsSplitPreview = false })
                }
            }
        }
        .sheet(isPresented: $showsDocument) {
            AtlasDocumentPreviewView(model: model.documentPreview, openSource: openPreviewSource, openLocalLink: modalLocalNavigation)
        }
        .sheet(isPresented: $showsLinked, onDismiss: closeLinked) { linkedReader }
        .onAppear { if ownsModel { open() } }
        .onDisappear { closeLinked(); if ownsModel { model.close() }; copyNotice = "" }
        .onChange(of: project?.info?.owner) { _, _ in projectChanged() }
        .onChange(of: project?.rootAccessIdentity) { _, _ in projectChanged() }
        .onChange(of: project?.closed) { _, _ in projectChanged() }
        .onChange(of: linkHistory.current?.id) { _, current in
            if current != nil { showsLinked = true }
        }
        .onChange(of: model.navigation) { _, _ in copyNotice = "" }
        .onChange(of: model.textSelection?.id) { _, _ in copyNotice = "" }
        .onChange(of: Array(model.fileQuery.utf8)) { _, _ in copyNotice = "" }
        .onChange(of: model.findOptions) { _, _ in copyNotice = "" }
        .onChange(of: Array(model.referenceName.utf8)) { _, _ in copyNotice = "" }
    }

    private var modalLocalNavigation: AtlasDocumentPreviewModel.LocalNavigation? {
        guard let openLocalLink = localNavigation else { return nil }
        return { target in
            // The preview invokes this after its worker drains. Dismiss the
            // overlay so the project host's linked reader/progress is visible;
            // accepted preview state stays available in the originating reader.
            showsDocument = false
            openLocalLink(target)
        }
    }

    private var localNavigation: AtlasDocumentPreviewModel.LocalNavigation? {
        openLocalLink ?? verifiedNavigation(from: model)
    }

    private func verifiedNavigation(from source: AtlasPagedReaderModel) -> AtlasDocumentPreviewModel.LocalNavigation? {
        guard project?.canOpenVerifiedDocumentLinks == true, !linkHistory.busy else { return nil }
        return { target in openLink(target, from: source) }
    }

    private func capturedNavigation(from source: AtlasProjectBrowserCapture) -> AtlasDocumentPreviewModel.LocalNavigation? {
        guard let project, project.canOperate, !linkHistory.busy,
              source.catalogOwner == project.info?.owner || project.canOpenVerifiedDocumentLinks else { return nil }
        return { target in openLink(target, from: source.reader, capture: source) }
    }

    /// Keep the bounded history's reader views mounted as well as their
    /// captures. This preserves native scroll/selection and preview state when
    /// Back/Forward changes the active file, without rebuilding its source.
    private var linkedReader: some View {
        VStack(spacing: 8) {
            HStack {
                Label("Linked source", systemImage: "link").font(.headline)
                Button("Back", action: linkHistory.back).disabled(!linkHistory.canBack)
                    .help("Return to the previous retained linked source without reading the file again")
                Button("Forward", action: linkHistory.forward).disabled(!linkHistory.canForward)
                    .help("Return to the next retained linked source without reading the file again")
                if linkHistory.current != nil {
                    Text("\(linkHistory.position + 1) of \(linkHistory.captures.count)")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Spacer()
                if linkHistory.busy {
                    ProgressView().controlSize(.small)
                    Button("Cancel link", action: cancelLink).disabled(linkHistory.canceling)
                }
                Button("Return to source", action: closeLinked)
                    .help("Close this link history and return to the originating source")
            }
            ZStack(alignment: .topLeading) {
                ForEach(linkHistory.captures) { capture in
                    linkedDestination(capture)
                        .opacity(linkHistory.current === capture ? 1 : 0)
                        .allowsHitTesting(linkHistory.current === capture)
                        .disabled(linkHistory.current !== capture)
                        .accessibilityHidden(linkHistory.current !== capture)
                }
            }
            Text("Back and Forward keep your place in up to three linked sources. Return to source restores the originating reader.")
                .font(.caption2).foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
            if !linkHistory.notice.isEmpty {
                Text(linkHistory.notice).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }.padding(14).frame(minWidth: 850, idealWidth: 1100, minHeight: 600, idealHeight: 760)
        .background(AtlasDocumentLinkFocus(capture: linkHistory.current?.id)
            .allowsHitTesting(false).accessibilityHidden(true))
    }

    private func linkedDestination(_ capture: AtlasProjectBrowserCapture) -> some View {
        VStack(spacing: 8) {
            Text(verbatim: capture.file.display).font(.caption).textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
            if capture.fragment.status != .none {
                Text(verbatim: capture.fragment.navigationDescription).font(.caption)
                    .foregroundStyle(capture.fragment.isFallback ? Color.orange : Color.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            AtlasPagedSourceView(model: capture.reader, canKeep: canKeep, keep: keep,
                openLocalLink: capturedNavigation(from: capture))
        }
    }

    private func openLink(_ target: AtlasReaderDocumentLinkTarget, from source: AtlasPagedReaderModel,
                          capture: AtlasProjectBrowserCapture? = nil) {
        guard let project, !linkHistory.busy, project.canOperate,
              capture?.catalogOwner == project.info?.owner || project.canOpenVerifiedDocumentLinks else { return }
        // An action retained by an older, hidden destination cannot branch the
        // currently displayed source. The engine separately verifies the exact
        // link/capture identities before returning a new reader.
        if let capture, linkHistory.current !== capture { return }
        linkHistory.bind(root: project.root, accessIdentity: project.rootAccessIdentity)
        linkHistory.follow {
            if let capture {
                return try await project.openRetainedDocumentLink(target, from: capture,
                    readerTransport: .native, search: .native, outline: .native, document: .native)
            }
            return try await project.openVerifiedDocumentLink(target, from: source,
                readerTransport: .native, search: .native, outline: .native, document: .native)
        }
    }

    private func cancelLink() { linkHistory.cancel() }

    private func closeLinked() {
        showsLinked = false
        linkHistory.close()
    }

    private func projectChanged() {
        // Catalog refresh withdraws only its pending link operation. Exact
        // captured history survives if raw root bytes and the actual native
        // grant are unchanged. Root/grant revocation closes the whole trail.
        guard let project, !project.closed, project.info != nil else {
            closeLinked()
            return
        }
        if linkHistory.bind(root: project.root, accessIdentity: project.rootAccessIdentity) {
            linkHistory.catalogRefreshed()
        } else {
            showsLinked = false
        }
    }

    private var sourcePane: some View {
        VStack(spacing: 4) {
            if let label = model.referenceSelectionLabel {
                HStack {
                    Text(label).font(.caption2).foregroundStyle(.secondary)
                    Spacer()
                    Button("Copy occurrence hex") {
                        guard let hex = model.referenceHex else { return }
                        NSPasteboard.general.clearContents()
                        copyNotice = NSPasteboard.general.setString(hex, forType: .string)
                            ? "Copied the occurrence's exact original bytes as hex." : "Clipboard write failed."
                    }
                    .help("Copy the selected occurrence's captured bytes, including its original encoding")
                }.padding(.horizontal, 10)
            }
            if let label = model.symbolSelectionLabel {
                HStack {
                    Text(label).font(.caption2).foregroundStyle(.secondary)
                    Spacer()
                    Button("Copy symbol hex") {
                        guard let hex = model.symbolHex else { return }
                        NSPasteboard.general.clearContents()
                        copyNotice = NSPasteboard.general.setString(hex, forType: .string)
                            ? "Copied the selected candidate's original bytes as hex." : "Clipboard write failed."
                    }
                }.padding(.horizontal, 10)
            }
            if model.findIsCurrent, let report = model.findReport {
                Text(report.summary).font(.caption2).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 10)
            }
            if let page = model.page {
                HStack {
                    Text("Bytes \(page.start)..<\(page.end) / \(page.identity.capturedBytes) · \(page.identity.encoding)")
                        .font(.system(size: 10.5, design: .monospaced))
                    if let line = physicalLineBase(page) {
                        Text("from line \(line)").font(.caption)
                    } else if showsLineNumbers && !page.text.isEmpty {
                        Text("Page lines (+)").font(.caption)
                            .help("The gutter counts lines within this page. Use Go line to request a known physical file line.")
                    }
                    Spacer()
                    Button("Copy original hex") { copyHex() }
                        .help("Copy this page's original captured bytes as lossless hexadecimal, not decoded text or a search selection")
                        .accessibilityLabel("Copy original page bytes as hexadecimal")
                }
                .padding(.horizontal, 10)
                if let position = page.position {
                    Text(position.summary)
                        .font(.caption).foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 10)
                        .accessibilityLabel("Exact logical source position: " + position.summary)
                }
            }
            if model.hasTextSelectionIntent {
                HStack(spacing: 10) {
                    if model.resolvingTextSelection { ProgressView().controlSize(.small) }
                    Text(model.textSelectionNotice)
                        .font(.caption2).foregroundStyle(.secondary)
                        .lineLimit(3).textSelection(.enabled)
                    Spacer(minLength: 4)
                    if model.textSelection != nil {
                        Menu("Copy selection") {
                            Button("Copy selected text") { copySelected(.text) }
                            Button("Copy text with location") { copySelected(.withLocation) }
                            Button("Copy original selection as hex") { copySelected(.originalHex) }
                        }
                        .disabled(model.busy || model.resolvingTextSelection)
                        .help("Copy this exact selected passage as logical text, text with capture provenance, or lossless original-byte hex. Line endings are preserved.")
                    } else if !model.resolvingTextSelection {
                        Button("Retry selection") { model.retryTextSelection() }
                            .disabled(model.busy)
                    }
                }.padding(.horizontal, 10).controlSize(.small)
            }
            if let source = model.source {
                // Equal page text can have different lexical context. Retain
                // the accepted page/source pair for this presentation update.
                let page = model.page
                AtlasSourceReader(source: source, navigation: model.navigation,
                    selection: model.nativeSelectionRange.map { AtlasReaderSelection(source: source, range: $0) },
                    wrapsLines: wrapsLines, showsLineNumbers: showsLineNumbers,
                    showsWhitespace: showsWhitespace, firstPhysicalLine: physicalLineBase(page),
                    usesSyntaxPalette: true, readerCommands: readerCommands,
                    onSelectionChange: { source, navigation, range in
                        copyNotice = ""
                        model.updateTextSelection(source: source, navigation: navigation, range: range)
                    }) {
                    if let page, page.text.utf8.elementsEqual(source.text.utf8),
                       let highlight = page.highlight {
                        return AtlasDocument.style(text: source.text,
                            runs: highlight.runs.map { ($0.range, $0.role.rawValue) })
                    }
                    return AtlasDocument.style(text: source.text, runs: [])
                }
            } else {
                VStack(spacing: 8) {
                    Text(model.busy ? "Opening retained source…" : "Retained source unavailable")
                    if !model.busy && ownsModel { Button("Retry opening", action: open) }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            VStack(alignment: .leading, spacing: 2) {
                if !model.notice.isEmpty { Text(model.notice) }
                if !copyNotice.isEmpty { Text(copyNotice) }
                Text(model.coverage)
            }
            .font(.caption2).foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 10).padding(.bottom, 4)
        }
    }

    private func physicalLineBase(_ page: AtlasReaderPage?) -> UInt64? {
        guard let page, !page.text.isEmpty else { return nil }
        if let line = page.firstPhysicalLine, line > 0 { return line }
        return page.start == 0 ? 1 : nil
    }

    private var controls: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 6) {
                if let pin {
                    Button { pin(model) } label: { Label("Pin source", systemImage: "pin") }
                        .disabled(!canPin || !model.canPin)
                        .keyboardShortcut("p", modifiers: [.command, .shift])
                        .help("Keep this exact retained source beside other files (⇧⌘P); pinning never rereads the disk file")
                }
                if let keep {
                    Button { keep(model) } label: {
                        Label(model.hasTextSelectionIntent ? "Keep selected text" : "Keep in reading desk", systemImage: "books.vertical")
                    }
                        .disabled(!canKeep || !model.canKeep)
                        .help("Keep this exact selected range or page, retaining its complete source up to 4 MiB. Add bookmarks or personal notes and save a checkpoint in the Reading Desk.")
                }
                Button("Back", action: model.back).disabled(!model.canGoBack)
                    .help("Return to the previous retained byte window")
                Button("Forward", action: model.forward).disabled(!model.canGoForward)
                    .help("Revisit the retained window left by Back; this does not restore an expired search selection")
                Button("Next page", action: model.next).disabled(!model.canGoNext)
                    .help("Continue after this page without reopening the file; distinct from navigation history")
                TextField("Byte offset", text: $model.offsetInput)
                    .frame(width: 88).textFieldStyle(.roundedBorder)
                    .accessibilityLabel("Original byte offset, zero based")
                    .onSubmit(model.goToByte)
                Button("Go byte", action: model.goToByte).disabled(model.page == nil)
                TextField("Line", text: $model.lineInput)
                    .frame(width: 64).textFieldStyle(.roundedBorder)
                    .accessibilityLabel("Physical source line, one based")
                    .onSubmit(model.goToLine)
                Button("Go line", action: model.goToLine).disabled(model.page == nil)
                TextField("Column", text: $model.columnInput)
                    .frame(width: 68).textFieldStyle(.roundedBorder)
                    .accessibilityLabel("Logical source column, one based")
                    .onSubmit(model.goToPosition)
                    .disabled(!model.canGoToPosition)
                Picker("Column unit", selection: $model.columnKind) {
                    ForEach(AtlasReaderColumnKind.allCases) { Text($0.label).tag($0) }
                }
                .labelsHidden().frame(width: 170)
                .accessibilityLabel("Logical column unit")
                .disabled(!model.canGoToPosition)
                Button("Go location", action: model.goToPosition)
                    .disabled(!model.canGoToPosition)
                    .help("Jump to the one-based line and column in this retained source. Columns use the selected logical unit; tabs are not expanded and visual order is not inferred.")
                Menu("Display") {
                    Toggle("Line numbers", isOn: $showsLineNumbers)
                    Toggle("Visible whitespace", isOn: $showsWhitespace)
                    Toggle("Wrap lines", isOn: $wrapsLines)
                }
                .disabled(model.source == nil)
                .help("Show line numbers or whitespace, or wrap long lines. These display choices preserve the captured source and selection.")
                Spacer()
                AtlasEditorActionView(source: { model.externalEditorSource() })
                Button("Refresh from disk", action: model.refreshFromDisk)
                    .disabled(!model.canRefresh)
                    .help("Explicitly recapture this project file. Success starts a new capture at the first page and clears old exact targets/history; failure keeps the retained source.")
                    .accessibilityLabel("Refresh retained source from the current disk file")
                if model.busy {
                    ProgressView().controlSize(.small)
                        .accessibilityLabel(model.isRefreshing ? "Refreshing source capture" : "Reading source page")
                    Button("Cancel", action: model.cancel)
                }
            }
            .controlSize(.small)
            .padding(.horizontal, 10).padding(.top, 4)
        }.frame(height: 36)
    }

    private var findControls: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 6) {
                AtlasReaderFindField(text: $model.fileQuery, commands: readerCommands,
                    active: !showsOutline && !showsReferences && !showsDocument && !showsLinked,
                    canMove: { [model] in model.canMoveHit },
                    move: { [model] in model.moveHit(backwards: $0) }, submit: model.find)
                    .frame(width: 190)
                    .help("Find in this retained file (⌘F). Move between accepted matches with ⌘G and ⇧⌘G.")
                Button("Find", action: model.find).disabled(!model.canFind || model.fileQuery.isEmpty)
                Toggle("Match case", isOn: $model.findOptions.caseSensitive)
                    .disabled(!model.canConfigureFind)
                    .help("When off, compare Unicode lowercase spellings. Original source characters and bytes remain unchanged.")
                Toggle("Count all matches", isOn: $model.findOptions.countAllMatches)
                    .disabled(!model.canConfigureFind)
                    .help("Continue scanning the retained file after navigation rows fill; completeness and retained rows are reported separately.")
                Button("Outline") { showsOutline = true }
                    .disabled(!model.canBuildOutline)
                    .popover(isPresented: $showsOutline) {
                        AtlasOutlineBrowser(model: model, findOccurrences: { target in
                            model.findReferences(for: target)
                            showsOutline = false
                            showsReferences = true
                        })
                    }
                Button("Occurrences") { showsReferences = true }
                    .disabled(model.page == nil)
                    .help("Find whole-identifier text occurrences in the retained source, including comments and literals")
                    .popover(isPresented: $showsReferences) {
                        AtlasReferenceBrowser(model: model, openOccurrence: { showsReferences = false })
                    }
                Button("Markdown") {
                    showsDocument = true
                    preparePreview()
                }
                .disabled(model.page == nil || model.busy || showsSplitPreview)
                .help("Preview the same retained capture with headings and source mapping; UTF-8 sources up to 64 KiB")
                Button(showsSplitPreview ? "Hide preview" : "Split preview") {
                    showsSplitPreview.toggle()
                    if showsSplitPreview { preparePreview() }
                    else { model.documentPreview.cancel(silent: true) }
                }
                .disabled(model.page == nil || model.busy || showsDocument)
                .help("Read source and Markdown side by side from one retained capture; each pane scrolls independently")
                Button("Previous hit") { model.moveHit(backwards: true) }.disabled(!model.canMoveHit)
                Button("Next hit") { model.moveHit(backwards: false) }.disabled(!model.canMoveHit)
                if let position = model.findPositionLabel {
                    Text(position).font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                        .accessibilityLabel(position)
                }
                if model.matchHex != nil {
                    Button("Copy hit hex") {
                        guard let hex = model.matchHex else { return }
                        NSPasteboard.general.clearContents()
                        copyNotice = NSPasteboard.general.setString(hex, forType: .string)
                            ? "Copied the matched original bytes as hex." : "Clipboard write failed."
                    }
                    .help("Copy the verified retained hit's original bytes, independently of native glyph selection")
                }
            }
            .controlSize(.small).padding(.horizontal, 10)
        }.frame(height: 32)
    }

    private func preparePreview() {
        if !model.documentPreview.authorized { model.documentPreview.prepare() }
    }
    private func openPreviewSource(_ target: AtlasReaderDocumentTarget) {
        // Resolve against both current models at activation, not a captured
        // filename or cached byte offset. The existing reader history/worker
        // owns the transaction, cancellation and source admission.
        guard !model.busy, let identity = model.page?.identity,
              let offset = model.documentPreview.sourceOffset(for: target, reader: identity) else { return }
        model.offsetInput = String(offset)
        model.goToByte()
        showsDocument = false
    }
    private func open() {
        copyNotice = ""
        model.open(root: root, path: path, accessLease: accessLease)
    }
    private enum SelectionCopyFormat { case text, withLocation, originalHex }

    private func copySelected(_ format: SelectionCopyFormat) {
        guard !model.busy, !model.resolvingTextSelection,
              let selected = model.textSelection else { return }
        let pasteboard = NSPasteboard.general
        let expectedChange = pasteboard.changeCount
        let value: String
        let success: String
        switch format {
        case .text:
            value = selected.text
            success = selected.containsReplacements
                ? "Copied selected logical text with replacement characters; original line endings are preserved."
                : "Copied selected logical text with original line endings."
        case .withLocation:
            value = selected.withLocationText
            success = "Copied selected logical text with its captured source identity, original byte range and decoding policy."
        case .originalHex:
            value = selected.originalHex
            success = "Copied the selection's exact original bytes as lossless hexadecimal."
        }
        // Resolution is already complete. No async source work or native
        // clipboard mutation occurs while waiting for a source-map response.
        guard model.textSelection?.id == selected.id else { return }
        switch AtlasReaderSelectionClipboard.publish(value, to: pasteboard, expectedChangeCount: expectedChange) {
        case .copied: copyNotice = success
        case .tooLarge:
            copyNotice = "The selection exceeds the 2 MiB clipboard staging budget. Select a smaller passage; nothing was copied."
        case .changed:
            copyNotice = "The clipboard changed before publication. Its newer contents were preserved; copy again to replace them."
        case .failed:
            copyNotice = "Clipboard publication failed. Its contents may have changed; no rollback was attempted."
        }
    }

    private func copyHex() {
        guard let page = model.page else { return }
        NSPasteboard.general.clearContents()
        copyNotice = NSPasteboard.general.setString(page.originalHex, forType: .string)
            ? "Copied \(page.end - page.start) original bytes as hex."
            : "The clipboard did not accept this page's original hex."
    }
}

/// Retained invisible text views must not keep the native first responder:
/// otherwise Copy or typing could target an older source after Back/Forward.
/// Resolve this sheet's own window, never an application-global key window.
@MainActor struct AtlasDocumentLinkFocus: NSViewRepresentable {
    let capture: UUID?
    final class Coordinator { var capture: UUID? }
    func makeCoordinator() -> Coordinator { Coordinator() }
    func makeNSView(context: Context) -> NSView { NSView() }
    func updateNSView(_ view: NSView, context: Context) {
        guard context.coordinator.capture != capture else { return }
        context.coordinator.capture = capture
        view.window?.makeFirstResponder(nil)
    }
}
