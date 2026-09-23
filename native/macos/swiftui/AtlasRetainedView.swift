import AppKit
import SwiftUI
import QuartzCore
import CoreText

private struct AtlasTransitionDraw {
    let tile: AtlasTextTile
    let rect: CGRect
    let contentScale: Double
    let clipped: CGRect
    let rows: Range<Int>
}

/// Render immutable source rows into a private context. The live camera and
/// layer tree never enter this worker; publication checks its generation.
private func renderAtlasTransition(canvas: CGRect, width: Int, height: Int,
                                   background: CGColor, draws: [AtlasTransitionDraw]) -> CGImage? {
    guard let bitmap = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
        bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
    bitmap.translateBy(x: 0, y: Double(height))
    bitmap.scaleBy(x: Double(width) / canvas.width, y: -Double(height) / canvas.height)
    bitmap.translateBy(x: -canvas.minX, y: -canvas.minY)
    for draw in draws {
        bitmap.saveGState()
        bitmap.clip(to: draw.clipped)
        bitmap.setFillColor(background)
        bitmap.fill(draw.clipped)
        bitmap.translateBy(x: draw.rect.minX, y: draw.rect.minY)
        bitmap.scaleBy(x: draw.contentScale, y: draw.contentScale)
        draw.tile.draw(in: bitmap, visibleRows: draw.rows)
        bitmap.restoreGState()
    }
    return bitmap.makeImage()
}

/// The native view owns image residence. Source capture and shaping remain in
/// AtlasDocument; camera updates only move the retained world and query detail.
struct AtlasRetainedView: NSViewRepresentable {
    let tiles: [AtlasTextTile]
    let revision: UUID
    let scale: Double
    let offset: CGPoint
    let selectedPath: String?
    let hitPaths: Set<String>
    var matchRows: [CGRect] = []
    var onPresentedFrame: ((UUID, Double, CGPoint) -> Void)? = nil

    @MainActor final class Coordinator {
        private var sourceTiles: [AtlasTextTile] = []
        private var sourceMutationEpoch: UInt64 = 0
        private var revision: UUID?
        private var scale = 0.0
        private var offset = CGPoint.zero
        private var selectedPath: String?
        private var hitPaths: Set<String> = []
        private var matchRows: [CGRect] = []
        private var size = CGSize.zero
        private var backing = 0.0
        private weak var window: NSWindow?
        private var metalFailures = 0

        func matches(_ input: AtlasRetainedView, view: AtlasRetainedSurface) -> Bool {
            guard let window = view.window, view.metal != nil,
                  view.bounds.width > 0, view.bounds.height > 0,
                  self.window === window,
                  revision == input.revision, scale == input.scale, offset == input.offset,
                  selectedPath == input.selectedPath, hitPaths == input.hitPaths,
                  matchRows == input.matchRows, size == view.bounds.size,
                  backing == Double(window.backingScaleFactor),
                  metalFailures == (view.metal?.failures ?? 0),
                  sourceMutationEpoch == AtlasTextTile.renderMutationEpoch,
                  sourceTiles.count == input.tiles.count else { return false }
            return sourceTiles.withUnsafeBufferPointer { old in
                input.tiles.withUnsafeBufferPointer { new in old.baseAddress == new.baseAddress }
            }
        }

        func record(_ input: AtlasRetainedView, view: AtlasRetainedSurface) {
            revision = input.revision; scale = input.scale; offset = input.offset
            selectedPath = input.selectedPath; hitPaths = input.hitPaths
            matchRows = input.matchRows; size = view.bounds.size
            backing = view.window.map { Double($0.backingScaleFactor) } ?? 0
            window = view.window; metalFailures = view.metal?.failures ?? 0
            sourceTiles = input.tiles
            sourceMutationEpoch = AtlasTextTile.renderMutationEpoch
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> AtlasRetainedSurface { AtlasRetainedSurface() }
    func updateNSView(_ view: AtlasRetainedSurface, context: Context) {
        view.onPresentedFrame = onPresentedFrame
        if context.coordinator.matches(self, view: view) { return }
        context.coordinator.record(self, view: view)
        view.update(tiles: tiles, revision: revision, scale: scale, offset: offset,
                    selectedPath: selectedPath, hitPaths: hitPaths, matchRows: matchRows)
    }
    static func dismantleNSView(_ view: AtlasRetainedSurface, coordinator: Coordinator) { view.stopClock() }
}

/// Counters are inspectable by native regression/profiling programs, not source logs.
struct AtlasRetainedMetrics {
    var installs = 0, overviewAssignments = 0, transformUpdates = 0
    var detailRasters = 0, detailHits = 0, detailEvictions = 0, detailFailures = 0
    var detailBytes = 0, peakDetailBytes = 0, detailPasses = 0
    var requestedPatches = 0, admissionLimitedQueries = 0
    var visibleRequestedPatches = 0, visibleRefusedPatches = 0, prefetchRequestedPatches = 0
    var visibleAllocationRefusals = 0, prefetchAllocationRefusals = 0
    var captionRasters = 0, captionBytes = 0, peakCaptionBytes = 0, visibleCaptions = 0
    var captionEvictions = 0, captionFailures = 0, captionFrameUpdates = 0, requestedCaptions = 0
    var detailSeconds = 0.0, maximumPassSeconds = 0.0
    var transitionRasters = 0, transitionBytes = 0, transitionFailures = 0
    var transitionSeconds = 0.0
}

final class AtlasRetainedSurface: NSView {
    private struct PatchKey: Hashable { let tile: Int; let level: Int; let x: Int; let y: Int }
    private struct Patch { let layer: CALayer; let image: CGImage; let rect: CGRect; let bytes: Int; var used: UInt64 }
    private struct Request { let key: PatchKey; let rect: CGRect }
    private struct CaptionKey: Hashable { let path: String; let backing: Int }
    private struct Caption { let layer: CALayer; let image: CGImage; let bytes: Int; let logicalWidth: Double; var used: UInt64 }
    private struct Cell: Hashable { let x: Int; let y: Int }
    // CPU detail residency plus one bitmap/image overlap stays below 512 MiB.
    // Allocate on demand: never trade text resolution for a smaller cache.
    // Core Animation's private GPU allocations are measured separately.
    static let residentByteLimit = 512 * 1024 * 1024 - 512 * 1024
    static let maximumRequests = 4096
    static let maximumResidentPatches = 4096
    static let captionByteLimit = 512 * 1024 * 1024
    static let maximumVisibleCaptions = 4096
    private var captions: [CaptionKey: Caption] = [:]
    private var captionFrames: [CaptionKey: CGRect] = [:]
    private var pendingCaptions: [CaptionKey] = []
    private let world = CALayer()
    private let metalEnabled: Bool
    private(set) var metal: AtlasMetalPresentation?
    private var metalCoveredTiles: Set<Int> = []
    var onPresentedFrame: ((UUID, Double, CGPoint) -> Void)?
    var metalScene: AtlasMetalGlyphRenderer? { metal?.scene }
    var gpuCoveredTiles: Set<Int> { metalCoveredTiles }
    var gpuGlyphTiles: Set<Int> { metal?.coveredGlyphTiles ?? [] }
    var gpuRasterTiles: Set<Int> { metal?.coveredRasterTiles ?? [] }
    var metalSubmissions: Int { metal?.submissions ?? 0 }
    var metalPresentations: Int { metal?.presentations ?? 0 }
    var presentedMetalCamera: (revision: UUID, scale: Double, offset: CGPoint)? {
        metal?.acceptedFrame.map { ($0.revision, $0.scale, $0.offset) }
    }
    private let transition = CALayer()
    private var transitionImage: CGImage?
    private var transitionTiles: [Int] = []
    private let transitionWorker = DispatchQueue(label: "fcb.atlas.transition", qos: .userInitiated)
    private var transitionGeneration: UInt64 = 0
    private var transitionInFlight = false
    static let transitionByteLimit = 384 * 1024 * 1024
    private var matchLayers: [CALayer] = []
    private var matchRows: [CGRect] = []
    private var tiles: [AtlasTextTile] = []
    private var overview: [CALayer] = []
    private var spatial: [Cell: [Int]] = [:]
    private var largeTiles: [Int] = []
    private var outlines: [String: CALayer] = [:]
    private var outlineGutters: [String: Double] = [:]
    private var directories: [String: CAShapeLayer] = [:]
    private var directoryGutters: [String: Double] = [:]
    private var revision: UUID?
    private var patches: [PatchKey: Patch] = [:]
    private var pending: [Request] = []
    private var required: Set<PatchKey> = []
    private var visibleRequired: Set<PatchKey> = []
    private var sharpFallbackRequired: Set<PatchKey> = []
    private var adequateOverview: Set<Int> = []
    private var level = -1
    private var backingScale = 1.0
    private var scale = 1.0
    private var offset = CGPoint.zero
    private var serial: UInt64 = 0
    private var clock: CADisplayLink?
    private lazy var clockTarget = ClockTarget(self)
    private var selected: String?
    private var hits: Set<String> = []
    private var lastViewport = CGRect.null
    private var lastLevel = Int.min
    private var lastDesiredPixelsPerWorld = 0.0
    private(set) var metrics = AtlasRetainedMetrics()
    var hasPendingCPUDetail: Bool { !pending.isEmpty || !pendingCaptions.isEmpty }
    var isCPUDetailClockActive: Bool { clock != nil }
    var hasPendingDetail: Bool { hasPendingCPUDetail || (metal?.hasWork ?? false) }
    // Diagnostic snapshots expose real retained images to the independent native
    // oracle. They do not render, upload, read source, or mutate cache state.
    var retainedDetailImages: [(tile: Int, rect: CGRect, level: Int, image: CGImage, visible: Bool, currentTier: Bool)] {
        patches.map { (tile: $0.key.tile, rect: $0.value.rect, level: $0.key.level, image: $0.value.image, visible: !$0.value.layer.isHidden, currentTier: required.contains($0.key)) }
    }
    var retainedOverviewImages: [(tile: Int, rect: CGRect, image: CGImage, visible: Bool)] {
        overview.enumerated().compactMap { index, retained in
            guard let image = tiles[index].raster, let contents = retained.contents,
                  (contents as AnyObject) === image else { return nil }
            return (tile: index, rect: retained.frame, image: image, visible: !retained.isHidden)
        }
    }
    var retainedTransitionImage: (rect: CGRect, image: CGImage, tiles: [Int])? {
        guard !transition.isHidden, let image = transitionImage else { return nil }
        return (transition.frame, image, transitionTiles)
    }
    var retainedCaptionImages: [(path: String, backing: Int, frame: CGRect, image: CGImage, visible: Bool)] {
        captions.map { (path: $0.key.path, backing: $0.key.backing, frame: $0.value.layer.frame,
                        image: $0.value.image, visible: !$0.value.layer.isHidden) }
    }
    var retainedDirectoryRects: [(path: String, rect: CGRect)] { directories.map { ($0.key, $0.value.frame) } }
    var retainedDirectoryColors: [String: CGColor] { directories.compactMapValues(\.strokeColor) }
    var retainedDirectoryDrawnPaths: Set<String> {
        Set(directories.filter { !$0.value.isHidden && $0.value.path != nil &&
            $0.value.strokeColor != nil && $0.value.lineWidth > 0 }.keys)
    }
    var retainedMatchRects: [CGRect] { matchLayers.map(\.frame) }
    override var isFlipped: Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    init(metalEnabled: Bool = true,
         metalRenderingMode: AtlasMetalPresentation.RenderingMode = .retainedCoverage) {
        self.metalEnabled = metalEnabled
        super.init(frame: .zero)
        wantsLayer = true
        layer?.masksToBounds = true
        layer?.backgroundColor = Monokai.background.cgColor
        world.anchorPoint = .zero
        world.position = .zero
        if metalEnabled {
            metal = AtlasMetalPresentation(renderingMode: metalRenderingMode)
            if let metal {
                layer?.addSublayer(metal.layer)
                metal.accept = { [weak self] frame, covered in
                    guard let self else { return }
                    if self.metalCoveredTiles != covered {
                        self.lastLevel = Int.min
                        if !covered.isDisjoint(with: self.transitionTiles) { self.clearTransition() }
                    }
                    self.metalCoveredTiles = covered
                    self.applyFrame(tiles: frame.tiles, revision: frame.revision, scale: frame.scale,
                        offset: frame.offset, selectedPath: frame.selectedPath,
                        hitPaths: frame.hitPaths, matchRows: frame.matchRows)
                }
            }
        }
        world.zPosition = 1
        layer?.addSublayer(world)
        transition.anchorPoint = .zero
        transition.zPosition = 1.5
        transition.isHidden = true
        world.addSublayer(transition)
    }
    required init?(coder: NSCoder) { nil }

    private final class ClockTarget: NSObject {
        weak var surface: AtlasRetainedSurface?
        init(_ surface: AtlasRetainedSurface) { self.surface = surface }
        @objc func step(_ link: CADisplayLink) { surface?.advanceMetalFrame(); surface?.prepareDetailPass() }
    }
    func stopClock() { clock?.invalidate(); clock = nil }
    private func startClock() {
        guard window != nil, clock == nil, hasPendingCPUDetail else { return }
        let link = displayLink(target: clockTarget, selector: #selector(ClockTarget.step(_:)))
        link.add(to: .main, forMode: .common)
        clock = link
    }
    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if window == nil {
            metal?.invalidatePresentation()
            metalCoveredTiles = []; lastLevel = Int.min
            refreshDetail(); stopClock()
        } else {
            setBackingScale(Double(window?.backingScaleFactor ?? 1))
            if let revision {
                update(tiles: tiles, revision: revision, scale: scale, offset: offset,
                    selectedPath: selected, hitPaths: hits, matchRows: matchRows)
            }
            refreshDetail(); startClock()
        }
    }
    override func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        setBackingScale(Double(window?.backingScaleFactor ?? 1))
    }
    func setBackingScale(_ value: Double) {
        guard value.isFinite, value > 0, value <= 8 else { return }
        backingScale = value; lastLevel = Int.min
        metal?.resize(size: bounds.size, backing: backingScale)
        refreshDetail(); startClock()
    }
    override func layout() {
        super.layout()
        metal?.resize(size: bounds.size, backing: backingScale)
        refreshDetail(); startClock()
    }

    func update(tiles next: [AtlasTextTile], revision nextRevision: UUID, scale: Double,
                offset: CGPoint, selectedPath: String?, hitPaths: Set<String>, matchRows: [CGRect] = []) {
        guard scale.isFinite, scale > 0, offset.x.isFinite, offset.y.isFinite else { return }
        if let metal, window != nil, bounds.width > 0, bounds.height > 0 {
            metal.prepare(tiles: next, revision: nextRevision)
            metal.request(.init(tiles: next, revision: nextRevision, scale: scale, offset: offset,
                selectedPath: selectedPath, hitPaths: hitPaths, matchRows: matchRows,
                size: bounds.size, backing: backingScale))
            startClock()
            return
        }
        metal?.invalidatePresentation()
        if !metalCoveredTiles.isEmpty { lastLevel = Int.min }
        metalCoveredTiles = []
        applyFrame(tiles: next, revision: nextRevision, scale: scale, offset: offset,
                   selectedPath: selectedPath, hitPaths: hitPaths, matchRows: matchRows)
    }

    func advanceMetalFrame() {
        metal?.advance(size: bounds.size, backing: backingScale)
    }

    private func applyFrame(tiles next: [AtlasTextTile], revision nextRevision: UUID, scale: Double,
                offset: CGPoint, selectedPath: String?, hitPaths: Set<String>, matchRows: [CGRect]) {
        CATransaction.begin(); CATransaction.setDisableActions(true)
        if revision != nextRevision {
            transitionGeneration &+= 1
            stopClock()
            world.sublayers = nil
            clearTransition()
            world.addSublayer(transition)
            for caption in captions.values { caption.layer.removeFromSuperlayer() }
            captions.removeAll(keepingCapacity: true); captionFrames.removeAll(keepingCapacity: true)
            pendingCaptions.removeAll(keepingCapacity: true); metrics.captionBytes = 0
            tiles = next; revision = nextRevision
            overview.removeAll(keepingCapacity: true); spatial.removeAll(keepingCapacity: true)
            outlines.removeAll(keepingCapacity: true); outlineGutters.removeAll(keepingCapacity: true)
            directories.removeAll(keepingCapacity: true); directoryGutters.removeAll(keepingCapacity: true)
            largeTiles.removeAll(keepingCapacity: true)
            for highlight in matchLayers { highlight.removeFromSuperlayer() }
            matchLayers.removeAll(keepingCapacity: true); self.matchRows = []
            patches.removeAll(keepingCapacity: true); pending.removeAll(keepingCapacity: true)
            required.removeAll(keepingCapacity: true); metrics.detailBytes = 0
            selected = nil; hits = []; lastViewport = .null
            for (index, tile) in tiles.enumerated() {
                let image = CALayer()
                image.anchorPoint = .zero
                image.frame = tile.rect
                image.contents = tile.raster
                image.contentsGravity = .resize
                image.minificationFilter = .linear
                image.magnificationFilter = .linear
                if let parcel = tile.parcelRect {
                    let gutter = Self.contentGutter(tile.rect, inside: parcel)
                    outlineGutters[tile.path] = min(outlineGutters[tile.path] ?? gutter, gutter)
                }
                if tile.parcelFirst, let parcel = tile.parcelRect {
                    let outline = CALayer()
                    outline.frame = parcel; outline.zPosition = 2
                    outline.borderWidth = 0
                    outline.borderColor = Self.outlineColor(tile.path)
                    world.addSublayer(outline); outlines[tile.path] = outline
                }
                world.addSublayer(image); overview.append(image)
                metrics.overviewAssignments += 1
                // Tiles are bounded source pages. Grid membership is built once.
                let rect = tile.rect
                if rect.width > 0, rect.height > 0,
                   [rect.minX, rect.minY, rect.maxX, rect.maxY].allSatisfy({ $0.isFinite && abs($0) < 1e12 }) {
                    if ceil(rect.width / 1024 + 1) * ceil(rect.height / 1024 + 1) > 64 {
                        largeTiles.append(index); continue
                    }
                    for y in Int(floor(rect.minY / 1024))...Int(floor(rect.maxY / 1024)) {
                        for x in Int(floor(rect.minX / 1024))...Int(floor(rect.maxX / 1024)) {
                            spatial[Cell(x: x, y: y), default: []].append(index)
                        }
                    }
                }
            }
            // The Rust treemap packs each directory contiguously. The union of
            // its descendant leaf parcels recovers its exact directory rectangle;
            // this is a projection of that layout, not a second layout algorithm.
            var directoryRects: [String: CGRect] = [:]
            for tile in tiles where tile.parcelFirst {
                guard let parcel = tile.parcelRect else { continue }
                let gutter = outlineGutters[tile.path] ?? 0
                var directory = (tile.path as NSString).deletingLastPathComponent
                while !directory.isEmpty && directory != "." && directory != "/" {
                    directoryRects[directory] = directoryRects[directory].map { $0.union(parcel) } ?? parcel
                    directoryGutters[directory] = min(directoryGutters[directory] ?? gutter, gutter)
                    directory = (directory as NSString).deletingLastPathComponent
                }
            }
            for (path, rect) in directoryRects {
                let boundary = CAShapeLayer()
                boundary.anchorPoint = .zero
                boundary.frame = rect; boundary.zPosition = 2.2
                boundary.strokeColor = Self.directoryColor(path)
                boundary.fillColor = nil
                boundary.lineJoin = .round
                world.addSublayer(boundary); directories[path] = boundary
            }
            metrics.installs += 1
        }
        self.scale = scale; self.offset = offset
        world.setAffineTransform(CGAffineTransform(a: scale, b: 0, c: 0, d: scale, tx: offset.x, ty: offset.y))
        metrics.transformUpdates += 1
        // Small retained bands move with the same world transform as source.
        // No source lookup or rasterization occurs on camera ticks.
        if self.matchRows != matchRows {
            for highlight in matchLayers { highlight.removeFromSuperlayer() }
            matchLayers.removeAll(keepingCapacity: true)
            self.matchRows = Array(matchRows.prefix(256))
            for rect in self.matchRows where rect.minX.isFinite && rect.minY.isFinite &&
                rect.width.isFinite && rect.height.isFinite && rect.width > 0 && rect.height > 0 {
                let highlight = CALayer()
                highlight.frame = rect
                highlight.backgroundColor = NSColor(srgbRed: 0.95, green: 0.79, blue: 0.24, alpha: 0.22).cgColor
                highlight.zPosition = 2.5
                world.addSublayer(highlight); matchLayers.append(highlight)
            }
        }
        if selected != selectedPath || hits != hitPaths {
            selected = selectedPath; hits = hitPaths
            for (path, outline) in outlines {
                let marked = path == selectedPath || hitPaths.contains(path)
                outline.borderWidth = Self.safeStrokeWidth(marked ? 1.5 : 0.85,
                    scale: scale, gutter: outlineGutters[path] ?? 0)
                outline.borderColor = marked ? (path == selectedPath ? NSColor.yellow : NSColor.orange).cgColor
                    : Self.outlineColor(path)
            }
        }
        CATransaction.commit()
        refreshDetail()
        onPresentedFrame?(nextRevision, scale, offset)
    }

    private func refreshDetail() {
        guard bounds.width > 0, bounds.height > 0, scale > 0 else { return }
        let desired = scale * backingScale
        let viewport = CGRect(x: -offset.x / scale, y: -offset.y / scale,
                              width: bounds.width / scale, height: bounds.height / scale)
        guard [viewport.minX, viewport.minY, viewport.maxX, viewport.maxY].allSatisfy({ $0.isFinite && abs($0) < 1e12 }) else { stopClock(); return }
        // An unchanged camera reuses exact retained images without raster work.
        guard viewport != lastViewport || desired != lastDesiredPixelsPerWorld || lastLevel == Int.min else { return }
        // Never magnify detail above its captured pixel density. Downward-only
        // hysteresis keeps sharper images during small wheel reversals. Prepare
        // the next tier ahead of a wheel gesture's bounded 12.8% queued zoom.
        let preparedDensity = desired * 1.15
        if level == -1 || preparedDensity > pow(2, Double(level)) || preparedDensity < pow(2, Double(level)) * 0.45 {
            level = max(-16, min(16, Int(ceil(log2(min(65536, max(1.0 / 65536, preparedDensity)))))))
        }
        let strokesChanged = lastViewport.isNull || desired != lastDesiredPixelsPerWorld || lastLevel == Int.min
        lastViewport = viewport; lastLevel = level; lastDesiredPixelsPerWorld = desired
        transitionGeneration &+= 1
        serial &+= 1
        pending.removeAll(keepingCapacity: true); required.removeAll(keepingCapacity: true)
        visibleRequired.removeAll(keepingCapacity: true)
        sharpFallbackRequired.removeAll(keepingCapacity: true)
        adequateOverview.removeAll(keepingCapacity: true)
        adequateOverview.formUnion(metalCoveredTiles)
        for (index, image) in overview.enumerated() {
            image.isHidden = metalCoveredTiles.contains(index)
        }
        metrics.visibleRequestedPatches = 0; metrics.visibleRefusedPatches = 0; metrics.prefetchRequestedPatches = 0
        var visible = Set(largeTiles)
        let cells = ceil(viewport.width / 1024 + 1) * ceil(viewport.height / 1024 + 1)
        if cells > Double(min(4096, tiles.count)) {
            // At overview scale, visiting every occupied tile is cheaper than
            // querying thousands of empty cells. Never hide content on this path.
            let side = 256 / pow(2, Double(level))
            let region = viewport.insetBy(dx: -side, dy: -side)
            visible = Set(tiles.indices.filter { tiles[$0].rect.intersects(region) })
        } else {
            for y in Int(floor(viewport.minY / 1024))...Int(floor(viewport.maxY / 1024)) {
                for x in Int(floor(viewport.minX / 1024))...Int(floor(viewport.maxX / 1024)) {
                    for index in spatial[Cell(x: x, y: y)] ?? [] { visible.insert(index) }
                }
            }
        }
        CATransaction.begin(); CATransaction.setDisableActions(true)
        var bordered: Set<String> = []
        for index in visible {
            let tile = tiles[index]
            if bordered.insert(tile.path).inserted, let outline = outlines[tile.path] {
                outline.borderWidth = Self.safeStrokeWidth(
                    tile.path == selected || hits.contains(tile.path) ? 1.5 : 0.85,
                    scale: scale, gutter: outlineGutters[tile.path] ?? 0)
            }
        }
        for (path, boundary) in directories {
            let hidden = !boundary.frame.intersects(viewport) ||
                min(boundary.frame.width, boundary.frame.height) * scale < 12
            // Hidden geometry is unobservable. Refresh it before re-entry even
            // when a pure pan leaves the zoom and stroke density unchanged.
            if !hidden && (strokesChanged || boundary.isHidden) {
                let depth = path.split(separator: "/").count
                let gutter = directoryGutters[path] ?? 0
                let width = Self.safeStrokeWidth(depth <= 2 ? 2.0 : 1.25,
                    scale: scale, gutter: gutter)
                // Directory bounds are unions of file parcels. A screen-space
                // inset grows into source text during zoom-out; keep the entire
                // stroke inside the actual text-free parcel gutter instead.
                let inset = min((1 + Double(depth) * 1.75) / scale,
                    max(0, gutter - width),
                    min(boundary.frame.width, boundary.frame.height) * 0.25)
                boundary.path = CGPath(rect: CGRect(origin: .zero, size: boundary.frame.size)
                    .insetBy(dx: inset, dy: inset), transform: nil)
                boundary.lineWidth = width
            }
            boundary.isHidden = hidden
        }
        CATransaction.commit()
        var requests: [Request] = []
        // Center-first admission bounds work even for very large displays.
        let center = CGPoint(x: viewport.midX, y: viewport.midY)
        var ranked: [(index: Int, distance: Double)] = []
        ranked.reserveCapacity(visible.count)
        for index in visible {
            let rect = tiles[index].rect
            let dx = Double(rect.midX - center.x)
            let dy = Double(rect.midY - center.y)
            ranked.append((index: index, distance: dx * dx + dy * dy))
        }
        ranked.sort {
            if $0.distance == $1.distance { return $0.index < $1.index }
            return $0.distance < $1.distance
        }
        let ordered = ranked.map { $0.index }
        refreshCaptions()
        // A persisted complete-source bitmap often already exceeds display
        // density at overview scale. Reuse those exact cached pixels instead of
        // spending hundreds of display callbacks drawing the same glyphs again.
        for index in ordered {
            let rect = tiles[index].rect
            if let image = tiles[index].raster, rect.width > 0, rect.height > 0,
               Double(image.width) / rect.width >= preparedDensity,
               Double(image.height) / rect.height >= preparedDensity {
                adequateOverview.insert(index)
            }
        }
        // Finish the entire visible pass before considering the one-patch
        // prefetch border. A central file cannot spend another file's coverage.
        // Budget and evict retained images rather than reducing text density.
        do {
            let pixelsPerWorld = pow(2, Double(level))
            let side = 256 / pixelsPerWorld
            requests.removeAll(keepingCapacity: true)
            visibleRequired.removeAll(keepingCapacity: true)
            metrics.visibleRequestedPatches = 0; metrics.visibleRefusedPatches = 0
            metrics.prefetchRequestedPatches = 0
            for prefetch in [false, true] {
                for index in ordered {
                    if prefetch && requests.count >= Self.maximumRequests { break }
                    if adequateOverview.contains(index) { continue }
                    let tileRect = tiles[index].rect
                    let region = prefetch ? viewport.insetBy(dx: -side, dy: -side) : viewport
                    let intersection = tileRect.intersection(region)
                    guard !intersection.isNull, intersection.width > 0, intersection.height > 0 else { continue }
                    let local = intersection.offsetBy(dx: -tileRect.minX, dy: -tileRect.minY)
                    let firstX = max(0, Int(floor(local.minX / side)))
                    let firstY = max(0, Int(floor(local.minY / side)))
                    let endX = max(firstX, Int(ceil(local.maxX / side)) - 1)
                    let endY = max(firstY, Int(ceil(local.maxY / side)) - 1)
                    for y in firstY...endY {
                        for x in firstX...endX {
                            let rect = CGRect(x: Double(x) * side, y: Double(y) * side, width: side, height: side)
                                .intersection(CGRect(origin: .zero, size: tileRect.size))
                            guard rect.width > 0, rect.height > 0 else { continue }
                            let overlap = rect.offsetBy(dx: tileRect.minX, dy: tileRect.minY).intersection(viewport)
                            let onScreen = !overlap.isNull && overlap.width > 0 && overlap.height > 0
                            guard prefetch != onScreen else { continue }
                            if !prefetch {
                                metrics.visibleRequestedPatches += 1
                            }
                            guard requests.count < Self.maximumRequests else {
                                if !prefetch { metrics.visibleRefusedPatches += 1 }
                                continue
                            }
                            let key = PatchKey(tile: index, level: level, x: x, y: y)
                            requests.append(Request(key: key, rect: rect))
                            if prefetch { metrics.prefetchRequestedPatches += 1 }
                            else { visibleRequired.insert(key) }
                        }
                        if prefetch && requests.count >= Self.maximumRequests { break }
                    }
                }
            }
        }
        lastLevel = level
        metrics.requestedPatches = requests.count
        if requests.count == Self.maximumRequests { metrics.admissionLimitedQueries += 1 }
        for request in requests {
            required.insert(request.key)
            if var patch = patches[request.key] {
                patch.used = serial; patches[request.key] = patch; metrics.detailHits += 1
            } else { pending.append(request) }
        }
        CATransaction.begin(); CATransaction.setDisableActions(true)
        updatePatchVisibility()
        prepareTransition(ordered: ordered, viewport: viewport)
        CATransaction.commit()
        if hasPendingCPUDetail { startClock() } else { stopClock() }
    }

    private func clearTransition() {
        transitionGeneration &+= 1
        transition.isHidden = true
        transition.contents = nil
        transitionImage = nil
        transitionTiles.removeAll(keepingCapacity: true)
        metrics.transitionBytes = 0
    }

    /// Cover only genuinely unresolved visible source. Unlike a magnified
    /// preview, this image is drawn at the exact physical display density.
    /// Each shaped row is replayed once per tile, rather than once per patch.
    private func prepareTransition(ordered: [Int], viewport: CGRect) {
        let start = CACurrentMediaTime()
        var missing: [Int] = []
        var covered: [Int: Double] = [:]
        for key in visibleRequired {
            guard let patch = patches[key] else { continue }
            let rect = patch.rect.offsetBy(dx: tiles[key.tile].rect.minX, dy: tiles[key.tile].rect.minY)
                .intersection(viewport)
            if !rect.isNull { covered[key.tile, default: 0] += rect.width * rect.height }
        }
        for index in ordered where !adequateOverview.contains(index) {
            let rect = tiles[index].rect.intersection(viewport)
            guard !rect.isNull, rect.width > 0, rect.height > 0 else { continue }
            if (covered[index] ?? 0) < rect.width * rect.height * (1 - 1e-9) { missing.append(index) }
        }
        if !missing.isEmpty {
            // Zooming out can select a coarser patch grid while a complete,
            // sharper grid is still visible underneath it. Count each retained
            // level separately: its cells are disjoint, but different levels
            // overlap and must never have their areas added together.
            let unresolved = Set(missing)
            let density = scale * backingScale
            var fallbackCoverage: [Int: [Int: Double]] = [:]
            var fallbackKeys: [Int: [Int: [PatchKey]]] = [:]
            for (key, patch) in patches where unresolved.contains(key.tile) &&
                !required.contains(key) && !patch.layer.isHidden {
                guard Double(patch.image.width) / patch.rect.width >= density,
                      Double(patch.image.height) / patch.rect.height >= density else { continue }
                let rect = patch.rect.offsetBy(dx: tiles[key.tile].rect.minX,
                                              dy: tiles[key.tile].rect.minY).intersection(viewport)
                guard !rect.isNull, !rect.isEmpty else { continue }
                fallbackCoverage[key.tile, default: [:]][key.level, default: 0] += rect.width * rect.height
                fallbackKeys[key.tile, default: [:]][key.level, default: []].append(key)
            }
            missing.removeAll { index in
                let target = tiles[index].rect.intersection(viewport)
                let area = target.width * target.height * (1 - 1e-9)
                guard let level = fallbackCoverage[index]?.filter({ $0.value >= area }).keys.max(),
                      let keys = fallbackKeys[index]?[level] else { return false }
                sharpFallbackRequired.formUnion(keys)
                return true
            }
        }
        guard !missing.isEmpty else { clearTransition(); return }
        if let image = transitionImage, transition.frame.contains(viewport),
           Double(image.width) / transition.frame.width >= scale * backingScale,
           Double(image.height) / transition.frame.height >= scale * backingScale,
           Set(missing).isSubset(of: Set(transitionTiles)) {
            // Its world rectangle and physical resolution remain sufficient.
            // Let Core Animation move it with the camera without any glyph work.
            return
        }
        let canvas = viewport.insetBy(dx: -32 / scale, dy: -32 / scale)
        let density = scale * backingScale * 1.15
        let physicalWidth = ceil(canvas.width * density) + 1
        let physicalHeight = ceil(canvas.height * density) + 1
        guard physicalWidth > 0, physicalHeight > 0,
              physicalWidth * physicalHeight * 12 <= Double(Self.transitionByteLimit) else {
            clearTransition(); metrics.transitionFailures += 1; return
        }
        let width = Int(physicalWidth), height = Int(physicalHeight)
        let background = Monokai.background.cgColor
        let draws = missing.map { index -> AtlasTransitionDraw in
            let tile = tiles[index]
            let clipped = tile.rect.intersection(canvas)
            let first = max(0, min(tile.lineCount,
                Int(floor(((clipped.minY - tile.rect.minY) / tile.contentScale - 38) / AtlasTextTile.lineHeight))))
            let end = max(first, min(tile.lineCount,
                Int(ceil(((clipped.maxY - tile.rect.minY) / tile.contentScale + 16) / AtlasTextTile.lineHeight))))
            return AtlasTransitionDraw(tile: tile, rect: tile.rect, contentScale: tile.contentScale,
                                       clipped: clipped, rows: first..<end)
        }
        if metal != nil && window != nil {
            guard !transitionInFlight else { return }
            transitionInFlight = true
            let generation = transitionGeneration
            let paintedTiles = missing
            transitionWorker.async { [weak self] in
                let image = renderAtlasTransition(canvas: canvas, width: width, height: height,
                                                  background: background, draws: draws)
                DispatchQueue.main.async { [weak self] in
                    guard let self else { return }
                    self.transitionInFlight = false
                    guard self.transitionGeneration == generation else {
                        self.lastLevel = Int.min
                        self.refreshDetail()
                        return
                    }
                    guard let image else { self.metrics.transitionFailures += 1; return }
                    CATransaction.begin(); CATransaction.setDisableActions(true)
                    self.transition.frame = canvas
                    self.transition.contents = image
                    self.transition.contentsGravity = .resize
                    self.transition.isHidden = false
                    CATransaction.commit()
                    self.transitionImage = image; self.transitionTiles = paintedTiles
                    self.metrics.transitionRasters += 1
                    self.metrics.transitionBytes = width * height * 4
                    self.metrics.transitionSeconds += CACurrentMediaTime() - start
                }
            }
            return
        }
        guard let image = renderAtlasTransition(canvas: canvas, width: width, height: height,
                                                background: background, draws: draws) else {
            clearTransition(); metrics.transitionFailures += 1; return
        }
        transition.frame = canvas
        transition.contents = image
        transition.contentsGravity = .resize
        transition.isHidden = false
        transitionImage = image; transitionTiles = missing
        metrics.transitionRasters += 1
        metrics.transitionBytes = width * height * 4
        metrics.transitionSeconds += CACurrentMediaTime() - start
    }

    private static func outlineColor(_ path: String) -> CGColor {
        directoryColor((path as NSString).deletingLastPathComponent).copy(alpha: 0.65) ?? NSColor.gray.cgColor
    }

    private static func contentGutter(_ content: CGRect, inside parcel: CGRect) -> Double {
        guard [content.minX, content.minY, content.maxX, content.maxY,
               parcel.minX, parcel.minY, parcel.maxX, parcel.maxY].allSatisfy(\.isFinite) else { return 0 }
        return max(0, min(content.minX - parcel.minX, content.minY - parcel.minY,
                          parcel.maxX - content.maxX, parcel.maxY - content.maxY))
    }

    private static func safeStrokeWidth(_ screenPoints: Double, scale: Double, gutter: Double) -> Double {
        max(0, min(screenPoints / scale, gutter * 0.75))
    }

    private static func directoryColor(_ path: String) -> CGColor {
        // Stable across project opens and file additions; Swift Hasher is seeded.
        let hash = path.utf8.reduce(UInt64(14695981039346656037)) { ($0 ^ UInt64($1)) &* 1099511628211 }
        return NSColor(calibratedHue: CGFloat(hash % 360) / 360,
                       saturation: 0.48, brightness: 0.90, alpha: 0.95).cgColor
    }

    private func refreshCaptions() {
        captionFrames.removeAll(keepingCapacity: true); pendingCaptions.removeAll(keepingCapacity: true)
        let backing = max(1, min(4, Int(ceil(backingScale))))
        for (path, outline) in outlines {
            let parcel = outline.frame
            let key = CaptionKey(path: path, backing: backing)
            guard captionFrames[key] == nil else { continue }
            let screen = CGRect(x: parcel.minX * scale + offset.x, y: parcel.minY * scale + offset.y,
                                width: parcel.width * scale, height: parcel.height * scale)
            let visible = screen.intersection(bounds)
            guard !visible.isNull, visible.width >= 48, visible.height >= 24 else { continue }
            // Keep the filename attached to its visible parcel during panning,
            // including when its original top/left edge is outside the viewport.
            captionFrames[key] = CGRect(x: visible.minX + 1, y: visible.minY + 1,
                                       width: min(320, visible.width - 2), height: 22)
            if captionFrames.count == Self.maximumVisibleCaptions { break }
        }
        CATransaction.begin(); CATransaction.setDisableActions(true)
        for key in Array(captions.keys) {
            guard var caption = captions[key] else { continue }
            if let frame = captionFrames[key] {
                caption.layer.frame = CGRect(origin: frame.origin, size: CGSize(width: min(frame.width, caption.logicalWidth), height: frame.height))
                caption.layer.contentsRect = CGRect(x: 0, y: 0, width: min(frame.width, caption.logicalWidth) / caption.logicalWidth, height: 1)
                caption.layer.isHidden = false; caption.used = serial
                metrics.captionFrameUpdates += 1
            } else { caption.layer.isHidden = true }
            captions[key] = caption
        }
        CATransaction.commit()
        for key in captionFrames.keys where captions[key] == nil { pendingCaptions.append(key) }
        metrics.requestedCaptions = captionFrames.count
        metrics.visibleCaptions = captions.values.filter { !$0.layer.isHidden }.count
    }

    /// One caption attempt shares the existing display-pass deadline/attempt cap.
    /// Filename-sized images are cropped at parcel edges, never shrunk to fit.
    private func prepareCaption() {
        let key = pendingCaptions.removeFirst()
        guard let frame = captionFrames[key] else { return }
        let line = CTLineCreateWithAttributedString(NSAttributedString(string: (key.path as NSString).lastPathComponent,
            attributes: [.font: NSFont.monospacedSystemFont(ofSize: 13, weight: .medium), .foregroundColor: NSColor.white]))
        let logicalWidth = min(320, max(1, ceil(CTLineGetTypographicBounds(line, nil, nil, nil)) + 12))
        let width = Int(logicalWidth) * key.backing, height = 22 * key.backing
        let bytes = width * height * 4
        // Reserve 512KiB for the worst caption bitmap/image overlap at 4×.
        while metrics.captionBytes + bytes > Self.captionByteLimit - 512 * 1024 || captions.count >= Self.maximumVisibleCaptions * 2 {
            guard let victim = captions.filter({ captionFrames[$0.key] == nil }).min(by: { $0.value.used < $1.value.used }) else { break }
            victim.value.layer.removeFromSuperlayer(); metrics.captionBytes -= victim.value.bytes
            captions.removeValue(forKey: victim.key); metrics.captionEvictions += 1
        }
        guard metrics.captionBytes + bytes <= Self.captionByteLimit - 512 * 1024, captions.count < Self.maximumVisibleCaptions * 2,
              let bitmap = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
                  bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { metrics.captionFailures += 1; return }
        bitmap.setFillColor(Monokai.background.withAlphaComponent(0.94).cgColor)
        bitmap.fill(CGRect(x: 0, y: 0, width: width, height: height))
        bitmap.translateBy(x: 0, y: Double(height)); bitmap.scaleBy(x: Double(key.backing), y: -Double(key.backing))

        bitmap.textMatrix = CGAffineTransform(scaleX: 1, y: -1)
        bitmap.textPosition = CGPoint(x: 6, y: 16); CTLineDraw(line, bitmap)
        guard let image = bitmap.makeImage() else { metrics.captionFailures += 1; return }
        let caption = CALayer()
        caption.frame = CGRect(origin: frame.origin, size: CGSize(width: min(frame.width, logicalWidth), height: frame.height)); caption.contents = image; caption.contentsGravity = .resize
        caption.contentsRect = CGRect(x: 0, y: 0, width: min(frame.width, logicalWidth) / logicalWidth, height: 1)
        caption.zPosition = 3; layer?.addSublayer(caption)
        captions[key] = Caption(layer: caption, image: image, bytes: bytes, logicalWidth: logicalWidth, used: serial)
        metrics.captionBytes += bytes; metrics.peakCaptionBytes = max(metrics.peakCaptionBytes, metrics.captionBytes)
        metrics.captionRasters += 1
        metrics.visibleCaptions = captions.values.filter { !$0.layer.isHidden }.count
    }

    /// Preserve previous detail while the new tier fills in. Current-tier
    /// opaque patches sit above all fallback tiers. Their grid rectangles are
    /// disjoint, so summed intersection area establishes complete replacement.
    private func updatePatchVisibility() {
        var coverage: [Int: [CGRect]] = [:]
        for key in required {
            if let patch = patches[key] { coverage[key.tile, default: []].append(patch.rect) }
        }
        for key in Array(patches.keys) {
            guard var patch = patches[key] else { continue }
            if adequateOverview.contains(key.tile) {
                if !patch.layer.isHidden { patch.layer.isHidden = true }
                continue
            }
            if required.contains(key) {
                // Most frames only transform the world. Do not dirty thousands
                // of already-visible layers or rewrite their cache entries.
                if patch.layer.isHidden { patch.layer.isHidden = false }
                if patch.layer.zPosition != 1 { patch.layer.zPosition = 1 }
                continue
            } else {
                let tileRect = tiles[key.tile].rect
                let worldRect = patch.rect.offsetBy(dx: tileRect.minX, dy: tileRect.minY)
                let visibleWorld = worldRect.intersection(lastViewport)
                let hidden: Bool
                if visibleWorld.isNull || visibleWorld.isEmpty {
                    hidden = true
                } else {
                    let visibleLocal = visibleWorld.offsetBy(dx: -tileRect.minX, dy: -tileRect.minY)
                    let covered = (coverage[key.tile] ?? []).reduce(0.0) { area, rect in
                        let overlap = visibleLocal.intersection(rect)
                        return area + (overlap.isNull ? 0 : overlap.width * overlap.height)
                    }
                    hidden = covered >= visibleWorld.width * visibleWorld.height * (1 - 1e-9)
                }
                if patch.layer.isHidden != hidden { patch.layer.isHidden = hidden }
                // Higher-resolution fallback wins overlaps, but remains below
                // every current-tier image, including when zooming out.
                let order = 0.25 + Double(key.level + 16) * 0.01
                if patch.layer.zPosition != order { patch.layer.zPosition = order }
                if !hidden && patch.used != serial {
                    patch.used = serial
                    patches[key] = patch
                }
            }
        }
    }

    /// Native display lifecycle only: at most sixteen 256² images and a 2ms soft
    /// deadline per pass. A single CoreText call cannot be preempted; record it.
    func prepareDetailPass() {
        let start = CACurrentMediaTime()
        var made = 0
        CATransaction.begin(); CATransaction.setDisableActions(true)
        while !pendingCaptions.isEmpty, made < 4, CACurrentMediaTime() - start < 0.002 {
            prepareCaption(); made += 1
        }
        while !pending.isEmpty, made < 16, CACurrentMediaTime() - start < 0.002 {
            let request = pending.removeFirst()
            made += 1
            let ratio = pow(2, Double(request.key.level))
            let width = min(256, max(1, Int(ceil(request.rect.width * ratio))))
            let height = min(256, max(1, Int(ceil(request.rect.height * ratio))))
            let bytes = width * height * 4
            while metrics.detailBytes + bytes > Self.residentByteLimit || patches.count >= Self.maximumResidentPatches {
                guard let victim = patches.filter({ !sharpFallbackRequired.contains($0.key) && (visibleRequired.contains(request.key) ? !visibleRequired.contains($0.key) : !required.contains($0.key)) }).min(by: { $0.value.used < $1.value.used }) else { break }
                victim.value.layer.removeFromSuperlayer(); metrics.detailBytes -= victim.value.bytes
                patches.removeValue(forKey: victim.key); metrics.detailEvictions += 1
            }
            guard metrics.detailBytes + bytes <= Self.residentByteLimit, patches.count < Self.maximumResidentPatches else {
                if visibleRequired.contains(request.key) { metrics.visibleAllocationRefusals += 1 }
                else { metrics.prefetchAllocationRefusals += 1 }
                continue
            }
            let tile = tiles[request.key.tile]
            guard let bitmap = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
                bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { metrics.detailFailures += 1; continue }
            bitmap.translateBy(x: 0, y: Double(height))
            bitmap.scaleBy(x: Double(width) / request.rect.width, y: -Double(height) / request.rect.height)
            bitmap.translateBy(x: -request.rect.minX, y: -request.rect.minY)
            bitmap.scaleBy(x: tile.contentScale, y: tile.contentScale)
            let nativeRect = CGRect(x: request.rect.minX / tile.contentScale, y: request.rect.minY / tile.contentScale,
                                    width: request.rect.width / tile.contentScale, height: request.rect.height / tile.contentScale)
            // Full already-shaped lines are clipped, never reshaped substrings.
            let first = max(0, min(tile.lineCount, Int(floor((nativeRect.minY - 38) / AtlasTextTile.lineHeight))))
            let end = max(first, min(tile.lineCount, Int(ceil((nativeRect.maxY + 16) / AtlasTextTile.lineHeight))))
            tile.draw(in: bitmap, visibleRows: first..<end)
            guard let image = bitmap.makeImage() else { metrics.detailFailures += 1; continue }
            let detail = CALayer()
            detail.frame = request.rect.offsetBy(dx: tile.rect.minX, dy: tile.rect.minY)
            detail.contents = image; detail.contentsGravity = .resize
            detail.minificationFilter = .linear; detail.magnificationFilter = .linear
            // Opaque backdrop prevents low-resolution ink showing through detail.
            detail.backgroundColor = layer?.backgroundColor
            detail.zPosition = 1
            world.addSublayer(detail)
            patches[request.key] = Patch(layer: detail, image: image, rect: request.rect, bytes: bytes, used: serial)
            metrics.detailBytes += bytes
            metrics.peakDetailBytes = max(metrics.peakDetailBytes, metrics.detailBytes)
            metrics.detailRasters += 1
        }
        updatePatchVisibility()
        // A fallback relied upon for sharp coverage cannot be evicted until
        // opaque current detail hides it. Reclaim that protection after the
        // visibility pass proves the replacement, never merely after queuing it.
        sharpFallbackRequired = sharpFallbackRequired.filter { patches[$0]?.layer.isHidden == false }
        // The screen-density image remains valid while this camera is still;
        // retire it only after every visible retained patch is ready.
        if transitionImage != nil && visibleRequired.allSatisfy({ patches[$0] != nil }) &&
            metrics.visibleRefusedPatches == 0 && metrics.visibleAllocationRefusals == 0 {
            clearTransition()
        }
        CATransaction.commit()
        let elapsed = CACurrentMediaTime() - start
        metrics.detailPasses += 1; metrics.detailSeconds += elapsed
        metrics.maximumPassSeconds = max(metrics.maximumPassSeconds, elapsed)
        if !hasPendingCPUDetail { stopClock() }
    }
}
