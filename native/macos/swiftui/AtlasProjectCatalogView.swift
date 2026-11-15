import SwiftUI

/// A real file browser over validated metadata, available before any previews.
/// Labels are the engine's escaped display form; only the parent resolves the
/// selected response-local ID into its current authorized source path.
@MainActor struct AtlasProjectCatalogView: View {
    let catalog: AtlasProjectCatalog
    let entries: [AtlasProjectCatalog.Entry]
    let preparing: Bool
    let canPrepare: Bool
    let hasAtlas: Bool
    let progress: String
    let build: () -> Void
    let cancel: () -> Void
    let refresh: () -> Void
    let showAtlas: () -> Void
    let open: (UInt64) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 10) {
                Label("Project files", systemImage: "folder").font(.headline)
                Spacer()
                if hasAtlas { Button("Show text atlas", action: showAtlas) }
                if preparing {
                    ProgressView().controlSize(.small).accessibilityLabel("Preparing text atlas")
                    Button("Stop previews", action: cancel)
                } else if !hasAtlas {
                    Button("Build text atlas", action: build).disabled(!canPrepare)
                        .help("Prepare bounded source previews explicitly. File reading and search do not require them.")
                }
                Button("Refresh project", action: refresh)
                    .help("Rediscover project membership and retire the old project view")
            }.padding(12)
            Text(catalog.summary).font(.callout).foregroundStyle(.secondary)
                .textSelection(.enabled).padding(.horizontal, 12).padding(.bottom, 6)
            if preparing {
                Text(progress + " You can keep opening files and searching.")
                    .font(.caption).padding(.horizontal, 12).padding(.bottom, 6)
            }
            Text("\(entries.count) files in the current filter · catalog limit \(AtlasProjectCatalog.maximumFiles) · policy: \(catalog.policy)")
                .font(.caption).foregroundStyle(.secondary).padding(.horizontal, 12).padding(.bottom, 8)
            Divider()
            if entries.isEmpty {
                ContentUnavailableView {
                    Label(catalog.entries.isEmpty && catalog.discoveryComplete ? "No catalogued files" : "No files in this view", systemImage: "folder")
                } description: {
                    Text(catalog.discoveryComplete ? "Change the file filter or choose another project."
                        : "Discovery was partial. Unseen files are not known to be absent; refresh or choose a narrower project root.")
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List(entries) { entry in
                    Button { open(entry.id) } label: {
                        HStack(alignment: .top, spacing: 10) {
                            Image(systemName: entry.sourcePath == nil ? "exclamationmark.triangle" : "doc.text")
                                .accessibilityHidden(true)
                            VStack(alignment: .leading, spacing: 3) {
                                Text(verbatim: entry.displayPath)
                                    .font(.system(size: 12, design: .monospaced)).lineLimit(2)
                                if entry.sourcePath == nil {
                                    Text("Filename is not UTF-8; retained in the catalog but unavailable to this native opener.")
                                        .font(.caption2).foregroundStyle(.secondary)
                                }
                            }
                            Spacer()
                            Text(String(entry.observedBytes) + " bytes")
                                .font(.system(size: 10, design: .monospaced)).foregroundStyle(.secondary)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .disabled(entry.sourcePath == nil)
                    .accessibilityLabel("Open " + entry.displayPath)
                    .help(entry.sourcePath == nil ? "Raw filename cannot be opened by the UTF-8 host"
                        : "Open a new retained source capture; preview preparation is not required")
                }
                .listStyle(.inset)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
