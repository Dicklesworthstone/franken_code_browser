import AppKit
import SwiftUI
import Metal
import QuartzCore
import Dispatch

/// Metal objects below are immutable after worker publication. The command
/// queue serializes GPU access; no source, parser, or filesystem enters here.
private final class AtlasCityGPU: @unchecked Sendable {
    private struct Instance { let rect: SIMD4<Float>; let metric: SIMD4<Float> }
    static let preparation = DispatchQueue(label: "dev.frankencode.browser.city-gpu", qos: .userInitiated)
    static let retirement = DispatchQueue(label: "dev.frankencode.browser.city-retirement", qos: .utility)
    static let maximumTargetPixels = 8 * 1024 * 1024
    let device: MTLDevice
    let queue: MTLCommandQueue
    let pipeline: MTLRenderPipelineState
    let solidDepth, contextDepth: MTLDepthStencilState
    let instances: MTLBuffer
    let count: Int
    let lifetime: AtlasNativeRetirement.Lease

    private init(scene: AtlasCityScene) throws {
        guard scene.instances.count <= AtlasCityScene.maximumFiles, let lifetime = scene.lifetime,
              let device = MTLCreateSystemDefaultDevice(), let queue = device.makeCommandQueue() else {
            throw AtlasCityError.unavailable
        }
        self.device = device; self.queue = queue; count = scene.instances.count; self.lifetime = lifetime
        let library = try device.makeLibrary(source: Self.shader, options: nil)
        let descriptor = MTLRenderPipelineDescriptor()
        descriptor.vertexFunction = library.makeFunction(name: "fcbCityVertex")
        descriptor.fragmentFunction = library.makeFunction(name: "fcbCityFragment")
        descriptor.colorAttachments[0].pixelFormat = .bgra8Unorm
        descriptor.depthAttachmentPixelFormat = .depth32Float
        pipeline = try device.makeRenderPipelineState(descriptor: descriptor)
        let solid = MTLDepthStencilDescriptor()
        solid.depthCompareFunction = .less; solid.isDepthWriteEnabled = true
        let ground = MTLDepthStencilDescriptor()
        // Flat context cannot consume the equal-depth roof of an unknown or
        // zero-byte parcel. It is drawn first and never writes depth.
        ground.depthCompareFunction = .always; ground.isDepthWriteEnabled = false
        guard let solidDepth = device.makeDepthStencilState(descriptor: solid),
              let contextDepth = device.makeDepthStencilState(descriptor: ground) else { throw AtlasCityError.unavailable }
        self.solidDepth = solidDepth; self.contextDepth = contextDepth
        let packed = scene.instances.map { Instance(rect: $0.rect,
            metric: SIMD4($0.height, $0.observedBytes == nil ? 1 : 0, 0, 0)) }
        let buffer = packed.withUnsafeBytes { bytes -> MTLBuffer? in
            guard let address = bytes.baseAddress, !bytes.isEmpty else { return nil }
            return device.makeBuffer(bytes: address, length: bytes.count, options: .storageModeShared)
        }
        guard let buffer else { throw AtlasCityError.unavailable }
        instances = buffer; instances.label = "City retained canonical instances"
    }
    static func prepare(_ scene: AtlasCityScene) async throws -> AtlasCityGPU {
        try await withCheckedThrowingContinuation { continuation in
            preparation.async { continuation.resume(with: Result { try AtlasCityGPU(scene: scene) }) }
        }
    }
    func depth(width: Int, height: Int) async throws -> AtlasCityDepth {
        try await withCheckedThrowingContinuation { continuation in
            Self.preparation.async { [self] in
                continuation.resume(with: Result {
                    guard width > 0, height > 0, width <= Self.maximumTargetPixels / height else {
                        throw AtlasCityError.unavailable
                    }
                    let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .depth32Float,
                        width: width, height: height, mipmapped: false)
                    descriptor.storageMode = .private; descriptor.usage = .renderTarget
                    let color = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .bgra8Unorm,
                        width: width, height: height, mipmapped: false)
                    color.storageMode = .private; color.usage = .renderTarget
                    let depthSize = device.heapTextureSizeAndAlign(descriptor: descriptor)
                    let colorSize = device.heapTextureSizeAndAlign(descriptor: color)
                    let depthAllowance = max(width * height * 4, depthSize.size + depthSize.align) + 65_536
                    let colorAllowance = max(width * height * 4, colorSize.size + colorSize.align) + 65_536
                    let bytes = depthAllowance + 3 * colorAllowance
                    guard let lifetime = AtlasCityLifetime.targets.reserve(bytes: bytes) else { throw AtlasCityError.unavailable }
                    guard let texture = device.makeTexture(descriptor: descriptor) else { throw AtlasCityError.unavailable }
                    guard texture.allocatedSize <= depthAllowance else { throw AtlasCityError.unavailable }
                    guard lifetime.publish(storage: texture, bytes: bytes) else { throw AtlasCityError.unavailable }
                    return AtlasCityDepth(texture: texture, width: width, height: height,
                        colorAllowance: colorAllowance, lifetime: lifetime)
                })
            }
        }
    }
    deinit {
        let hold = AtlasCityResourceHold([device, queue, pipeline, solidDepth, contextDepth, instances, lifetime])
        Self.retirement.async { withExtendedLifetime(hold) {} }
    }

    private static let shader = """
    #include <metal_stdlib>
    using namespace metal;
    struct CityInstance { float4 rect; float4 metric; };
    struct CityUniforms { float4x4 matrix; uint4 options; float4 building; float4 unknown; float4 selected; float4 contextColor; };
    struct CityVertex { float4 position [[position]]; float2 uv; float4 color; float marked [[flat]]; };
    constant float3 cityCorners[30] = {
        float3(0,0,1),float3(1,0,1),float3(1,1,1), float3(0,0,1),float3(1,1,1),float3(0,1,1),
        float3(0,0,0),float3(1,0,0),float3(1,0,1), float3(0,0,0),float3(1,0,1),float3(0,0,1),
        float3(1,0,0),float3(1,1,0),float3(1,1,1), float3(1,0,0),float3(1,1,1),float3(1,0,1),
        float3(1,1,0),float3(0,1,0),float3(0,1,1), float3(1,1,0),float3(0,1,1),float3(1,1,1),
        float3(0,1,0),float3(0,0,0),float3(0,0,1), float3(0,1,0),float3(0,0,1),float3(0,1,1)
    };
    constant float3 cityNormals[5] = { float3(0,0,1),float3(0,-1,0),float3(1,0,0),float3(0,1,0),float3(-1,0,0) };
    constant float2 cityUV[6] = { float2(0,0),float2(1,0),float2(1,1),float2(0,0),float2(1,1),float2(0,1) };
    vertex CityVertex fcbCityVertex(uint vertex [[vertex_id]], uint instance [[instance_id]],
        device const CityInstance *items [[buffer(0)]], constant CityUniforms &u [[buffer(1)]]) {
        CityInstance item = items[instance];
        float3 corner = cityCorners[vertex];
        bool context = u.options.y != 0;
        float3 point = float3(item.rect.xy + corner.xy * item.rect.zw, context ? 0.0f : corner.z * item.metric.x);
        CityVertex out; out.position = u.matrix * float4(point, 1.0f); out.uv = cityUV[vertex % 6];
        float3 color = item.metric.y > 0 ? u.unknown.rgb : u.building.rgb;
        bool selected = instance == u.options.x;
        if (selected) color = u.selected.rgb;
        float light = 0.48f + 0.52f * max(0.0f, dot(cityNormals[vertex / 6], normalize(float3(-0.45f,-0.55f,1.0f))));
        out.color = float4(context ? u.contextColor.rgb : color * light, 1.0f);
        out.marked = selected && !context ? 1.0f : 0.0f;
        return out;
    }
    fragment float4 fcbCityFragment(CityVertex in [[stage_in]]) {
        float edge = min(min(in.uv.x, 1.0f-in.uv.x), min(in.uv.y, 1.0f-in.uv.y));
        float pixel = max(max(fwidth(in.uv.x),fwidth(in.uv.y)),0.000001f);
        float interior = smoothstep(0.0f,pixel*(in.marked > 0 ? 2.3f : 1.15f),edge);
        return float4(in.color.rgb * mix(0.48f,1.0f,interior),1.0f);
    }
    """
}

private final class AtlasCityResourceHold: @unchecked Sendable {
    let objects: [AnyObject]
    init(_ objects: [AnyObject]) { self.objects = objects }
}
private final class AtlasCityDepth: @unchecked Sendable {
    let texture: MTLTexture
    let width, height: Int
    let colorAllowance: Int
    let lifetime: AtlasNativeRetirement.Lease
    init(texture: MTLTexture, width: Int, height: Int, colorAllowance: Int, lifetime: AtlasNativeRetirement.Lease) {
        self.texture = texture; self.width = width; self.height = height
        self.colorAllowance = colorAllowance; self.lifetime = lifetime
    }
}

struct AtlasCityMetalView: NSViewRepresentable {
    @AtlasAppearance private var appearance
    let scene: AtlasCityScene?
    let frame: AtlasCityFrame?
    let display: UInt64
    let selected: UInt32
    let viewport: (Double, Double, Double) -> Void
    let move: (AtlasCityMotion) -> Void
    let click: (Double, Double) -> Void
    let presented: (AtlasCityReceipt) -> Void
    let failed: (String) -> Void
    func makeNSView(context: Context) -> AtlasCitySurface { AtlasCitySurface() }
    func updateNSView(_ view: AtlasCitySurface, context: Context) {
        view.viewport = viewport; view.move = move; view.click = click; view.presented = presented; view.failed = failed
        view.install(scene: scene, frame: frame, display: display, selected: selected, palette: appearance)
    }
    static func dismantleNSView(_ view: AtlasCitySurface, coordinator: ()) { view.stop() }
}

/// One retained instance buffer, at most two commands in flight, one newest
/// pending draw, one preparation job. Resizes admit at most 8M depth pixels;
/// old/current/preparing targets and two flights have a fixed overlap bound.
@MainActor final class AtlasCitySurface: NSView {
    private struct Drawing: Equatable, Sendable {
        let view: AtlasCityFrame
        let display: UInt64
        let selected: UInt32
        let palette: AtlasAppearancePalette
    }
    private struct Uniforms {
        var c0, c1, c2, c3: SIMD4<Float>
        var options: SIMD4<UInt32>
        var building, unknown, selected, contextColor: SIMD4<Float>
    }
    private struct Flight {
        let serial: UInt64
        let command: MTLCommandBuffer
        let gpu: AtlasCityGPU
        let depth: AtlasCityDepth
        let drawable: CAMetalDrawable
    }
    private final class ClockTarget: NSObject, CAMetalDisplayLinkDelegate {
        weak var owner: AtlasCitySurface?
        func metalDisplayLink(_ link: CAMetalDisplayLink, needsUpdate update: CAMetalDisplayLink.Update) {
            MainActor.assumeIsolated { owner?.draw(update.drawable) }
        }
    }
    private let metal = CAMetalLayer()
    private let clockTarget = ClockTarget()
    private var clock: CAMetalDisplayLink?
    private var gpu: AtlasCityGPU?
    private var depth: AtlasCityDepth?
    /// Completed commands can precede presentation. Keep the visible color
    /// allocation charged until a later drawable is actually displayed.
    private var displayedDepth: AtlasCityDepth?
    private var displayedSerial: UInt64 = 0
    private var lastPresentationRequestSerial: UInt64 = 0
    private var requestedScene: AtlasCityScene?
    private var owner: UInt64 = 0, layoutID: UInt64 = 0
    private var epoch = UUID()
    private var preparingScene = false, preparingDepth = false
    private var pending: Drawing?
    private var latest: Drawing?
    private var flights: [Flight] = []
    private var serial: UInt64 = 0
    private var stopped = false
    private var windowObserver: NSObjectProtocol?
    private var reportedWidth = 0.0, reportedHeight = 0.0, reportedBacking = 0.0
    private var lastPointer: CGPoint?
    private var pointerDown: CGPoint?
    private var dragging = false, orbiting = false
    var viewport: (Double, Double, Double) -> Void = { _, _, _ in }
    var move: (AtlasCityMotion) -> Void = { _ in }
    var click: (Double, Double) -> Void = { _, _ in }
    var presented: (AtlasCityReceipt) -> Void = { _ in }
    var failed: (String) -> Void = { _ in }
    override var isFlipped: Bool { true }
    override var acceptsFirstResponder: Bool { true }

    init() {
        super.init(frame: .zero)
        wantsLayer = true; layer = metal
        metal.pixelFormat = .bgra8Unorm; metal.isOpaque = true; metal.framebufferOnly = true
        metal.maximumDrawableCount = 3; metal.allowsNextDrawableTimeout = true
        metal.presentsWithTransaction = false; metal.colorspace = CGColorSpace(name: CGColorSpace.sRGB)
        metal.backgroundColor = NSColor(calibratedRed: 0.07, green: 0.09, blue: 0.12, alpha: 1).cgColor
        clockTarget.owner = self
        setAccessibilityElement(true); setAccessibilityRole(.image)
        setAccessibilityLabel("City view of the retained repository atlas")
        setAccessibilityHelp("Drag to pan; Option-drag to orbit. Use the adjacent City controls to navigate and read files with the keyboard.")
    }
    required init?(coder: NSCoder) { nil }
    // Swift 6 deinit is nonisolated. Native lifecycle cleanup belongs to
    // dismantleNSView -> stop() and the main-actor window-detachment callback,
    // never an off-main access to the display link or notification token.

    func install(scene: AtlasCityScene?, frame: AtlasCityFrame?, display: UInt64, selected: UInt32,
                 palette: AtlasAppearancePalette = .canonical) {
        guard !stopped else { return }
        if scene?.owner != requestedScene?.owner || scene?.layout != requestedScene?.layout {
            epoch = UUID(); requestedScene = scene; owner = scene?.owner ?? 0; layoutID = scene?.layout ?? 0
            latest = nil; pending = nil; gpu = nil; depth = nil
            clock?.invalidate(); clock = nil
            metal.isHidden = true
            prepareScene()
        }
        if let frame, frame.owner == owner, frame.layout == layoutID, display > 0 {
            let drawing = Drawing(view: frame, display: display, selected: selected, palette: palette)
            if let latest, latest.palette != palette { metal.isHidden = true }
            if drawing != latest { latest = drawing; pending = drawing }
        } else { latest = nil; pending = nil }
        metal.backgroundColor = palette.background.cgColor
        reportViewport(); prepareDepth(); wake()
    }
    override func layout() {
        super.layout()
        reportViewport(); prepareDepth(); wake()
    }
    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if let windowObserver { NotificationCenter.default.removeObserver(windowObserver) }
        windowObserver = nil
        if let window {
            ensureClock()
            windowObserver = NotificationCenter.default.addObserver(forName: NSWindow.didChangeOcclusionStateNotification,
                object: window, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated {
                    guard let self, !self.stopped else { return }
                    if self.window?.occlusionState.contains(.visible) == true { self.pending = self.latest }
                    self.wake()
                }
            }
            if pending == nil { pending = latest }
        } else {
            clock?.invalidate(); clock = nil
        }
        reportViewport(); prepareDepth(); wake()
    }
    override func viewDidChangeBackingProperties() {
        super.viewDidChangeBackingProperties()
        reportViewport(); prepareDepth(); wake()
    }
    private var backing: Double { window?.backingScaleFactor ?? 1 }
    private func reportViewport() {
        let width = Double(bounds.width), height = Double(bounds.height), backing = backing
        guard width > 1, height > 1,
              width != reportedWidth || height != reportedHeight || backing != reportedBacking else { return }
        reportedWidth = width; reportedHeight = height; reportedBacking = backing
        // Avoid publishing SwiftUI state synchronously from updateNSView/layout.
        DispatchQueue.main.async { [weak self] in
            guard let self, !self.stopped, self.reportedWidth == width,
                  self.reportedHeight == height, self.reportedBacking == backing else { return }
            self.viewport(width, height, backing)
        }
    }
    private func prepareScene() {
        guard !stopped, !preparingScene, let scene = requestedScene else { return }
        preparingScene = true
        let ticket = epoch
        Task { [weak self] in
            do {
                let gpu = try await AtlasCityGPU.prepare(scene)
                guard let self else { return }
                self.preparingScene = false
                guard !self.stopped, self.epoch == ticket else { self.prepareScene(); return }
                self.gpu = gpu; self.metal.device = gpu.device
                self.ensureClock()
                self.prepareDepth(); self.wake()
            } catch {
                guard let self else { return }
                self.preparingScene = false
                guard !self.stopped, self.epoch == ticket else { self.prepareScene(); return }
                self.failed("City graphics could not be prepared. Return to 2D or retry City.")
            }
        }
    }
    private func ensureClock() {
        guard !stopped, window != nil, gpu != nil, clock == nil else { return }
        let clock = CAMetalDisplayLink(metalLayer: metal)
        clock.delegate = clockTarget; clock.isPaused = true
        clock.add(to: .main, forMode: .common); self.clock = clock
    }
    private func physicalSize() -> (Int, Int)? {
        let w = ceil(Double(bounds.width) * backing), h = ceil(Double(bounds.height) * backing)
        guard w.isFinite, h.isFinite, w >= 1, h >= 1,
              w <= Double(AtlasCityGPU.maximumTargetPixels), h <= Double(AtlasCityGPU.maximumTargetPixels),
              w * h <= Double(AtlasCityGPU.maximumTargetPixels) else { return nil }
        return (Int(w), Int(h))
    }
    private func prepareDepth() {
        guard !stopped, let gpu, window != nil, bounds.width > 1, bounds.height > 1 else { return }
        guard let (width, height) = physicalSize() else {
            pending = nil; latest = nil; clock?.isPaused = true
            failed("The City viewport exceeds its graphics memory limit. Make the window smaller or return to 2D.")
            return
        }
        if depth?.width == width && depth?.height == height {
            installAdmittedSize(width: width, height: height)
            return
        }
        guard !preparingDepth else { return }
        preparingDepth = true
        let ticket = epoch
        Task { [weak self] in
            do {
                let target = try await gpu.depth(width: width, height: height)
                guard let self else { return }
                self.preparingDepth = false
                guard !self.stopped, self.epoch == ticket else { self.prepareDepth(); return }
                if let size = self.physicalSize(), size.0 == width, size.1 == height {
                    self.depth = target; self.installAdmittedSize(width: width, height: height)
                }
                self.prepareDepth(); self.wake()
            } catch {
                guard let self else { return }
                self.preparingDepth = false
                guard !self.stopped, self.epoch == ticket else { self.prepareDepth(); return }
                self.failed("City depth storage is unavailable. Return to 2D or retry City.")
            }
        }
    }
    private func installAdmittedSize(width: Int, height: Int) {
        // The depth and three potential color surfaces have been admitted
        // before CAMetalLayer may allocate drawables for these dimensions.
        CATransaction.begin(); CATransaction.setDisableActions(true)
        metal.contentsScale = backing; metal.drawableSize = CGSize(width: width, height: height)
        CATransaction.commit()
    }
    private func wake() {
        let size = physicalSize()
        let targetReady = depth != nil && size?.0 == depth?.width && size?.1 == depth?.height
        clock?.isPaused = stopped || window?.occlusionState.contains(.visible) != true
            || pending == nil || gpu == nil || !targetReady || flights.count >= 2
    }
    private func retireCompleted() {
        flights.removeAll { $0.command.status == .completed || $0.command.status == .error }
        wake()
    }
    private func draw(_ drawable: CAMetalDrawable) {
        retireCompleted()
        guard !stopped, flights.count < 2, let drawing = pending, drawing == latest,
              let gpu, let depth, drawing.view.owner == owner, drawing.view.layout == layoutID,
              drawing.view.width == Double(bounds.width), drawing.view.height == Double(bounds.height),
              drawable.texture.width == depth.width, drawable.texture.height == depth.height,
              let size = physicalSize(), size.0 == depth.width, size.1 == depth.height else { wake(); return }
        guard serial < .max, let command = gpu.queue.makeCommandBuffer() else {
            pending = nil; wake(); failed("City command submission is unavailable. Return to 2D or retry City."); return
        }
        guard drawable.texture.allocatedSize <= depth.colorAllowance else {
            pending = nil; wake(); failed("City drawable storage exceeds its admitted memory allowance. Return to 2D."); return
        }
        let descriptor = MTLRenderPassDescriptor()
        descriptor.colorAttachments[0].texture = drawable.texture
        descriptor.colorAttachments[0].loadAction = .clear; descriptor.colorAttachments[0].storeAction = .store
        let palette = drawing.palette, background = palette.background
        descriptor.colorAttachments[0].clearColor = MTLClearColor(red: background.red, green: background.green, blue: background.blue, alpha: 1)
        descriptor.depthAttachment.texture = depth.texture
        descriptor.depthAttachment.loadAction = .clear; descriptor.depthAttachment.storeAction = .dontCare
        descriptor.depthAttachment.clearDepth = 1
        guard let encoder = command.makeRenderCommandEncoder(descriptor: descriptor) else {
            pending = nil; wake(); failed("City rendering could not begin. Return to 2D or retry City."); return
        }
        let m = drawing.view.matrix
        var uniform = Uniforms(c0: SIMD4(m[0], m[1], m[2], m[3]), c1: SIMD4(m[4], m[5], m[6], m[7]),
            c2: SIMD4(m[8], m[9], m[10], m[11]), c3: SIMD4(m[12], m[13], m[14], m[15]),
            options: SIMD4(drawing.selected, 1, 0, 0),
            building: palette.color("type").vector, unknown: palette.color("comment").vector,
            selected: palette.selection.vector, contextColor: palette.parcel.vector)
        encoder.setRenderPipelineState(gpu.pipeline)
        encoder.setCullMode(.none)
        encoder.setVertexBuffer(gpu.instances, offset: 0, index: 0)
        encoder.setDepthStencilState(gpu.contextDepth)
        encoder.setVertexBytes(&uniform, length: MemoryLayout<Uniforms>.stride, index: 1)
        encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 6, instanceCount: gpu.count)
        uniform.options.y = 0
        encoder.setDepthStencilState(gpu.solidDepth)
        encoder.setVertexBytes(&uniform, length: MemoryLayout<Uniforms>.stride, index: 1)
        // Scene order is ascending file ID; strict-less depth implements the
        // shared CPU picker's equal-depth lower-ID rule.
        encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 30, instanceCount: gpu.count)
        encoder.endEncoding()
        serial += 1
        let serial = serial, ticket = epoch
        pending = nil
        drawable.addPresentedHandler { [weak self, depth] drawable in
            let time = drawable.presentedTime
            Task { @MainActor [weak self] in
                guard let self, time.isFinite, time > 0, !self.stopped else { return }
                if serial > self.displayedSerial {
                    self.displayedSerial = serial; self.displayedDepth = depth
                }
                guard self.epoch == ticket,
                      self.window != nil, self.latest == drawing,
                      drawing.view.width == Double(self.bounds.width), drawing.view.height == Double(self.bounds.height) else { return }
                self.presented(.init(view: drawing.view, frame: serial, display: drawing.display))
            }
        }
        command.addCompletedHandler { [weak self, gpu, depth, drawable] command in
            // Independent completion ownership survives view/window teardown.
            withExtendedLifetime((gpu, depth)) {}
            let failed = command.status == .error
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.retireCompleted()
                if failed && !self.stopped && self.epoch == ticket {
                    self.failed("A City graphics command failed. Return to 2D or retry City.")
                } else if !failed, !self.stopped, self.epoch == ticket,
                          serial > self.lastPresentationRequestSerial,
                          self.latest?.palette == drawing.palette, self.window != nil,
                          drawing.view.width == Double(self.bounds.width),
                          drawing.view.height == Double(self.bounds.height) {
                    // Accept after completion without waiting on the main
                    // thread. A submitted frame from the previous palette may
                    // retire its resources but must never become visible.
                    self.metal.isHidden = false
                    self.lastPresentationRequestSerial = serial
                    drawable.present()
                }
            }
        }
        flights.append(Flight(serial: serial, command: command, gpu: gpu, depth: depth, drawable: drawable))
        command.commit(); wake()
    }

    func stop() {
        guard !stopped else { return }
        stopped = true; epoch = UUID(); pending = nil; latest = nil; requestedScene = nil
        clock?.invalidate(); clock = nil; metal.isHidden = true
        if let windowObserver { NotificationCenter.default.removeObserver(windowObserver) }
        windowObserver = nil
        // Commands also retain their resources through completion. Detaching
        // therefore needs neither a driver wait nor a synchronous GPU teardown.
        gpu = nil; depth = nil
        // displayedDepth stays charged through layer detachment/deinit.
        viewport = { _, _, _ in }; move = { _ in }; click = { _, _ in }
        presented = { _ in }; failed = { _ in }
    }

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        let point = convert(event.locationInWindow, from: nil)
        pointerDown = point; lastPointer = point; dragging = false; orbiting = event.modifierFlags.contains(.option)
    }
    override func mouseDragged(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        guard let start = pointerDown, let last = lastPointer else { return }
        if !dragging && hypot(point.x - start.x, point.y - start.y) < 3 { return }
        dragging = true; lastPointer = point
        let x = Double(point.x - last.x), y = Double(point.y - last.y)
        move(orbiting ? .orbit(x * 0.006, y * 0.006) : .pan(x, y))
    }
    override func mouseUp(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        if pointerDown != nil && !dragging && !orbiting && bounds.contains(point) { click(point.x, point.y) }
        pointerDown = nil; lastPointer = nil; dragging = false
    }
    override func scrollWheel(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        if event.modifierFlags.contains(.command) || event.modifierFlags.contains(.control) {
            move(.zoom(point.x, point.y, exp(max(-0.2, min(0.2, Double(event.scrollingDeltaY) * 0.01)))))
        } else { move(.pan(Double(event.scrollingDeltaX), Double(event.scrollingDeltaY))) }
    }
    override func magnify(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        move(.zoom(point.x, point.y, exp(max(-0.3, min(0.3, Double(event.magnification))))))
    }
    override func keyDown(with event: NSEvent) {
        let option = event.modifierFlags.contains(.option)
        switch event.keyCode {
        case 123: move(option ? .orbit(-0.12, 0) : .pan(40, 0))
        case 124: move(option ? .orbit(0.12, 0) : .pan(-40, 0))
        case 125: move(option ? .orbit(0, -0.08) : .pan(0, -40))
        case 126: move(option ? .orbit(0, 0.08) : .pan(0, 40))
        default: super.keyDown(with: event)
        }
    }
}
