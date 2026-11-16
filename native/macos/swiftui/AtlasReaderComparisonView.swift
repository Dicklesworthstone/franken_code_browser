import SwiftUI

/// The user chooses the before/after roles by pin order. Comparison navigates
/// exact retained ranges in both readers. Full targets remain distinct from
/// bounded displayed intersections, including insertion/deletion point anchors.
@MainActor struct AtlasReaderComparisonView: View {
    let beforeLabel: String
    let afterLabel: String
    @Bindable var before: AtlasPagedReaderModel
    @Bindable var after: AtlasPagedReaderModel
    var project: AtlasProjectBrowserModel? = nil
    var title = "Compare pinned sources"
    var roleExplanation = "Before and after are the first and second pins. Open a range to select both sides. Byte ranges may split characters; each reader identifies the exact bytes shown."
    var canKeep: ((AtlasPagedReaderModel) -> Bool)? = nil
    var keep: ((AtlasPagedReaderModel) -> Void)? = nil
    @State private var comparison = AtlasReaderComparisonModel()
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Label(title, systemImage: "arrow.left.arrow.right")
                    .font(.headline)
                Spacer()
                if comparison.busy {
                    ProgressView().controlSize(.small)
                    Button("Cancel", action: comparison.cancel)
                } else {
                    Button("Compare") { comparison.compare(before: before, after: after) }
                        .disabled(!before.canCompare || !after.canCompare)
                    if let report = comparison.report, report.quality != "exact",
                       report.limits.editDistance < 512 || report.limits.work < 64 * 1024 * 1024 {
                        Button("Refine") { comparison.compare(before: before, after: after, refine: true) }
                            .disabled(!before.canCompare || !after.canCompare)
                            .help("Retry with the bounded larger edit and work allowance")
                    }
                }
                Button("Done") { comparison.close(); dismiss() }.keyboardShortcut(.cancelAction)
            }.padding(12).background(.bar)
            VStack(alignment: .leading, spacing: 5) {
                Text(comparison.notice).font(.callout)
                Text(roleExplanation)
                    .font(.caption).foregroundStyle(.secondary)
                if let report = comparison.report,
                   report.matches(before: before.page?.identity, after: after.page?.identity) {
                    HStack {
                        Button { comparison.moveChange(backwards: true, before: before, after: after) } label: {
                            Label("Previous change", systemImage: "chevron.up")
                        }
                        .disabled(!comparison.canMoveChange(backwards: true) || before.busy || after.busy)
                        Button { comparison.moveChange(backwards: false, before: before, after: after) } label: {
                            Label("Next change", systemImage: "chevron.down")
                        }
                        .disabled(!comparison.canMoveChange(backwards: false) || before.busy || after.busy)
                        Text(comparison.changePosition).font(.caption)
                        Spacer()
                        Text("\(report.work) / \(report.limits.work) work units").font(.caption2).foregroundStyle(.secondary)
                        Toggle("Show equal ranges", isOn: $comparison.showsEqual)
                            .toggleStyle(.checkbox).font(.caption)
                    }.controlSize(.small)
                    ScrollViewReader { scroller in
                        ScrollView {
                            LazyVStack(alignment: .leading, spacing: 6) {
                                ForEach(comparison.showsEqual ? report.spans : report.changes) { span in
                                    HStack(spacing: 10) {
                                        Image(systemName: comparison.selectedSpanID == span.id ? "arrow.right.circle.fill" : "circle")
                                            .foregroundStyle(comparison.selectedSpanID == span.id ? Color.accentColor : Color.secondary)
                                            .accessibilityHidden(true)
                                        Text(span.label).font(.caption.weight(.semibold))
                                            .frame(width: 130, alignment: .leading)
                                        Text("\(span.before.start)..<\(span.before.end) → \(span.after.start)..<\(span.after.end)")
                                            .font(.system(.caption, design: .monospaced)).lineLimit(1)
                                        Spacer()
                                        Button("Open pair") { comparison.open(spanID: span.id, before: before, after: after) }
                                            .disabled(before.busy || after.busy || comparison.busy)
                                            .accessibilityLabel("Open both sides of \(span.label.lowercased()), before byte \(span.before.start), after byte \(span.after.start)")
                                    }.id(span.id)
                                }
                            }.padding(.vertical, 3)
                        }
                        .onChange(of: comparison.selectedSpanID) { _, id in
                            if let id { scroller.scrollTo(id, anchor: .center) }
                        }
                    }.frame(height: 125)
                }
            }.padding(.horizontal, 12).padding(.vertical, 8)
            Divider()
            HSplitView {
                VStack(spacing: 0) {
                    sideLabel("Before", path: beforeLabel)
                    AtlasPagedSourceView(model: before, canKeep: canKeep?(before) == true, keep: keep, project: project)
                }.frame(minWidth: 360)
                VStack(spacing: 0) {
                    sideLabel("After", path: afterLabel)
                    AtlasPagedSourceView(model: after, canKeep: canKeep?(after) == true, keep: keep, project: project)
                }.frame(minWidth: 360)
            }
        }
        .frame(minWidth: 900, idealWidth: 1120, minHeight: 620, idealHeight: 760)
        .task { comparison.compare(before: before, after: after) }
        .onChange(of: before.page?.identity) { _, _ in comparison.validate(before: before.page?.identity, after: after.page?.identity) }
        .onChange(of: after.page?.identity) { _, _ in comparison.validate(before: before.page?.identity, after: after.page?.identity) }
        .onDisappear { comparison.close() }
    }

    private func sideLabel(_ role: String, path: String) -> some View {
        HStack {
            Text(role).font(.caption.weight(.semibold))
            Text(path).font(.caption).lineLimit(1).truncationMode(.middle)
            Spacer()
        }.padding(.horizontal, 10).padding(.vertical, 6).background(.bar)
    }
}
