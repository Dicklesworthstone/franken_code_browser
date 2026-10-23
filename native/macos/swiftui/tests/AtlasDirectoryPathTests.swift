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
        metadataResidency()
        print("Directory path regression passed: \(checks) exact visible states, pure-pan re-entry, threshold, resize and selection")
    }

    @MainActor static func metadataResidency() {
        func entry(_ id: UInt64, _ raw: [UInt8], _ x: Double) -> AtlasProjectCatalog.Entry {
            .init(id: id, rawPath: raw, displayPath: String(decoding: raw, as: UTF8.self),
                sourcePath: String(bytes: raw, encoding: .utf8), observedBytes: 12,
                x: x, y: 0, width: 150, height: 120)
        }
        let composed = "src/é.rs", decomposed = "src/e\u{301}.rs"
        let metadata = [entry(1, Array(composed.utf8), 0), entry(2, Array(decomposed.utf8), 160),
            entry(3, [115, 114, 99, 47, 255, 46, 114, 115], 320)]
        let surface = AtlasRetainedSurface(metalEnabled: false)
        surface.frame = CGRect(x: 0, y: 0, width: 500, height: 200)
        let layout = UUID(), cold = UUID(), hot = UUID(), evicted = UUID()
        var receipts: [(UUID, UUID, Double, CGPoint)] = []
        var legacyLayouts: [UUID] = []
        surface.onPresentedContent = { receipts.append(($0, $1, $2, $3)) }
        surface.onPresentedFrame = { revision, _, _ in legacyLayouts.append(revision) }
        func drainReceipts() {
            let deadline = Date(timeIntervalSinceNow: 1)
            while surface.hasPendingContentNotification && Date() < deadline {
                _ = RunLoop.main.run(mode: .default, before: Date(timeIntervalSinceNow: 0.001))
            }
            precondition(!surface.hasPendingContentNotification, "bounded deferred receipt must drain")
        }
        surface.update(tiles: [], revision: layout, scale: 1, offset: .zero,
            selectedPath: composed, hitPaths: [], metadata: metadata, contentRevision: cold)
        precondition(surface.metrics.metadataParcels == 3 && surface.retainedMetadataRects.count == 3,
                     "every cold raw file remains visible, including distinct canonical Unicode spellings")
        precondition(surface.metrics.overviewAssignments == 0 && surface.metrics.captionRasters == 0,
                     "publishing metadata must not create source tiles or shape every filename")
        precondition(receipts.isEmpty && surface.hasPendingContentNotification,
                     "CPU acceptance must not mutate observable residency during updateNSView")
        drainReceipts()
        precondition(receipts.last?.0 == layout && receipts.last?.1 == cold,
                     "an empty resident set must acknowledge metadata so first viewport demand can run")
        for file in metadata {
            precondition(surface.retainedMetadataPath!.contains(CGPoint(x: file.x + file.width / 2, y: file.height / 2)))
        }
        precondition(surface.retainedMetadataSelectionPath!.contains(CGPoint(x: 75, y: 60)))
        precondition(!surface.retainedMetadataSelectionPath!.contains(CGPoint(x: 235, y: 60)))
        let markerBuilds = surface.metrics.metadataMarkerBuilds
        surface.update(tiles: [], revision: layout, scale: 1, offset: CGPoint(x: -20, y: -10),
            selectedPath: composed, hitPaths: [], metadata: metadata, contentRevision: cold)
        precondition(surface.metrics.metadataMarkerBuilds == markerBuilds,
                     "panning reuses marker geometry instead of rebuilding hit paths")
        surface.update(tiles: [], revision: layout, scale: 2, offset: .zero,
            selectedPath: composed, hitPaths: [], metadata: metadata, contentRevision: cold)
        precondition(surface.metrics.metadataMarkerBuilds == markerBuilds, "zoom only changes retained marker stroke width")
        surface.update(tiles: [], revision: layout, scale: 1, offset: .zero,
            selectedPath: decomposed, hitPaths: [], metadata: metadata, contentRevision: cold)
        precondition(surface.retainedMetadataSelectionPath!.contains(CGPoint(x: 235, y: 60)))
        precondition(!surface.retainedMetadataSelectionPath!.contains(CGPoint(x: 75, y: 60)),
                     "selection compares native bytes rather than canonical String equality")
        precondition(surface.metrics.contentNotificationsScheduled == 2,
                     "three camera/selection updates share one queued main-thread drain")
        drainReceipts()
        precondition(receipts.count == 2 && receipts.last?.2 == 1 && receipts.last?.3 == CGPoint.zero,
                     "the deferred callback reports only the newest accepted camera")

        let text = "let late = 1\n"
        let capture = AtlasHighlightCapture(schema: "fcb.source-document/1", text: text,
            runs: [.init(start: "0", length: String(text.utf16.count), role: "keyword")])
        let document = AtlasDocument(path: composed, capture: capture)!
        let tile = document.tiles[0]
        tile.parcelRect = CGRect(x: 0, y: 0, width: 150, height: 120)
        tile.rect = tile.parcelRect!.insetBy(dx: 4, dy: 4); tile.parcelFirst = true
        let metadataPath = surface.retainedMetadataPath
        let camera = CGPoint(x: 17, y: -9)
        surface.update(tiles: [tile], revision: layout, scale: 1.5, offset: camera,
            selectedPath: composed, hitPaths: [], metadata: metadata, contentRevision: hot)
        drainReceipts()
        precondition(surface.metrics.metadataInstalls == 1 && surface.retainedMetadataPath == metadataPath,
                     "source arrival does not rebuild or move the metadata map")
        precondition(receipts.last?.0 == layout && receipts.last?.1 == hot &&
                     receipts.last?.2 == 1.5 && receipts.last?.3 == camera)
        surface.prepareDetailPass()
        surface.update(tiles: [], revision: layout, scale: 1.5, offset: camera,
            selectedPath: nil, hitPaths: [], metadata: metadata, contentRevision: evicted)
        drainReceipts()
        precondition(surface.retainedDetailImages.isEmpty && surface.retainedOverviewImages.isEmpty,
                     "content eviction clears caches keyed by old tile array slots")
        precondition(surface.metrics.metadataInstalls == 1 && surface.retainedMetadataRects.count == 3)
        precondition(receipts.last?.1 == evicted && legacyLayouts.allSatisfy { $0 == layout },
                     "content receipts advance independently of navigation history")

        let many = (0..<20_000).map { index -> AtlasProjectCatalog.Entry in
            let name = "file-\(index).rs"
            return .init(id: UInt64(index + 1), rawPath: Array(name.utf8), displayPath: name,
                sourcePath: name, observedBytes: 0, x: Double(index % 200) * 10,
                y: Double(index / 200) * 10, width: 10, height: 10)
        }
        let rasters = surface.metrics.captionRasters
        surface.update(tiles: [], revision: UUID(), scale: 0.2, offset: .zero,
            selectedPath: nil, hitPaths: [], metadata: many, contentRevision: UUID())
        precondition(surface.metrics.metadataParcels == 20_000 && surface.retainedMetadataRects.count == 20_000)
        precondition(surface.retainedMetadataPath!.contains(CGPoint(x: 1995, y: 995)),
                     "the last cold file is present without any source preparation")
        precondition(surface.metrics.metadataCaptionVisits <= AtlasRetainedSurface.maximumMetadataCaptionVisits)
        precondition(surface.metrics.captionRasters == rasters && surface.metrics.requestedCaptions == 0,
                     "an overview never shapes the complete metadata catalog")
        surface.update(tiles: [], revision: UUID(), scale: 1, offset: .zero,
            selectedPath: nil, hitPaths: [], metadata: [], contentRevision: UUID())
        precondition(surface.retainedMetadataRects.isEmpty, "an empty scope replaces prior geometry")
        drainReceipts()
        surface.stopClock()
    }
}
