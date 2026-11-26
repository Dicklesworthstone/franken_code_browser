import SwiftUI

/// Browse whole-token text occurrences in one retained source. Every activation uses
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
                    .accessibilityLabel("Whole identifier in the retained source")
                    .onSubmit(model.findReferences)
                Button("Find", action: model.findReferences)
                    .disabled(!model.canFindReferences || model.referenceName.isEmpty || model.busy)
            }
            Toggle("Match case", isOn: $model.referenceCaseSensitive)
                .disabled(!model.supportsReferenceMatching || model.busy)
                .help("Turn off to compare Unicode lowercase spellings while keeping exact source ranges.")
            Text(model.referenceCaseSensitive
                ? "Matches the whole identifier, including declarations, comments and string literals. Case-sensitive; no compiler binding resolution."
                : "Matches whole identifiers using Unicode lowercase. Original spelling and source ranges are preserved; comments and literals are included. No compiler binding resolution.")
                .font(.caption).foregroundStyle(.secondary)
            if let page = model.referencePage {
                Text(page.summary).font(.caption).foregroundStyle(.secondary)
                    .accessibilityLabel(page.summary)
                if !model.referencesAreCurrent {
                    Text("These results are inactive. Run Find again to use them.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                if page.rows.isEmpty {
                    Text(page.inventory.matchesCounted == 0
                        ? "No whole-identifier occurrences in this retained capture."
                        : "No occurrences in this result window.")
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
                            .accessibilityLabel("Open occurrence \(occurrence.ordinal) of \(page.inventory.name), line \(occurrence.line)")
                            .help("Select this exact occurrence in the retained source; line numbers count LF boundaries")
                        }
                    }
                    .id(page.id)
                }
                HStack {
                    Button("Previous page", action: model.previousReferencePage)
                        .disabled(!model.canReferenceBack)
                    Text("\(page.rows.first?.ordinal ?? 0)...\(page.rows.last?.ordinal ?? 0)")
                        .font(.caption).opacity(page.rows.isEmpty ? 0 : 1)
                    Spacer()
                    Button("Next page", action: model.nextReferencePage)
                        .disabled(!model.referencesAreCurrent || page.nextOffset == nil || model.busy)
                }
                if page.inventory.window != nil {
                    HStack {
                        Button("Previous results", action: model.previousReferenceWindow)
                            .disabled(!model.canReferenceWindowBack)
                        Spacer()
                        Button("Next results", action: model.nextReferenceWindow)
                            .disabled(!model.canReferenceWindowNext)
                    }
                    .help("Open another bounded set of occurrences from this same retained source.")
                }
            } else {
                Text(model.canFindReferences
                    ? "Enter an identifier, use Search selection in the reader, or choose Occurrences beside a source outline entry."
                    : "Identifier occurrences are available for retained source captures up to 32 MiB.")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            if !model.notice.isEmpty {
                Text(model.notice).font(.caption2).foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }
            Text(model.supportsReferenceWindows
                ? "Searches this retained file up to 32 MiB, counts every whole-token occurrence and retains up to 4,096 per result window. Line numbers count LF boundaries. Changing the identifier, case option or capture invalidates old targets."
                : "Searches this retained file only, up to 32 MiB and 4,096 occurrences. Line numbers count LF boundaries; exact byte ranges also work for CR-only source. Changing the identifier, case option or capture invalidates old targets.")
                .font(.caption2).foregroundStyle(.secondary)
        }
        .padding(12)
        .frame(width: 470, height: 440)
    }
}
