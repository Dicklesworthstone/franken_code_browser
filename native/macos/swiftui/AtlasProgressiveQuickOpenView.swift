import SwiftUI

@MainActor struct AtlasProgressiveQuickOpenView: View {
    let root: String
    let accessLease: AnyObject?
    let open: @MainActor (AtlasProgressiveFileCapture) -> Void
    @State private var model = AtlasProgressiveQuickOpenModel(transport: .native)
    @FocusState private var queryFocused: Bool
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Label("Quick Open", systemImage: "doc.text.magnifyingglass").font(.headline)
                Spacer()
                Button("Restart search") { model.submit() }.disabled(model.text.isEmpty)
                    .help("Start a fresh filename search from the project root")
                Button("Done") { model.close(); dismiss() }.keyboardShortcut(.cancelAction)
            }
            Text(verbatim: root).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
            HStack {
                TextField("Filename or path…", text: $model.text)
                    .textFieldStyle(.roundedBorder).focused($queryFocused)
                    .accessibilityLabel("Filename or path to find in the project")
                    .onSubmit { if model.canOpen { activate() } else if !model.busy { model.submit() } }
                    .onKeyPress(.downArrow) { model.move(backwards: false); return .handled }
                    .onKeyPress(.upArrow) { model.move(backwards: true); return .handled }
                Button("Find") { model.submit() }.disabled(model.text.isEmpty)
                if model.busy {
                    ProgressView().controlSize(.small).accessibilityLabel(model.opening ? "Capturing selected file" : "Finding project files")
                    Button(model.opening ? "Cancel opening" : "Stop", action: model.stop)
                }
            }
            HStack {
                Picker("Match", selection: $model.mode) {
                    ForEach(AtlasFileFindMode.allCases, id: \.self) { Text($0.label).tag($0) }
                }
                .pickerStyle(.segmented).frame(width: 240)
                Toggle("Match case", isOn: $model.matchCase).toggleStyle(.checkbox)
                    .help("When off, uses Unicode lowercase mapping, not full case folding or canonical normalization")
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
                            Text(row.explanation).font(.caption).foregroundStyle(.secondary)
                        }
                        .contentShape(Rectangle())
                        .tag(report.selection(row.id))
                        .onTapGesture(count: 2) {
                            model.selected = report.selection(row.id)
                            activate()
                        }
                    }
                }
                .onSubmit(activate)
                .overlay {
                    if report.rows.isEmpty {
                        Text(report.more ? (model.busy ? "Searching more filenames…" : "No matches yet. Continue searching.")
                            : (report.complete ? "No matching filenames" : "No matches in the searched files; coverage is incomplete."))
                            .foregroundStyle(.secondary)
                    }
                }
                HStack {
                    Text("\(report.catalogued) filenames discovered · \(report.searched) searched")
                        .font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    if report.more {
                        Button("Continue", action: model.continueSearch).disabled(!model.canContinue)
                            .help("Search another bounded group of files and update the retained best matches")
                        if !model.busy, !report.paused { Button("Stop", action: model.stop) }
                    }
                }
            } else {
                ContentUnavailableView(model.busy ? "Finding files…" : "Find a project file",
                    systemImage: "doc.text.magnifyingglass",
                    description: Text("Search continues until useful results appear. Continue explores further; changing the query starts a fresh search."))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            HStack(alignment: .bottom) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(model.notice).textSelection(.enabled)
                    Text("Filenames are searched without loading source. Opening a result captures its current contents.")
                }
                .font(.caption).foregroundStyle(.secondary).frame(maxWidth: .infinity, alignment: .leading)
                Button("Reload results", action: model.reload).disabled(!model.canReload)
                    .help("Recover the accepted result snapshot without advancing discovery")
                Button("Open file", action: activate).disabled(!model.canOpen)
            }
        }
        .padding(16).frame(minWidth: 740, idealWidth: 860, minHeight: 480, idealHeight: 580)
        .onAppear { model.bind(root: root, accessLease: accessLease); queryFocused = true }
        .onDisappear { model.close() }
    }
    private func activate() {
        model.openSelected(readerTransport: .native, search: .native, outline: .native, document: .native, deliver: open)
    }
}
