import SwiftUI

/// Quick Open uses filename metadata, not source text, atlas tiles or a native
/// replacement matcher. The calling window authorizes the current project root.
@MainActor struct AtlasQuickOpenView: View {
    let root: String
    let accessLease: AnyObject?
    let open: @MainActor (AtlasFileOpenChoice) -> Void
    @State private var model = AtlasQuickOpenModel(transport: .native)
    @FocusState private var queryFocused: Bool
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label("Quick Open", systemImage: "doc.text.magnifyingglass").font(.headline)
                Spacer()
                Button("Refresh catalog", action: model.refresh)
                    .help("Discover current filenames again; ordinary query edits reuse the frozen catalog")
                Button("Done") { model.close(); dismiss() }.keyboardShortcut(.cancelAction)
            }
            Text(verbatim: root).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
            HStack {
                TextField("Filename or path…", text: $model.text)
                    .textFieldStyle(.roundedBorder).focused($queryFocused)
                    .accessibilityLabel("Filename or path to find in the project")
                    .onSubmit { model.activate(deliver: open) }
                    .onKeyPress(.downArrow) { model.move(backwards: false); return .handled }
                    .onKeyPress(.upArrow) { model.move(backwards: true); return .handled }
                Button("Find") { model.submit() }.disabled(model.text.isEmpty)
                if model.busy {
                    ProgressView().controlSize(.small).accessibilityLabel("Finding project files")
                    Button("Cancel", action: model.cancel)
                }
            }
            HStack {
                Picker("Match", selection: $model.mode) {
                    ForEach(AtlasFileFindMode.allCases, id: \.self) { Text($0.label).tag($0) }
                }
                .pickerStyle(.segmented).frame(width: 240)
                Toggle("Match case", isOn: $model.matchCase).toggleStyle(.checkbox)
                    .help("When off, uses the engine's Unicode lowercase mapping, not full case folding or canonical normalization")
                Spacer()
                Text("Workspace filenames").font(.caption).foregroundStyle(.secondary)
            }
            if let report = model.report, model.isCurrent {
                List(selection: $model.selected) {
                    ForEach(report.rows) { row in
                        HStack(alignment: .firstTextBaseline, spacing: 10) {
                            Image(systemName: "doc.text").foregroundStyle(.secondary)
                            Text(verbatim: row.display).font(.system(size: 12, design: .monospaced))
                                .lineLimit(2).frame(maxWidth: .infinity, alignment: .leading)
                            Text(row.path == nil ? "Filename cannot be opened" : row.explanation)
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        .contentShape(Rectangle())
                        .tag(report.rowID(row.id))
                        .disabled(row.path == nil)
                        .onTapGesture(count: 2) {
                            model.selected = report.rowID(row.id)
                            model.openSelected(deliver: open)
                        }
                    }
                }
                .id(report.id)
                .onSubmit { model.openSelected(deliver: open) }
                .overlay {
                    if report.rows.isEmpty {
                        Text(report.complete ? "No matching filenames" : "No filenames found in the partial catalog")
                            .foregroundStyle(.secondary)
                    }
                }
            } else {
                ContentUnavailableView(model.busy ? "Finding files…" : "Find a project file",
                    systemImage: "doc.text.magnifyingglass",
                    description: Text("Find files by name or path without loading their contents. Fuzzy, exact, and prefix modes use the shared engine."))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            HStack(alignment: .bottom) {
                Text(model.notice).font(.caption).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading).textSelection(.enabled)
                Button("Open file") { model.openSelected(deliver: open) }
                    .disabled(!model.canOpen)
                    .help("Open the selected file separately; no source-search match coordinates are used")
            }
        }
        .padding(16).frame(minWidth: 720, idealWidth: 800, minHeight: 460, idealHeight: 560)
        .onAppear { model.bind(root: root, accessLease: accessLease); queryFocused = true }
        .onDisappear { model.close() }
    }
}
