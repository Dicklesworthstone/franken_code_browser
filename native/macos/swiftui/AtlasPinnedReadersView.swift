import SwiftUI

/// One selected pin beside the current reader; switching the bounded pin tabs
/// keeps each parent-owned model, capture, position and query alive.
@MainActor struct AtlasPinnedReadersView: View {
    @Bindable var readers: AtlasPinnedReaders<AtlasPagedReaderModel>

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Label("Pinned sources", systemImage: "pin.fill")
                    .font(.caption.weight(.semibold))
                Text("\(readers.entries.count)/\(AtlasPinnedReaders<AtlasPagedReaderModel>.maximumPins)")
                    .font(.caption).foregroundStyle(.secondary)
                Spacer()
                if readers.busy {
                    ProgressView().controlSize(.small).accessibilityLabel("Pinning source")
                    Button("Cancel", action: readers.cancelPin)
                }
                if let selected = readers.selected {
                    Button {
                        readers.close(selected.id)
                    } label: { Label("Close pin", systemImage: "xmark") }
                    .help("Close this pinned reader; other readers keep their captures and positions")
                }
            }
            .controlSize(.small).padding(.horizontal, 10).padding(.vertical, 6)
            .background(.bar)
            if !readers.entries.isEmpty {
                ScrollView(.horizontal) {
                    HStack(spacing: 6) {
                        ForEach(readers.entries) { entry in
                            Button { readers.select(entry.id) } label: {
                                Text((entry.label as NSString).lastPathComponent)
                                    .lineLimit(1).truncationMode(.middle)
                            }
                            .buttonStyle(.bordered)
                            .tint(readers.selectedID == entry.id ? .accentColor : .secondary)
                            .help(entry.label)
                            .accessibilityLabel("Pinned source: \(entry.label)")
                            .accessibilityAddTraits(readers.selectedID == entry.id ? .isSelected : [])
                        }
                    }.padding(.horizontal, 10).padding(.vertical, 4)
                }
                .frame(height: 34)
            }
            Divider()
            if let entry = readers.selected {
                AtlasPagedSourceView(model: entry.reader)
                    .id(entry.id)
            } else {
                Text(readers.busy ? "Preparing an independent retained reader…" : "Pin a retained source to read files side by side.")
                    .font(.callout).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            if !readers.notice.isEmpty {
                Text(readers.notice).font(.caption2).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 10).padding(.vertical, 4)
            }
        }
        .background(.background)
    }
}
