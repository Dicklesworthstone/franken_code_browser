import SwiftUI

/// Full retained-reader tools for a captured or restored desk source. Pinning
/// forks these exact bytes; the two pins can be compared using the existing
/// comparison sheet. Neither reader appearance nor a pin opens a disk label.
@MainActor struct AtlasDeskReaderView: View {
    @Bindable var desk: AtlasReadingDeskModel
    let capture: AtlasDeskReaderCapture
    @Bindable var pins: AtlasPinnedReaders<AtlasPagedReaderModel>
    @Environment(\.dismiss) private var dismiss
    @State private var updateNotice = ""

    private var pinGroup: String { "reading-desk:\(capture.owner)" }
    private var keepLabel: String {
        capture.reader.matchHex != nil || capture.reader.symbolHex != nil
            ? "Keep selected result in desk" : "Keep current page in desk"
    }

    var body: some View {
        VStack(spacing: 8) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 3) {
                    Text(capture.identity.displayPath).font(.headline).lineLimit(2).textSelection(.enabled)
                    if let range = capture.pane.location.selection {
                        Text("Saved desk selection: original bytes \(range.start)..<\(range.end)")
                            .font(.caption).foregroundStyle(.secondary)
                    } else {
                        Text("Saved desk location: original byte \(capture.pane.location.offset)")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
                Spacer()
                Button("Return to saved location") {
                    capture.reader.offsetInput = String(capture.pane.location.offset)
                    capture.reader.goToByte()
                }.disabled(capture.reader.busy)
                Button(keepLabel) {
                    if capture.keepCurrentPage(in: desk) {
                        updateNotice = "Updating the original desk location. Save a new checkpoint when you want to preserve it after closing the app."
                    }
                }
                .disabled(!capture.canUpdate(desk))
                .help("Store this reader's exact Find or Outline result, or its full page range, in the original retained pane. Native text selection is not interpreted as original byte offsets.")
                Button("Done") { dismiss() }.keyboardShortcut(.cancelAction)
            }
            .controlSize(.small).padding(.horizontal, 12).padding(.top, 12)
            HSplitView {
                AtlasPagedSourceView(model: capture.reader, canPin: pins.canAdd, pin: { reader in
                    pins.pin(reader, root: pinGroup, label: capture.identity.displayPath)
                }).frame(minWidth: 420, maxWidth: .infinity, maxHeight: .infinity)
                if !pins.entries.isEmpty || pins.busy {
                    AtlasPinnedReadersView(readers: pins).frame(minWidth: 350, idealWidth: 480)
                }
            }
            HStack {
                Text(updateNotice.isEmpty ? "The desk keeps its original selection until you explicitly keep a new reader page or result." : updateNotice)
                    .font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                Spacer()
                if desk.busy { ProgressView().controlSize(.small) }
            }.padding(.horizontal, 12).padding(.bottom, 8)
        }
        .frame(minWidth: 980, idealWidth: 1280, minHeight: 680, idealHeight: 820)
        .onAppear { pins.bind(root: pinGroup) }
        .onChange(of: desk.busy) { _, busy in
            if !busy && !updateNotice.isEmpty { updateNotice = desk.notice }
        }
        .onDisappear { capture.reader.close() }
    }
}
