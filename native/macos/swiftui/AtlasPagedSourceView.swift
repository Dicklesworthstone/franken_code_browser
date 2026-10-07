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
    @State private var model: AtlasPagedReaderModel
    @State private var copyNotice = ""
    @State private var showsOutline = false
    @State private var showsDocument = false

    init(root: String, path: String, accessLease: AnyObject?, captured: AtlasSearchCapturedHit? = nil) {
        self.root = root
        self.path = path
        self.accessLease = accessLease
        _model = State(initialValue: AtlasPagedReaderModel(transport: .native, search: .native,
            outline: .native, document: .native, captured: captured))
    }

    var body: some View {
        VStack(spacing: 4) {
            controls
            findControls
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
                AtlasSourceReader(source: source, navigation: model.navigation,
                    selection: model.nativeSelectionRange.map { AtlasReaderSelection(source: source, range: $0) }) {
                    NSAttributedString(string: source.text, attributes: [
                        .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular),
                        .foregroundColor: NSColor.textColor
                    ])
                }
            } else {
                VStack(spacing: 8) {
                    Text(model.busy ? "Opening retained source…" : "Retained source unavailable")
                    if !model.busy { Button("Retry opening", action: open) }
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
        .sheet(isPresented: $showsDocument) {
            AtlasDocumentPreviewView(model: model.documentPreview)
        }
        .onAppear(perform: open)
        .onDisappear { model.close(); copyNotice = "" }
        .onChange(of: model.navigation) { _, _ in copyNotice = "" }
        .onChange(of: Array(model.fileQuery.utf8)) { _, _ in copyNotice = "" }
    }

    private var controls: some View {
        HStack(spacing: 6) {
            Button("Back", action: model.back).disabled(!model.canGoBack)
                .help("Return to the previous retained byte window")
            Button("Next", action: model.next).disabled(!model.canGoNext)
                .help("Continue after this page without reopening the file")
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
            if model.busy {
                ProgressView().controlSize(.small).accessibilityLabel("Reading source page")
                Button("Cancel", action: model.cancel)
            }
        }
        .controlSize(.small)
        .padding(.horizontal, 10).padding(.top, 4)
    }

    private var findControls: some View {
        HStack(spacing: 6) {
            TextField("Find in retained file…", text: $model.fileQuery)
                .textFieldStyle(.roundedBorder).onSubmit(model.find)
                .accessibilityLabel("Exact text to find in this retained file")
            Button("Find", action: model.find).disabled(!model.canFind || model.fileQuery.isEmpty)
            Button("Outline") { showsOutline = true }
                .disabled(!model.canBuildOutline)
                .popover(isPresented: $showsOutline) { AtlasOutlineBrowser(model: model) }
            Button("Markdown") {
                showsDocument = true
                if !model.documentPreview.authorized { model.documentPreview.prepare() }
            }
            .disabled(model.page == nil || model.busy)
            .help("Preview the same retained capture with headings and source mapping; UTF-8 sources up to 64 KiB")
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
