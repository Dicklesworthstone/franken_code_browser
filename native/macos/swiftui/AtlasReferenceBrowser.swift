import SwiftUI

/// Browse exact text occurrences in one retained source. Every activation uses
/// the original inventory/page token; list offsets never become source targets.
@MainActor struct AtlasReferenceBrowser: View {
    @Bindable var model: AtlasPagedReaderModel
    var openOccurrence: (() -> Void)? = nil

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("Identifier occurrences").font(.headline)
                Spacer()
                if model.busy {
                    ProgressView().controlSize(.small)
                    Button("Cancel", action: model.cancel)
                }
            }
            HStack {
                TextField("Identifier…", text: $model.referenceName)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel("Exact case-sensitive identifier in the retained source")
                    .onSubmit(model.findReferences)
                Button("Find", action: model.findReferences)
                    .disabled(!model.canFindReferences || model.referenceName.isEmpty || model.busy)
            }
            Text("Matches the whole identifier, including declarations, comments and string literals. Case-sensitive; no compiler binding resolution.")
                .font(.caption).foregroundStyle(.secondary)
            if let page = model.referencePage {
                Text(page.summary).font(.caption).foregroundStyle(.secondary)
                    .accessibilityLabel(page.summary)
                if !model.referencesAreCurrent {
                    Text("These results are inactive. Run Find again to use them.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                if page.rows.isEmpty {
                    Text("No whole-identifier occurrences in this retained capture.")
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    List {
                        ForEach(page.rows) { occurrence in
                            Button {
                                guard model.referencesAreCurrent, !model.busy,
                                      let target = page.target(id: occurrence.id) else { return }
                                model.openReference(target)
                                openOccurrence?()
                            } label: {
                                HStack(alignment: .firstTextBaseline) {
                                    Text("Line \(occurrence.line)")
                                        .font(.system(.body, design: .monospaced))
                                    Spacer()
                                    Text("bytes \(occurrence.range.start)..<\(occurrence.range.end)")
                                        .font(.system(.caption, design: .monospaced))
                                        .foregroundStyle(.secondary)
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .disabled(!model.referencesAreCurrent || model.busy)
                            .accessibilityLabel("Open occurrence \(occurrence.id) of \(page.inventory.name), line \(occurrence.line)")
                            .help("Select this exact occurrence in the retained source; line numbers count LF boundaries")
                        }
                    }
                    .id(page.id)
                }
                HStack {
                    Button("Previous page", action: model.previousReferencePage)
                        .disabled(!model.canReferenceBack)
                    Text("\(page.request.start + 1)...\(page.request.start + UInt64(page.rows.count))")
                        .font(.caption).opacity(page.rows.isEmpty ? 0 : 1)
                    Spacer()
                    Button("Next page", action: model.nextReferencePage)
                        .disabled(!model.referencesAreCurrent || page.nextOffset == nil || model.busy)
                }
            } else {
                Text(model.canFindReferences
                    ? "Enter an identifier, or choose Occurrences beside a source outline entry."
                    : "Identifier occurrences are available for retained source captures up to 512 KiB.")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            if !model.notice.isEmpty {
                Text(model.notice).font(.caption2).foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
            Text("Searches this retained file only, up to 512 KiB and 4,096 occurrences. Line numbers count LF boundaries; exact byte ranges also work for CR-only source. Changing the identifier or refreshing the capture invalidates old targets.")
                .font(.caption2).foregroundStyle(.secondary)
        }
        .padding(12)
        .frame(width: 470, height: 440)
    }
}
