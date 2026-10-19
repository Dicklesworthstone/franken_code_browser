import SwiftUI

/// The user chooses the before/after roles by pin order. Comparison navigates
/// exact retained byte anchors in the existing readers, so page/search/outline
/// behavior and original-byte copy continue through the ordinary reader UI.
@MainActor struct AtlasReaderComparisonView: View {
    let beforeLabel: String
    let afterLabel: String
    @Bindable var before: AtlasPagedReaderModel
    @Bindable var after: AtlasPagedReaderModel
    @State private var comparison = AtlasReaderComparisonModel()
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Label("Compare pinned sources", systemImage: "arrow.left.arrow.right")
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
                Text("Before and after are the first and second pins. Ranges use original bytes; opening a range may widen the page to complete text characters.")
                    .font(.caption).foregroundStyle(.secondary)
                if let report = comparison.report,
                   report.matches(before: before.page?.identity, after: after.page?.identity) {
                    HStack {
                        Text("\(report.work) / \(report.limits.work) work units").font(.caption2).foregroundStyle(.secondary)
                        Spacer()
                        Toggle("Show equal ranges", isOn: $comparison.showsEqual)
                            .toggleStyle(.checkbox).font(.caption)
                    }
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 6) {
                            ForEach(comparison.showsEqual ? report.spans : report.changes) { span in
                                HStack(spacing: 10) {
                                    Text(span.label).font(.caption.weight(.semibold))
                                        .frame(width: 130, alignment: .leading)
                                    Text("\(span.before.start)…\(span.before.end) → \(span.after.start)…\(span.after.end)")
                                        .font(.system(.caption, design: .monospaced)).lineLimit(1)
                                    Spacer()
                                    Button("Before") { before.goToComparedRange(span.before, identity: report.before) }
                                        .disabled(before.busy || comparison.busy)
                                        .accessibilityLabel("Open before \(span.label.lowercased()) at byte \(span.before.start)")
                                    Button("After") { after.goToComparedRange(span.after, identity: report.after) }
                                        .disabled(after.busy || comparison.busy)
                                        .accessibilityLabel("Open after \(span.label.lowercased()) at byte \(span.after.start)")
                                }
                            }
                        }.padding(.vertical, 3)
                    }.frame(height: 125)
                }
            }.padding(.horizontal, 12).padding(.vertical, 8)
            Divider()
            HSplitView {
                VStack(spacing: 0) {
                    sideLabel("Before", path: beforeLabel)
                    AtlasPagedSourceView(model: before)
                }.frame(minWidth: 360)
                VStack(spacing: 0) {
                    sideLabel("After", path: afterLabel)
                    AtlasPagedSourceView(model: after)
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
