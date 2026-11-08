import SwiftUI
import AppKit

/// A separate live-range surface. Opening it never replaces an exact retained
/// reader or imports an old search, bookmark, outline or saved-archive anchor.
@MainActor struct AtlasObservedReaderView: View {
    let presentation: AtlasObservedReaderPresentation
    private var origin: AtlasObservedReaderSource { presentation.source }
    @State private var model: AtlasObservedReaderModel
    @State private var wrapsLines = false
    @State private var showsLineNumbers = true
    @State private var showsWhitespace = false
    @State private var copyNotice = ""
    @Environment(\.dismiss) private var dismiss

    init(presentation: AtlasObservedReaderPresentation) {
        self.presentation = presentation
        _model = State(initialValue: presentation.model)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Label("Observed Range Reader", systemImage: "doc.text.viewfinder").font(.headline)
                Spacer()
                Button("Reopen current file", action: model.reopenCurrentFile).disabled(!model.canReopen)
                    .help("Open the current pathname as a new descriptor and observe its first range. Keep the accepted observation if opening fails.")
                Button("Done") { presentation.close(); dismiss() }.keyboardShortcut(.cancelAction)
            }
            Text(verbatim: model.info?.identity.displayPath ?? String(reflecting: origin.input.displayPath))
                .font(.system(.caption, design: .monospaced))
                .textSelection(.enabled).lineLimit(2)
            Text(verbatim: "Project: " + String(reflecting: origin.input.root)).font(.caption).foregroundStyle(.secondary)
                .textSelection(.enabled).lineLimit(1).truncationMode(.middle)
            Text("Observe part of a current file, including files larger than the complete reader's 32 MiB limit. Each read is independent; the original complete reader stays unchanged.")
                .font(.caption).foregroundStyle(.secondary)
            controls
            Divider()
            if let page = model.page {
                let observation = page.observation
                HStack(alignment: .firstTextBaseline) {
                    VStack(alignment: .leading, spacing: 3) {
                        Text("Observation \(observation.id) · original bytes \(page.visibleRange.start)..<\(page.visibleRange.end)")
                            .font(.system(.caption, design: .monospaced))
                        Text("File length observed before read: \(observation.observedLength) bytes · \(observation.request.encoding.label)")
                            .font(.caption).foregroundStyle(.secondary)
                        if let finalLength = observation.finalObservedLength, finalLength != observation.observedLength {
                            Text("Length after read: \(finalLength) bytes; the file changed during this observation.")
                                .font(.caption).foregroundStyle(.orange)
                        }
                    }
                    Spacer()
                    Button("Copy original hex", action: copyOriginalHex).disabled(!model.canCopy)
                        .help("Copy the exact original bytes of this retained observation, not a new file read or the decoded selection")
                }
                if let source = model.source {
                    AtlasSourceReader(source: source, navigation: model.navigation, selection: nil,
                        wrapsLines: wrapsLines, showsLineNumbers: showsLineNumbers,
                        showsWhitespace: showsWhitespace, firstPhysicalLine: page.firstPhysicalLine,
                        backgroundColor: Monokai.background, foregroundColor: Monokai.color("plain")) {
                            AtlasDocument.style(text: source.text, runs: [])
                        }
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            } else {
                ContentUnavailableView(model.busy ? "Observing the opened file…" : "No accepted observation",
                    systemImage: "doc.text.viewfinder",
                    description: Text("Opening reads metadata first. Only the requested range and bounded decoding context are loaded."))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            Divider()
            VStack(alignment: .leading, spacing: 4) {
                Text(model.notice).textSelection(.enabled)
                if !copyNotice.isEmpty { Text(copyNotice) }
                Text(model.coverage)
                Text("Selected text copies the displayed decoding. Original hex copies exact bytes. At most eight observations are retained; an evicted range is unavailable until you explicitly observe it again.")
            }
            .font(.caption).foregroundStyle(.secondary)
        }
        .padding(14).frame(minWidth: 880, idealWidth: 1120, minHeight: 600, idealHeight: 760)
        .onAppear { presentation.start() }
        .onDisappear { presentation.close() }
        .onChange(of: model.navigation) { _, _ in copyNotice = "" }
    }

    private var controls: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 7) {
                Button("Back", action: model.back).disabled(!model.canBack)
                    .help("Restore an earlier retained observation without rereading the file")
                Button("Forward", action: model.forward).disabled(!model.canForward)
                    .help("Restore a later retained observation without rereading the file")
                Divider().frame(height: 20)
                Button("Previous bytes", action: model.previousRange).disabled(!model.canPrevious)
                    .help("Read a new observation before this byte range")
                Button("Next bytes", action: model.nextRange).disabled(!model.canNext)
                    .help("Read a new observation after this byte range")
                Button("Observe again", action: model.observeAgain).disabled(model.page == nil || model.busy)
                    .help("Explicitly read this byte location again from the same opened file object")
                Spacer()
                Menu("Display") {
                    Toggle("Line numbers", isOn: $showsLineNumbers)
                    Toggle("Visible whitespace", isOn: $showsWhitespace)
                    Toggle("Wrap lines", isOn: $wrapsLines)
                }
                .menuStyle(.borderlessButton).fixedSize()
            }
            HStack(spacing: 7) {
                Text("Byte offset").font(.caption)
                TextField("0", text: $model.offsetInput).frame(width: 150)
                    .textFieldStyle(.roundedBorder).onSubmit(model.observeInput)
                    .accessibilityLabel("Absolute original-byte offset")
                Picker("Decode", selection: $model.encoding) {
                    ForEach(AtlasObservedReaderEncoding.allCases) { Text($0.label).tag($0) }
                }.frame(width: 165)
                Picker("Range", selection: $model.pageBytes) {
                    Text("16 KiB").tag(UInt64(16 * 1024))
                    Text("64 KiB").tag(UInt64(64 * 1024))
                    Text("256 KiB").tag(UInt64(256 * 1024))
                }.frame(width: 150)
                Button("Observe", action: model.observeInput).disabled(!model.canObserve)
                    .help("Read a new bounded observation. A newer request cancels an obsolete pending request.")
                Spacer()
                if model.busy {
                    ProgressView().controlSize(.small)
                    Button("Cancel", action: model.cancel)
                }
            }
            Text("Decoding is explicit for the next observation. It does not determine the encoding of unknown file regions.")
                .font(.caption2).foregroundStyle(.secondary)
        }
    }

    private func copyOriginalHex() {
        let navigation = model.navigation
        copyNotice = ""
        Task {
            guard let hex = await model.copyOriginalHex(), !model.closed,
                  model.navigation == navigation else { return }
            NSPasteboard.general.clearContents()
            copyNotice = NSPasteboard.general.setString(hex, forType: .string)
                ? "Copied this observation's original bytes as hexadecimal." : "Clipboard write failed."
        }
    }
}
