import SwiftUI
import AppKit

/// Nonspatial reading of the shared engine's retained byte/line windows. The
/// enclosing app gives each file activation a fresh view identity and revokes
/// it when project/search origin changes. No byte coordinates are inferred from
/// AppKit's page-local text selection; original-byte copy is explicitly hex.
@MainActor struct AtlasPagedSourceView: View {
    let root: String
    let path: String
    let accessLease: AnyObject?
    private let ownsModel: Bool
    private let pin: ((AtlasPagedReaderModel) -> Void)?
    private let canPin: Bool
    private let keep: ((AtlasPagedReaderModel) -> Void)?
    private let canKeep: Bool
    @State private var model: AtlasPagedReaderModel
    @State private var copyNotice = ""
    @State private var showsOutline = false
    @State private var showsDocument = false
    @State private var showsSplitPreview = false

    init(root: String, path: String, accessLease: AnyObject?, captured: AtlasSearchCapturedHit? = nil,
         supplied: AtlasSuppliedReaderSource? = nil,
         canPin: Bool = false, pin: ((AtlasPagedReaderModel) -> Void)? = nil,
         canKeep: Bool = false, keep: ((AtlasPagedReaderModel) -> Void)? = nil) {
        self.root = root
        self.path = path
        self.accessLease = accessLease
        ownsModel = true
        self.canPin = canPin
        self.pin = pin
        self.canKeep = canKeep
        self.keep = keep
        _model = State(initialValue: AtlasPagedReaderModel(transport: .native, search: .native,
            outline: .native, document: .native, captured: captured, supplied: supplied))
    }

    /// A pin is already open and owned by the enclosing native workspace.
    /// Temporary tab/view disappearance neither reopens nor closes its capture.
    init(model: AtlasPagedReaderModel, canPin: Bool = false, pin: ((AtlasPagedReaderModel) -> Void)? = nil,
         canKeep: Bool = false,
         keep: ((AtlasPagedReaderModel) -> Void)? = nil) {
        root = ""; path = ""; accessLease = nil
        ownsModel = false; self.canPin = canPin; self.pin = pin
        self.canKeep = canKeep; self.keep = keep
        _model = State(initialValue: model)
    }

    var body: some View {
        VStack(spacing: 4) {
            controls
            findControls
            HSplitView {
                // This child retains its structural identity when the preview
                // is inserted or removed: toggling does not reopen the source.
                sourcePane.frame(minWidth: 380, maxWidth: .infinity, maxHeight: .infinity)
                if showsSplitPreview {
                    AtlasDocumentPreviewView(model: model.documentPreview, embedded: true,
                        openSource: openPreviewSource, closePreview: { showsSplitPreview = false })
                }
            }
        }
        .sheet(isPresented: $showsDocument) {
            AtlasDocumentPreviewView(model: model.documentPreview, openSource: openPreviewSource)
        }
        .onAppear { if ownsModel { open() } }
        .onDisappear { if ownsModel { model.close() }; copyNotice = "" }
        .onChange(of: model.navigation) { _, _ in copyNotice = "" }
        .onChange(of: Array(model.fileQuery.utf8)) { _, _ in copyNotice = "" }
    }

    private var sourcePane: some View {
        VStack(spacing: 4) {
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
                    if let line = page.firstPhysicalLine { Text("from line \(line)").font(.caption) }
                    Spacer()
                    Button("Copy original hex") { copyHex() }
                        .help("Copy this page's original captured bytes as lossless hexadecimal, not decoded text or a search selection")
                        .accessibilityLabel("Copy original page bytes as hexadecimal")
                }
                .padding(.horizontal, 10)
            }
            if let source = model.source {
                // Equal page text can have different lexical context. Retain
                // the accepted page/source pair for this presentation update.
                let page = model.page
                AtlasSourceReader(source: source, navigation: model.navigation,
                    selection: model.nativeSelectionRange.map { AtlasReaderSelection(source: source, range: $0) },
                    backgroundColor: Monokai.background, foregroundColor: Monokai.color("plain")) {
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
                    Button { keep(model) } label: { Label("Keep in reading desk", systemImage: "books.vertical") }
                        .disabled(!canKeep || !model.canPin)
                        .help("Keep the entire exact source capture at this page, with bookmarks, cross-file history and explicit checkpoint saving")
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
                Spacer()
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
                TextField("Find in retained file…", text: $model.fileQuery)
                    .frame(width: 190)
                    .textFieldStyle(.roundedBorder).onSubmit(model.find)
                    .accessibilityLabel("Exact text to find in this retained file")
                Button("Find", action: model.find).disabled(!model.canFind || model.fileQuery.isEmpty)
                Button("Outline") { showsOutline = true }
                    .disabled(!model.canBuildOutline)
                    .popover(isPresented: $showsOutline) { AtlasOutlineBrowser(model: model) }
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
    private func copyHex() {
        guard let page = model.page else { return }
        NSPasteboard.general.clearContents()
        copyNotice = NSPasteboard.general.setString(page.originalHex, forType: .string)
            ? "Copied \(page.end - page.start) original bytes as hex."
            : "The clipboard did not accept this page's original hex."
    }
}
