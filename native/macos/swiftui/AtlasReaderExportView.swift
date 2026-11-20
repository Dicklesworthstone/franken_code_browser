import SwiftUI
import AppKit

/// Explicit destination and original-byte range review. The selected source
/// is frozen before the panel's nested event loop; it is checked again before
/// a lifetime-only retained-session handoff to the export worker.
@MainActor struct AtlasReaderExportView: View {
    @Bindable var reader: AtlasPagedReaderModel
    @Bindable var exporter: AtlasReaderExportModel
    let target: AtlasReaderExportTarget
    @Environment(\.dismiss) private var dismiss
    @State private var startInput: String
    @State private var endInput: String
    @State private var panelNotice = ""

    init(reader: AtlasPagedReaderModel, exporter: AtlasReaderExportModel, target: AtlasReaderExportTarget) {
        self.reader = reader; self.exporter = exporter; self.target = target
        _startInput = State(initialValue: String(target.start))
        _endInput = State(initialValue: String(target.end))
    }

    private var currentTarget: AtlasReaderExportTarget? {
        guard reader.acceptsOriginalExport(target),
              let start = number(startInput), let end = number(endInput) else { return nil }
        let origin = start == target.start && end == target.end ? target.origin : .byteRange
        return reader.originalExportTarget(origin, start: start, end: end)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Label("Export original source bytes", systemImage: "square.and.arrow.up").font(.title2)
                Spacer()
                Button("Done") { dismiss() }.keyboardShortcut(.cancelAction).disabled(exporter.busy)
            }
            Text(verbatim: target.identity.displayPath).font(.headline).textSelection(.enabled).lineLimit(3)
            Text("Retained capture · \(target.identity.capturedBytes) bytes")
                .font(.caption).foregroundStyle(.secondary)
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Start byte").font(.caption)
                    TextField("0", text: $startInput).textFieldStyle(.roundedBorder).frame(width: 150)
                        .accessibilityLabel("Export original start byte, inclusive, zero based")
                }
                VStack(alignment: .leading, spacing: 4) {
                    Text("End byte, exclusive").font(.caption)
                    TextField(String(target.identity.capturedBytes), text: $endInput)
                        .textFieldStyle(.roundedBorder).frame(width: 150)
                        .accessibilityLabel("Export original end byte, exclusive")
                }
                Button("Whole capture") {
                    startInput = "0"; endInput = String(target.identity.capturedBytes)
                }
                Button("Original selection") {
                    startInput = String(target.start); endInput = String(target.end)
                }
            }.disabled(exporter.busy)
            if let currentTarget {
                Text("\(currentTarget.byteCount) bytes will be exported. Ranges may span multiple reader pages.")
                    .font(.callout).foregroundStyle(.secondary)
            } else {
                Text(reader.acceptsOriginalExport(target)
                    ? "Enter whole byte offsets with 0 ≤ start ≤ end ≤ \(target.identity.capturedBytes)."
                    : "The reader or selection changed. Close this panel and choose Export again for the current source.")
                    .font(.callout).foregroundStyle(.orange)
            }
            Text("The file preserves the capture's original encoding, line endings, BOM and malformed bytes. Choose a new filename; existing files are preserved.")
                .font(.callout).fixedSize(horizontal: false, vertical: true)
            if let request = exporter.request {
                Divider()
                Text("Last destination").font(.caption.weight(.semibold))
                Text(verbatim: request.destination).font(.caption.monospaced()).textSelection(.enabled)
                Text("Original bytes \(request.target.start)..<\(request.target.end)")
                    .font(.caption).foregroundStyle(.secondary)
            }
            if !panelNotice.isEmpty { Text(panelNotice).font(.callout).foregroundStyle(.orange) }
            if !exporter.notice.isEmpty {
                Text(exporter.notice).font(.callout).textSelection(.enabled)
                    .accessibilityLabel("Source export outcome: " + exporter.notice)
            }
            HStack {
                if exporter.busy {
                    ProgressView().controlSize(.small)
                    Button(exporter.cancellationRequested ? "Cancel requested" : "Cancel export", action: exporter.cancel)
                        .disabled(exporter.cancellationRequested)
                }
                Spacer()
                Button(exporter.request == nil ? "Choose destination…" : "Choose another destination…", action: chooseDestination)
                    .buttonStyle(.borderedProminent)
                    .disabled(exporter.busy || currentTarget == nil || exporter.domain == nil)
            }
        }
        .padding(20).frame(minWidth: 680, idealWidth: 760)
        .interactiveDismissDisabled(exporter.busy)
        .onDisappear { exporter.cancel() }
    }

    private func chooseDestination() {
        guard !exporter.busy, let currentTarget, let domain = exporter.domain else { return }
        let panel = NSSavePanel()
        panel.title = "Export original captured source"
        panel.prompt = "Export"
        panel.nameFieldStringValue = "CapturedSource.bin"
        panel.message = "Write exactly \(currentTarget.byteCount) retained bytes. Choose a new filename. Existing files will not be replaced."
        panel.canCreateDirectories = true
        guard panel.runModal() == .OK, let url = panel.url else { return }
        let access = AtlasSavedFileAccess(selectedURL: url)
        guard reader.acceptsOriginalExport(currentTarget),
              let source = reader.retainedExportSource(matching: currentTarget.identity, registryDomain: domain) else {
            panelNotice = "The source or selection changed while choosing a destination. No export was started. Close this panel and choose Export again."
            return
        }
        guard let destination = access.nativePath else {
            panelNotice = "The selected destination cannot be represented exactly by the native export interface. No export was started."
            return
        }
        panelNotice = ""
        exporter.save(source: source, target: currentTarget, destination: destination, destinationAccess: access)
    }

    private func number(_ value: String) -> UInt64? {
        let value = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let number = UInt64(value), String(number) == value else { return nil }
        return number
    }
}
