// FrankenCodeBrowser — the zoomable code atlas (SpriteKit on Metal).
//
// Every file is an SKSpriteNode whose texture IS its real source text
// (Monokai, rastered once on a background queue) — the GPU composites
// the treemap every frame at display rate with zero per-frame CPU.
// Blob files (jsonl/json/huge) render as line-density bars instead:
// a visual summary, not a wall of raw JSON.
// Camera: steered wheel zoom (cursor-anchored, exponential approach),
// inertial pan, stepped in scene update at display rate.

import AppKit
import SpriteKit
import SwiftUI

// MARK: - Theme

enum ScopeTheme {
    static let background = NSColor(srgbRed: 0.102, green: 0.106, blue: 0.102, alpha: 1)
    static let cellFill = NSColor(srgbRed: 0.13, green: 0.15, blue: 0.14, alpha: 1)
    static let stroke = NSColor(srgbRed: 0.24, green: 0.84, blue: 0.77, alpha: 0.75)
    static let strokeSelected = NSColor(srgbRed: 1.0, green: 0.95, blue: 0.55, alpha: 1)
    static let label = NSColor(srgbRed: 0.88, green: 0.92, blue: 0.90, alpha: 0.95)
    static let barCode = NSColor(srgbRed: 0.32, green: 0.72, blue: 0.66, alpha: 0.95)
    static let barComment = NSColor(srgbRed: 0.42, green: 0.41, blue: 0.35, alpha: 0.9)
    static let barString = NSColor(srgbRed: 0.90, green: 0.80, blue: 0.35, alpha: 0.95)
    static let barKeyword = NSColor(srgbRed: 0.95, green: 0.30, blue: 0.50, alpha: 0.95)
}

private func lineRoleColor(_ role: UInt8) -> NSColor {
    switch role {
    case 1: return ScopeTheme.barComment
    case 2: return ScopeTheme.barString
    case 3: return ScopeTheme.barKeyword
    default: return ScopeTheme.barCode
    }
}

private func lineRoleKind(_ line: Substring) -> UInt8 {
    let trimmed = line.drop { $0 == " " || $0 == "\t" }
    if trimmed.hasPrefix("//") || trimmed.hasPrefix("#") { return 1 }
    if line.contains("\"") || line.contains("'") { return 2 }
    let keywordish = ["fn ", "func ", "def ", "class ", "struct ", "impl ", "pub ", "if ", "for ", "while ", "return "]
    return keywordish.contains { line.contains($0) } ? 3 : 0
}

// MARK: - File cell node

final class FileCellNode: SKNode {
    let path: String
    let worldRect: CGRect
    let bytes: Int
    let isBlob: Bool

    private(set) var texture: SKTexture?
    private(set) var textureBucket = 0
    private(set) var textTexture: SKTexture?
    private(set) var textBucket = 0
    private(set) var barTexture: SKTexture?
    private(set) var barBucket = 0
    var bakeInFlight = false
    var bakeFailed = false
    var bakedBytes = 0
    var lastUsedTick: UInt64 = 0
    var labelShown = false
    var textBaked = false
    var barsBaked = false

    private var tile: SKSpriteNode = SKSpriteNode(color: .clear, size: .zero)
    private var edges: [SKSpriteNode] = []
    private var textSprite: SKSpriteNode?
    private var barSprite: SKSpriteNode?
    private var labelNode: SKLabelNode?

    init(path: String, worldRect: CGRect, bytes: Int) {
        self.path = path
        self.worldRect = worldRect
        self.bytes = bytes
        let ext = (path as NSString).pathExtension.lowercased()
        isBlob = bytes > 200_000 || ext == "jsonl" || ext == "json" || ext == "xml"
        super.init()
        name = "cell"
        userData = ["path": path]

        tile = SKSpriteNode(color: ScopeTheme.cellFill, size: worldRect.size)
        tile.position = CGPoint(x: worldRect.midX, y: worldRect.midY)
        addChild(tile)

        let thickness: CGFloat = 1.5
        let edgeSpecs: [(CGPoint, CGSize)] = [
            (CGPoint(x: worldRect.midX, y: worldRect.maxY), CGSize(width: worldRect.width, height: thickness)),
            (CGPoint(x: worldRect.midX, y: worldRect.minY), CGSize(width: worldRect.width, height: thickness)),
            (CGPoint(x: worldRect.minX, y: worldRect.midY), CGSize(width: thickness, height: worldRect.height)),
            (CGPoint(x: worldRect.maxX, y: worldRect.midY), CGSize(width: thickness, height: worldRect.height)),
        ]
        let topDir = (path as NSString).pathComponents.dropFirst().first ?? "root"
        let hueHash = abs(topDir.unicodeScalars.reduce(0) { $0 &+ Int($1.value) }) % 5
        let strokeHue: NSColor = [ScopeTheme.stroke,
            NSColor(srgbRed: 0.72, green: 0.55, blue: 0.95, alpha: 0.75),
            NSColor(srgbRed: 0.95, green: 0.45, blue: 0.55, alpha: 0.75),
            NSColor(srgbRed: 0.55, green: 0.90, blue: 0.45, alpha: 0.75),
            NSColor(srgbRed: 0.95, green: 0.65, blue: 0.25, alpha: 0.75)][hueHash]
        for (position, size) in edgeSpecs {
            let edge = SKSpriteNode(color: strokeHue, size: size)
            edge.position = position
            edge.zPosition = 5
            addChild(edge)
            edges.append(edge)
        }
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("built in code") }

    func setSelected(_ selected: Bool) {
        let color = selected ? ScopeTheme.strokeSelected : ScopeTheme.stroke
        for edge in edges { edge.color = color }
    }

    func setLabel(visible: Bool, text: String) {
        if visible {
            if labelNode == nil {
                let node = SKLabelNode(text: text)
                node.fontName = "SFMono-Regular"
                node.fontSize = 10
                node.fontColor = ScopeTheme.label
                node.horizontalAlignmentMode = .left
                node.verticalAlignmentMode = .top
                node.position = CGPoint(x: worldRect.minX + 4, y: worldRect.maxY - 3)
                node.zPosition = 20
                addChild(node)
                labelNode = node
                labelShown = true
            }
        } else if labelShown {
            labelNode?.removeFromParent()
            labelNode = nil
            labelShown = false
        }
    }

    func setTextTexture(_ texture: SKTexture, bucket: Int, bytes: Int) {
        if let old = textSprite { old.removeFromParent() }
        let sprite = SKSpriteNode(texture: texture)
        sprite.size = worldRect.size
        sprite.position = CGPoint(x: worldRect.midX, y: worldRect.midY)
        sprite.zPosition = 10
        addChild(sprite)
        textSprite = sprite
        textTexture = texture
        textBucket = bucket
        bakedBytes += bytes
    }

    func setBarTexture(_ texture: SKTexture, bucket: Int, bytes: Int) {
        if let old = barSprite { old.removeFromParent() }
        let sprite = SKSpriteNode(texture: texture)
        sprite.size = worldRect.size
        sprite.position = CGPoint(x: worldRect.midX, y: worldRect.midY)
        sprite.zPosition = 8
        addChild(sprite)
        barSprite = sprite
        barTexture = texture
        barBucket = bucket
        bakedBytes += bytes
    }

    func clearTextures() {
        textSprite?.removeFromParent()
        textSprite = nil
        textTexture = nil
        textBucket = 0
        barSprite?.removeFromParent()
        barSprite = nil
        barTexture = nil
        barBucket = 0
        textBaked = false
        barsBaked = false
        bakedBytes = 0
    }

    /// Shows/hides the two LOD layers for the current on-screen size.
    func applyLOD(onScreenHeight: CGFloat) {
        barSprite?.isHidden = onScreenHeight < 10
        textSprite?.isHidden = onScreenHeight < 120
    }
}

// MARK: - Scene

final class CodeAtlasScene: SKScene {
    static let worldSize: CGFloat = 4096
    static let bakeBudgetBytes = 512 << 20
    static let textureLineHeight: CGFloat = 5      // px per source line at bucket base
    static let textureCharWidth: CGFloat = 2.4     // px per character
    static let barTextureMaxLines = 1200
    static let textTextureMaxLines = 2500

    let onSelect: (String) -> Void
    let readText: (String) -> String
    let rootPath: String
    var contentBounds: CGRect
    var hitPaths: Set<String> = []
    var selectedPath: String?

    private let worldNode = SKNode()
    private var cells: [String: FileCellNode] = [:]
    private var cellOrder: [String] = []
    private var bakeCursor = 0
    private var bakedTextureBytes = 0
    private var bakedCellCount = 0
    private var tick: UInt64 = 0
    private var lastUpdateTime: TimeInterval?
    private var dragged = false
    private var dragStartWorld = CGPoint.zero
    private var dragStartCamera = CGPoint.zero
    private var lastDragPoint = CGPoint.zero
    private var lastDragTime = TimeInterval.zero

    // Camera physics: zoom (screen pts per world unit) eases toward
    // targetZoom; pan glides with sampled velocity and exponential decay.
    private var zoom: CGFloat = 0.3
    private var targetZoom: CGFloat = 0.3
    private var zoomAnchorView: CGPoint = .zero
    private var zoomAnchorScene: CGPoint = .zero
    private var zoomAnchorActive = false

    init(files: [AtlasFile], contentBounds: CGRect, rootPath: String,
         onSelect: @escaping (String) -> Void, readText: @escaping (String) -> String) {
        self.onSelect = onSelect
        self.readText = readText
        self.rootPath = rootPath
        self.contentBounds = contentBounds
        super.init(size: CGSize(width: 2400, height: 1400))

        backgroundColor = ScopeTheme.background
        let cameraNode = SKCameraNode()
        camera = cameraNode
        addChild(cameraNode)
        addChild(worldNode)

        for file in files {
            let rect = CGRect(x: file.x, y: file.y, width: file.w, height: file.h)
            let cell = FileCellNode(path: file.path, worldRect: rect, bytes: file.bytes)
            worldNode.addChild(cell)
            cells[file.path] = cell
            cellOrder.append(file.path)
        }
        refitIfPossible()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("built in code") }

    // MARK: Camera — camera.xScale = 1/zoom; zoom = screen pts per world unit

    private var cameraZoom: CGFloat { 1 / (camera?.xScale ?? 1) }

    private func scaleCamera(to newZoom: CGFloat) {
        camera?.setScale(max(1 / 64, min(1 / 0.05, 1 / newZoom)))
    }

    private func refitIfPossible() {
        guard size.width > 1, size.height > 1, contentBounds.width > 1 else { return }
        let fitZoom = min(size.width / contentBounds.width, size.height / contentBounds.height)
        zoom = fitZoom
        targetZoom = fitZoom
        camera?.position = CGPoint(x: contentBounds.midX, y: contentBounds.midY)
    }

    override func didMove(to view: SKView) {
        refitIfPossible()
    }

    // MARK: Input

    func steerZoom(by factor: CGFloat, at viewPoint: CGPoint) {
        let anchorScene = convertPoint(fromView: viewPoint)
        targetZoom = max(0.05, min(64, targetZoom * factor))
        zoomAnchorView = viewPoint
        zoomAnchorScene = anchorScene
        zoomAnchorActive = true
    }

    func click(at viewPoint: CGPoint) {
        guard !dragged else { return }
        let location = convertPoint(fromView: viewPoint)
        for node in nodes(at: location) {
            var current: SKNode? = node
            while let candidate = current {
                if let path = candidate.userData?["path"] as? String {
                    selectedPath = path
                    applySelection()
                    onSelect(path)
                    return
                }
                current = candidate.parent
            }
        }
    }

    func beginDrag(at viewPoint: CGPoint) {
        dragged = false
        dragStartWorld = convertPoint(fromView: viewPoint)
        dragStartCamera = camera?.position ?? .zero
        lastDragPoint = viewPoint
        lastDragTime = currentTime
        panVelocity = .zero
    }

    func drag(to viewPoint: CGPoint) {
        let world = convertPoint(fromView: viewPoint)
        guard let camera = camera else { return }
        if hypot(world.x - dragStartWorld.x, world.y - dragStartWorld.y) * cameraZoom > 3 {
            dragged = true
        }
        camera.position.x = dragStartCamera.x - (world.x - dragStartWorld.x)
        camera.position.y = dragStartCamera.y - (world.y - dragStartWorld.y)
        // Velocity sampling for the inertial glide.
        let dt = max(0.008, currentTime - lastDragTime)
        if dt > 0 {
            let viewDX = Double(viewPoint.x - lastDragPoint.x)
            let viewDY = Double(viewPoint.y - lastDragPoint.y)
            panVelocity = CGPoint(x: viewDX / dt, y: viewDY / dt)
        }
        lastDragPoint = viewPoint
        lastDragTime = currentTime
    }

    private var currentTime: TimeInterval = 0
    private var panVelocity = CGPoint.zero

    private func select(path: String?) {
        selectedPath = path
        applySelection()
    }

    func applySelectionExternal() { applySelection() }

    private func applySelection() {
        for (key, cell) in cells { cell.setSelected(key == selectedPath) }
    }

    // MARK: Per-frame

    override func update(_ currentTime: TimeInterval) {
        self.currentTime = currentTime
        let dt = lastUpdateTime.map { min(0.05, max(0.001, currentTime - $0)) } ?? 0.016
        lastUpdateTime = currentTime
        tick &+= 1

        // Zoom easing toward the steering target, cursor-anchored.
        if zoomAnchorActive || abs(zoom - targetZoom) > zoom * 0.002 {
            zoom += (targetZoom - zoom) * min(1, dt * 9)
            scaleCamera(to: zoom)
            if zoomAnchorActive {
                camera?.position.x = zoomAnchorScene.x
                camera?.position.y = zoomAnchorScene.y
            }
            if abs(zoom - targetZoom) <= zoom * 0.001 { zoomAnchorActive = false }
        }

        // Pan inertia with exponential decay.
        if abs(panVelocity.x) > 1 || abs(panVelocity.y) > 1 {
            let scaleX = CGFloat(1 / cameraZoom)
            camera?.position.x -= CGFloat(panVelocity.x) * CGFloat(dt) * scaleX
            camera?.position.y += CGFloat(panVelocity.y) * CGFloat(dt) * scaleX
            let decay = exp(-dt * 5.5)
            panVelocity.x *= decay
            panVelocity.y *= decay
        }

        clampCamera()
        if tick % 6 == 0 { scheduleLOD() }
        if tick % 2 == 0 { bakeNext() }
    }

    /// Keeps the camera inside the content (+8% margin): there is never
    /// a reason to look at nothing.
    private func clampCamera() {
        guard let camera = camera else { return }
        let visible = visibleWorldRect()
        var b = contentBounds
        b = b.insetBy(dx: -b.width * 0.08, dy: -b.height * 0.08)
        var position = camera.position
        if visible.width >= b.width {
            position.x = b.midX
        } else {
            if visible.minX < b.minX { position.x += b.minX - visible.minX }
            if visible.maxX > b.maxX { position.x -= visible.maxX - b.maxX }
        }
        if visible.height >= b.height {
            position.y = b.midY
        } else {
            if visible.minY < b.minY { position.y += b.minY - visible.minY }
            if visible.maxY > b.maxY { position.y -= visible.maxY - b.maxY }
        }
        camera.position = position
    }

    private func visibleWorldRect() -> CGRect {
        guard let camera = camera else { return .zero }
        let halfW = size.width / 2 * camera.xScale
        let halfH = size.height / 2 * camera.yScale
        return CGRect(x: camera.position.x - halfW, y: camera.position.y - halfH,
            width: halfW * 2, height: halfH * 2)
    }

    private func scheduleLOD() {
        let viewRect = visibleWorldRect()
        let zoom = cameraZoom
        for (path, cell) in cells {
            let visible = cell.worldRect.intersects(viewRect)
            if visible { cell.lastUsedTick = tick }
            guard visible else {
                if cell.labelShown { cell.setLabel(visible: false, text: path) }
                cell.applyLOD(onScreenHeight: 0)
                continue
            }
            let onScreenHeight = cell.worldRect.height * zoom
            cell.applyLOD(onScreenHeight: onScreenHeight)
            cell.setLabel(visible: onScreenHeight >= 26 && cell.worldRect.width * zoom >= 90,
                          text: (path as NSString).lastPathComponent)
        }
    }

    private var bakeScanCursor = 0

    private func bakeNext() {
        guard !cells.isEmpty else { return }
        let zoom = cameraZoom
        let viewRect = visibleWorldRect()
        for _ in 0..<cells.count {
            let path = cellOrder[bakeCursor % cellOrder.count]
            bakeCursor += 1
            guard let cell = cells[path], !cell.bakeInFlight,
                cell.worldRect.intersects(viewRect)
            else { continue }
            let onScreenHeight = cell.worldRect.height * zoom
            // Near: full text texture. Far: saturated line-bar summary.
            if onScreenHeight >= 120, !cell.textBaked, !cell.bakeFailed {
                bakeCell(cell, kind: .text, zoom: zoom)
                return
            }
            if onScreenHeight >= 10, !cell.barsBaked, !cell.bakeFailed {
                bakeCell(cell, kind: .bars, zoom: zoom)
                return
            }
        }
    }

    enum BakeKind { case text, bars }

    private func bakeCell(_ cell: FileCellNode, kind: BakeKind, zoom: CGFloat) {
        cell.bakeInFlight = true
        let path = cell.path
        let rootPath = self.rootPath
        let isText = kind == .text
        let pixelW: CGFloat = isText ? min(2400, max(900, cell.worldRect.width * 8)) : min(2048, max(320, cell.worldRect.width * 1.5))
        let pixelH: CGFloat = isText ? min(4096, max(600, cell.worldRect.height * 8)) : min(2400, max(160, cell.worldRect.height * 1.2))

        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let source = Self.readFor(rootPath: rootPath, path: path)
            var cg: CGImage?
            if isText, !source.isEmpty {
                let capped = source.split(separator: "\n", omittingEmptySubsequences: false)
                    .prefix(Self.textTextureMaxLines).joined(separator: "\n")
                cg = Self.rasterMonokaiText(capped, pixelW: pixelW, pixelH: pixelH)
            } else {
                cg = Self.rasterBarTexture(text: source, pixelW: pixelW, pixelH: pixelH)
            }
            DispatchQueue.main.async {
                guard let self, let cg else {
                    cell.bakeInFlight = false
                    cell.bakeFailed = true
                    return
                }
                let texture = SKTexture(cgImage: cg)
                texture.filteringMode = .linear
                let bytes = cg.bytesPerRow * cg.height
                if isText {
                    cell.setTextTexture(texture, bucket: Int(pixelW), bytes: bytes)
                } else {
                    cell.setBarTexture(texture, bucket: Int(pixelW), bytes: bytes)
                }
                self.bakedTextureBytes += bytes
                self.bakedCellCount += 1
                cell.bakeInFlight = false
                if self.bakedCellCount % 40 == 0 {
                    NSLog("[fcb-atlas] %d bakes, %dMB resident", self.bakedCellCount, self.bakedTextureBytes >> 20)
                }
                while self.bakedTextureBytes > Self.bakeBudgetBytes {
                    guard let oldest = self.cells.values
                        .filter({ ($0.textTexture != nil || $0.barTexture != nil) && $0.path != path && !$0.worldRect.intersects(self.visibleWorldRect()) })
                        .sorted(by: { $0.lastUsedTick < $1.lastUsedTick })
                        .first, oldest.texture != nil || oldest.barTexture != nil
                    else { break }
                    self.bakedTextureBytes -= oldest.bakedBytes
                    oldest.clearTextures()
                }
            }
        }
    }

    static func readFor(rootPath: String, path: String) -> String {
        let full = rootPath.hasSuffix("/") ? rootPath + path : rootPath + "/" + path
        guard let data = try? Data(contentsOf: URL(fileURLWithPath: full)) else { return "" }
        return String(decoding: data.prefix(1 << 20), as: UTF8.self)
    }

    // MARK: Rasters (thread-safe: pure CGContext + AppKit bridge)

    /// Per-line vertical bars — the visual summary for blob files.
    static func rasterBarTexture(text: String, pixelW: CGFloat, pixelH: CGFloat) -> CGImage? {
        let width = Int(pixelW), height = Int(pixelH)
        let space = CGColorSpace(name: CGColorSpace.sRGB)!
        guard let ctx = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
            bytesPerRow: width * 4, space: space,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
        ctx.setFillColor(ScopeTheme.cellFill.cgColor)
        ctx.fill(CGRect(x: 0, y: 0, width: width, height: height))

        let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
        let count = min(lines.count, barTextureMaxLines)
        guard count > 0 else { return ctx.makeImage() }
        let maxLength = lines.prefix(count).map(\.count).max() ?? 1
        let slot = CGFloat(width) / CGFloat(count)
        for (index, line) in lines.prefix(count).enumerated() {
            let role = lineRoleKind(line)
            let color = lineRoleColor(role)
            let barHeight = max(2, CGFloat(line.count) / CGFloat(max(maxLength, 1)) * CGFloat(height) * 0.92)
            ctx.setFillColor(color.cgColor)
            ctx.fill(CGRect(x: CGFloat(index) * slot + 0.5, y: 0,
                width: max(0.8, slot - 0.6), height: min(barHeight, CGFloat(height))))
        }
        return ctx.makeImage()
    }

    /// Full Monokai text raster.
    static func rasterMonokaiText(_ text: String, pixelW: CGFloat, pixelH: CGFloat) -> CGImage? {
        let width = Int(pixelW), height = Int(pixelH)
        let space = CGColorSpace(name: CGColorSpace.sRGB)!
        guard let ctx = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
            bytesPerRow: width * 4, space: space,
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
        ctx.setFillColor(Monokai.background.cgColor)
        ctx.fill(CGRect(x: 0, y: 0, width: width, height: height))

        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(cgContext: ctx, flipped: true)
        let colored = Monokai.colorSource(text)
        colored.draw(
            with: NSRect(x: 4, y: 2, width: CGFloat(width) - 8, height: CGFloat(height) - 4),
            options: [.usesLineFragmentOrigin]
        )
        NSGraphicsContext.restoreGraphicsState()
        return ctx.makeImage()
    }
}

// MARK: - Host view (SKView root — scroll zoom, pinch, pan, select)

final class CodeAtlasHostView: SKView {
    var atlasScene: CodeAtlasScene?
    var lastFileCount = -1
    var dragStarted = false

    override var acceptsFirstResponder: Bool { true }

    override func scrollWheel(with event: NSEvent) {
        let delta = event.scrollingDeltaY
        guard abs(delta) > 0.0001 else { return }
        atlasScene?.steerZoom(by: pow(1.0011, delta), at: convert(event.locationInWindow, from: nil))
    }

    override func magnify(with event: NSEvent) {
        atlasScene?.steerZoom(by: 1 + event.magnification, at: convert(event.locationInWindow, from: nil))
    }

    override func mouseDown(with event: NSEvent) {
        dragStarted = true
        atlasScene?.beginDrag(at: convert(event.locationInWindow, from: nil))
    }

    override func mouseDragged(with event: NSEvent) {
        guard dragStarted else { return }
        atlasScene?.drag(to: convert(event.locationInWindow, from: nil))
    }

    override func mouseUp(with event: NSEvent) {
        dragStarted = false
        atlasScene?.click(at: convert(event.locationInWindow, from: nil))
    }
}

// MARK: - SwiftUI bridge

struct CodeAtlasView: NSViewRepresentable {
    let files: [AtlasFile]
    let contentBounds: CGRect
    let rootPath: String
    let hitPaths: Set<String>
    let selectedPath: String?
    let onSelect: (String) -> Void
    let readText: (String) -> String

    func makeNSView(context: Context) -> CodeAtlasHostView {
        let host = CodeAtlasHostView()
        host.allowsTransparency = false
        host.preferredFramesPerSecond = 120
        host.ignoresSiblingOrder = true
        host.lastFileCount = files.count
        let scene = makeScene()
        host.presentScene(scene)
        host.atlasScene = scene
        return host
    }

    private func makeScene() -> CodeAtlasScene {
        CodeAtlasScene(
            files: files, contentBounds: contentBounds, rootPath: rootPath,
            onSelect: onSelect, readText: readText
        )
    }

    func updateNSView(_ host: CodeAtlasHostView, context: Context) {
        if host.lastFileCount != files.count {
            host.lastFileCount = files.count
            let scene = makeScene()
            host.presentScene(scene)
            host.atlasScene = scene
        } else if let scene = host.atlasScene {
            scene.hitPaths = hitPaths
            scene.selectedPath = selectedPath
            scene.applySelectionExternal()
        }
    }

    static func dismantleNSView(_ host: CodeAtlasHostView, coordinator: ()) {
    }
}
