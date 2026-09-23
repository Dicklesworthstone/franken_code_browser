import AppKit
import QuartzCore

// Run with the production retained surface and its ordinary CPU acceptance path.
// Metal submission does not participate in directory path construction.
@main struct AtlasDirectoryPathTests {
    @MainActor static func main() {
        _ = NSApplication.shared
        let capture = AtlasHighlightCapture(schema: "fcb.source-document/1", text: "let source = 1\n",
            runs: [.init(start: "0", length: "15", role: "keyword")])
        guard let document = AtlasDocument(path: "root/nested/leaf/file.swift", capture: capture),
              let tile = document.tiles.first else { preconditionFailure("source fixture") }
        let parcel = CGRect(x: 0, y: 0, width: 500, height: 300)
        tile.rect = parcel.insetBy(dx: 5, dy: 5); tile.parcelRect = parcel; tile.parcelFirst = true
        let surface = AtlasRetainedSurface(metalEnabled: false)
        surface.frame = CGRect(x: 0, y: 0, width: 200, height: 100)
        let revision = UUID()
        func update(_ scale: Double, _ offset: CGPoint, selected: Bool = false) {
            surface.update(tiles: [tile], revision: revision, scale: scale, offset: offset,
                           selectedPath: selected ? tile.path : nil, hitPaths: [])
        }
        func shapes(_ layer: CALayer) -> [CAShapeLayer] {
            (layer as? CAShapeLayer).map { [$0] } ?? (layer.sublayers ?? []).flatMap(shapes)
        }
        func boundaries() -> [CAShapeLayer] {
            guard let layer = surface.layer else { preconditionFailure("retained root layer") }
            return shapes(layer).filter { $0.zPosition == 2.2 }
        }
        var checks = 0
        func verify(_ scale: Double) {
            let layers = boundaries().sorted { ($0.path?.boundingBox.minX ?? -1) < ($1.path?.boundingBox.minX ?? -1) }
            precondition(layers.count == 3, "three real directory ancestors")
            for (index, layer) in layers.enumerated() {
                let depth = index + 1
                let width = min((depth <= 2 ? 2.0 : 1.25) / scale, 3.75)
                let inset = min((1 + Double(depth) * 1.75) / scale, 5 - width, 75)
                let expected = CGPath(rect: CGRect(x: inset, y: inset,
                    width: 500 - 2 * inset, height: 300 - 2 * inset), transform: nil)
                precondition(!layer.isHidden && layer.path == expected,
                             "visible directory must restore current-scale ancestor geometry")
                precondition(layer.lineWidth == width)
                precondition(inset + width <= 5, "directory stroke must stay outside source text")
                precondition(layer.strokeColor != nil)
                checks += 1
            }
            let fileBorders = shapes(surface.layer!).filter { $0.zPosition == 2 }
            precondition(fileBorders.isEmpty, "file outlines are retained border layers, not stroked paths")
            let fileOutline = (surface.layer!.sublayers ?? []).flatMap { $0.sublayers ?? [] }
                .first { $0.zPosition == 2 && !($0 is CAShapeLayer) }
            precondition(fileOutline != nil && fileOutline!.borderWidth < 5,
                         "file border must stay inside its text-free parcel gutter")
        }
        update(1, CGPoint(x: -4000, y: -4000))
        precondition(boundaries().count == 3 && boundaries().allSatisfy { $0.isHidden && $0.path == nil },
                     "initially hidden directories need no geometry")
        update(1, .zero); verify(1)
        let colors = surface.retainedDirectoryColors
        let initial = boundaries().map { $0.path }
        update(2, CGPoint(x: -4000, y: -4000))
        precondition(boundaries().allSatisfy(\.isHidden))
        precondition(zip(boundaries(), initial).allSatisfy { $0.path == $1 },
                     "hidden directory geometry must remain retained instead of rebuilding")
        update(2, .zero); verify(2) // Pure pan: scale is unchanged, stale paths must refresh.
        update(0.01, .zero)
        precondition(boundaries().allSatisfy(\.isHidden), "sub-threshold boundaries stay hidden")
        update(0.2, .zero); verify(0.2)
        update(0.4, CGPoint(x: -4000, y: -4000))
        surface.frame.size = CGSize(width: 300, height: 200)
        update(0.4, .zero, selected: true); verify(0.4)
        precondition(surface.retainedDirectoryColors == colors, "selection and camera preserve colors")
        surface.stopClock()
        print("Directory path regression passed: \(checks) exact visible states, pure-pan re-entry, threshold, resize and selection")
    }
}
