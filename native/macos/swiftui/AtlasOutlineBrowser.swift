import SwiftUI

/// This is a bounded presentation of Rust's retained candidate index, not a
/// second parser or compiler resolver. Filtering never reopens the pathname.
@MainActor struct AtlasOutlineBrowser: View {
    @Bindable var model: AtlasPagedReaderModel

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("Source outline").font(.headline)
                Spacer()
                if model.busy {
                    ProgressView().controlSize(.small)
                    Button("Cancel", action: model.cancel)
                }
                Button(model.hasOutline ? "Rebuild" : "Build", action: model.buildOutline)
                    .disabled(!model.canBuildOutline || model.busy)
            }
            Picker("Language", selection: $model.outlineLanguage) {
                ForEach(AtlasOutlineLanguage.allCases) { Text($0.label).tag($0) }
            }
            HStack {
                TextField("Symbol name…", text: $model.outlineNeedle)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel("Filter retained symbol names")
                    .onSubmit(model.filterOutline)
                Picker("Name matching", selection: $model.outlineMode) {
                    ForEach(AtlasOutlineNameMode.allCases) { Text($0.rawValue).tag($0) }
                }
                .labelsHidden().frame(width: 95)
                Button("Filter", action: model.filterOutline)
                    .disabled(!model.hasOutline || model.busy)
            }
            if let page = model.outlinePage {
                Text(page.summary).font(.caption).foregroundStyle(.secondary)
                List {
                    ForEach(page.rows) { symbol in
                        Button {
                            // Capture the row's ORIGINAL page token, not a
                            // reusable list offset or the latest page's row ID.
                            if let target = page.target(id: symbol.id) { model.openSymbol(target) }
                        } label: {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(String(reflecting: symbol.name))
                                    .font(.system(.body, design: .monospaced)).lineLimit(1)
                                Text("\(symbol.kind) · line \(symbol.line)" + (symbol.nameRange == nil ? " · declaration evidence" : ""))
                                    .font(.caption2).foregroundStyle(.secondary)
                            }
                            .padding(.leading, CGFloat(min(symbol.depth, 8)) * 8)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .disabled(!model.outlineIsCurrent || model.busy)
                        .accessibilityLabel("Open outline candidate " + String(reflecting: symbol.name))
                        .help("Navigate this candidate in the retained capture; this is not compiler-proven name resolution")
                    }
                }
                .id(page.id)
                HStack {
                    Button("Previous page", action: model.previousOutlinePage).disabled(!model.canOutlineBack)
                    Text("\(page.request.start + 1)...\(page.request.start + UInt64(page.rows.count))")
                        .font(.caption).opacity(page.rows.isEmpty ? 0 : 1)
                    Spacer()
                    Button("Next page", action: model.nextOutlinePage)
                        .disabled(page.nextOffset == nil || model.busy)
                }
            } else {
                Text(model.hasOutline ? "Apply the filter to display a new symbol page." : "Build an outline from the retained source.")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            if !model.notice.isEmpty { Text(model.notice).font(.caption2).foregroundStyle(.secondary) }
            Text("Heuristic candidates only. The shared extractor currently admits up to 64 KiB source and 4,096 items, with additional complexity limits. Source reading remains available when extraction is refused.")
                .font(.caption2).foregroundStyle(.secondary)
        }
        .padding(12)
        .frame(width: 440, height: 400)
        .onAppear {
            if !model.hasOutline && !model.busy { model.buildOutline() }
        }
    }
}
