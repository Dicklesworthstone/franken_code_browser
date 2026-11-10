import SwiftUI

/// City and the 2D atlas share file identities and source activation. Reading
/// remains in the existing frontal source pane, never on oblique building faces.
@MainActor struct AtlasCityView: View {
    @State private var model: AtlasCityModel
    let selectedPath: String?
    let scopeLabel: String
    let onSelect: (AtlasProjectCatalog.Entry) -> Void
    let returnTo2D: () -> Void

    init(revision: UUID, entries: [AtlasProjectCatalog.Entry], complete: Bool,
         seed: AtlasCitySeed, selectedPath: String?, scopeLabel: String,
         onSelect: @escaping (AtlasProjectCatalog.Entry) -> Void, returnTo2D: @escaping () -> Void) {
        _model = State(initialValue: AtlasCityModel(revision: revision, entries: entries, complete: complete, seed: seed))
        self.selectedPath = selectedPath; self.scopeLabel = scopeLabel
        self.onSelect = onSelect; self.returnTo2D = returnTo2D
    }

    var body: some View {
        ZStack(alignment: .topLeading) {
            AtlasCityMetalView(scene: model.scene, frame: model.frame, display: model.display,
                selected: model.selectedIndex,
                viewport: { model.resize(width: $0, height: $1, backing: $2) },
                move: model.move, click: { model.click(x: $0, y: $1) },
                presented: model.presented, failed: model.renderingFailed)
            VStack(alignment: .leading, spacing: 5) {
                Text("City · \(scopeLabel)").font(.headline)
                Text("Height: observed file bytes, logarithmic, capped at 1 GiB.")
                    .font(.caption)
                if let scene = model.scene {
                    Text("\(scene.instances.count) known files · \(scene.cappedCount) capped · \(scene.unknownCount) unknown sizes")
                        .font(.caption).foregroundStyle(.secondary)
                    if !scene.discoveryComplete {
                        Text("Partial catalog: more files may exist.").font(.caption).foregroundStyle(.orange)
                    }
                    Text("Known files keep a small minimum height; unknown sizes are flat and gray.")
                        .font(.caption2).foregroundStyle(.secondary)
                }
                Text("Drag to pan · Option-drag to orbit · Pinch or Command-scroll to zoom")
                    .font(.caption2).foregroundStyle(.secondary)
            }
            .padding(10).background(.regularMaterial, in: RoundedRectangle(cornerRadius: 8))
            .padding(12).allowsHitTesting(false)

            VStack {
                Spacer()
                if let selected = model.selectedDescription {
                    Text(verbatim: selected).font(.system(size: 11, design: .monospaced))
                        .lineLimit(1).truncationMode(.middle)
                        .padding(.horizontal, 10).padding(.vertical, 5)
                        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 6))
                        .accessibilityLabel("Selected file: " + selected)
                }
                controls
            }.padding(12)
            if model.failed || model.scene == nil {
                VStack(spacing: 10) {
                    if !model.failed { ProgressView().controlSize(.small) }
                    Text(model.notice).font(.callout).multilineTextAlignment(.center)
                    HStack {
                        if model.failed { Button("Retry City", action: model.retry) }
                        Button("Return to 2D", action: returnTo2D)
                    }
                }
                .padding(18).frame(maxWidth: 420)
                .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 10))
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onAppear {
            model.onSelect = onSelect; model.selectPath(selectedPath); model.start()
        }
        .onChange(of: selectedPath.map { Array($0.utf8) }) { _, _ in model.selectPath(selectedPath) }
        .onDisappear { model.close() }
    }

    private var controls: some View {
        VStack(spacing: 8) {
            HStack(spacing: 10) {
                Button("Fit City") { model.move(.fit) }
                    .help("Fit the complete City, including building heights")
                control("Pan left", "arrow.left") { model.move(.pan(60, 0)) }
                control("Pan right", "arrow.right") { model.move(.pan(-60, 0)) }
                control("Pan up", "arrow.up") { model.move(.pan(0, 60)) }
                control("Pan down", "arrow.down") { model.move(.pan(0, -60)) }
                Divider().frame(height: 18)
                control("Rotate left", "rotate.left") { model.move(.orbit(-0.15, 0)) }
                control("Rotate right", "rotate.right") { model.move(.orbit(0.15, 0)) }
                control("Lower tilt", "arrow.down.right.and.arrow.up.left") { model.move(.orbit(0, -0.1)) }
                control("Raise tilt", "arrow.up.left.and.arrow.down.right") { model.move(.orbit(0, 0.1)) }
                Divider().frame(height: 18)
                control("Zoom out", "minus.magnifyingglass") { zoom(1 / 1.4) }
                control("Zoom in", "plus.magnifyingglass") { zoom(1.4) }
            }.disabled(!model.ready)
            HStack(spacing: 12) {
                Button("Read previous file") { model.readAdjacent(-1) }
                    .keyboardShortcut("[", modifiers: [.command, .option])
                    .disabled(!model.ready)
                Button("Read next file") { model.readAdjacent(1) }
                    .keyboardShortcut("]", modifiers: [.command, .option])
                    .disabled(!model.ready)
                Text("Arrow keys pan; Option-arrows rotate and tilt.")
                    .font(.caption2).foregroundStyle(.secondary)
                Spacer(minLength: 4)
                Button("Return to 2D", action: returnTo2D)
                    .help("Return to the same 2D camera and selection")
            }
        }
        .controlSize(.small).padding(10)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 8))
    }
    private func zoom(_ factor: Double) {
        guard let frame = model.frame else { return }
        model.move(.zoom(frame.width / 2, frame.height / 2, factor))
    }
    private func control(_ title: String, _ symbol: String, action: @escaping () -> Void) -> some View {
        Button(action: action) { Image(systemName: symbol) }
            .accessibilityLabel(title).help(title)
    }
}
