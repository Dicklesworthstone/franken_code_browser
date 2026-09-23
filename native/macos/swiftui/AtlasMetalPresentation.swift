import AppKit
import Metal
import CoreText
import QuartzCore
import os

/// The sole drawable owner. Input coalesces requests; the display callback
/// submits work and accepts completed frames without waiting for the GPU.
@MainActor final class AtlasMetalPresentation {
    enum RenderingMode { case nativeDisplay, retainedCapture, retainedCoverage }
    let renderingMode: RenderingMode
    struct Frame {
        let tiles: [AtlasTextTile]
        let revision: UUID
        let scale: Double
        let offset: CGPoint
        let selectedPath: String?
        let hitPaths: Set<String>
        let matchRows: [CGRect]
        let size: CGSize
        let backing: Double
    }
    private struct Request { let serial: UInt64; let frame: Frame; let tileIDs: [Int]; let rasterIDs: [Int] }
    private struct Flight {
        let request: Request
        let scene: AtlasMetalGlyphRenderer?
        let raster: AtlasMetalRasterRenderer?
        let command: MTLCommandBuffer
        let drawable: CAMetalDrawable
        let delivery: DeliverySample?
    }
    private static let logger = Logger(subsystem: "org.franken.FrankenCodeBrowser", category: "Metal")
    private var loggedRevision: UUID?
    let layer = CAMetalLayer()
    let device: MTLDevice
    private let queue: MTLCommandQueue
    private final class ClockTarget: NSObject, CAMetalDisplayLinkDelegate {
        weak var owner: AtlasMetalPresentation?
        func metalDisplayLink(_ link: CAMetalDisplayLink, needsUpdate update: CAMetalDisplayLink.Update) {
            // The link is registered exclusively on RunLoop.main. Keep the
            // ready drawable in this callback instead of hopping through Task.
            MainActor.assumeIsolated { owner?.display(drawable: update.drawable, deadline: update.targetTimestamp, targetPresentation: update.targetPresentationTimestamp) }
        }
    }
    private let clockTarget = ClockTarget()
    private var clock: CAMetalDisplayLink?
    private(set) var scene: AtlasMetalGlyphRenderer?
    private(set) var rasterScene: AtlasMetalRasterRenderer?
    // EXPERIMENTAL: cached-image quads reduce CPU work but currently regress
    // accepted-camera cadence. Opt in only for renderer performance experiments.
    var rasterEnabled = ProcessInfo.processInfo.environment["FCB_METAL_OVERVIEW"] == "1"
    // Experimental until foreground cadence is qualified. The completion path
    // remains the default; both paths retain command resources until terminal.
    var scheduledPresentationEnabled = ProcessInfo.processInfo.environment["FCB_METAL_SCHEDULED_PRESENTATION"] == "1"
    private var revision: UUID?
    private var batchIDs: [Int] = []
    private var latestFrame: Frame?
    private var pending: Request?
    private var flights: [Flight] = []
    private var serial: UInt64 = 0
    private var acceptedSerial: UInt64 = 0
    private(set) var submissions = 0, presentations = 0, failures = 0
    private(set) var maximumInFlight = 0
    private(set) var gpuSeconds: [Double] = []
    struct PresentationSample {
        let serial: UInt64
        let revision: UUID
        let time: Double
    }
    struct DeliverySample {
        let serial: UInt64
        let callbackTime: Double
        let deadline: Double
        let targetPresentation: Double
        var encodedTime: Double
        var gpuStart: Double = 0
        var gpuEnd: Double = 0
        var presentCall: Double = 0
    }
    private(set) var deliverySamples: [DeliverySample] = []
    private let recordPresentationTiming = ProcessInfo.processInfo.environment["FCB_METAL_PRESENTATION_TIMING"] == "1"
    private(set) var presentationSamples: [PresentationSample] = []
    private(set) var presentationCallbacks = 0
    private(set) var skippedPresentationCallbacks = 0
    private(set) var coveredTiles: Set<Int> = []
    private(set) var coveredGlyphTiles: Set<Int> = []
    private(set) var coveredRasterTiles: Set<Int> = []
    private(set) var acceptedFrame: Frame?
    var hasWork: Bool { pending != nil || !flights.isEmpty }
    /// Read only on an explicit diagnostic request, never on camera ticks.
    /// Command status reads do not wait for completion or acquire a drawable.
    var diagnosticState: [String: Any] {
        ["request_serial": String(serial), "accepted_serial": String(acceptedSerial),
         "pending_serial": pending.map { String($0.serial) } ?? "none",
         "flights_serial_status": flights.map { "\($0.request.serial):\($0.command.status.rawValue)" },
         "clock_paused": clock.map { String($0.isPaused) } ?? "unavailable",
         "layer_hidden": layer.isHidden, "submissions": submissions,
         "presentations": presentations, "failures": failures,
         "revision": revision?.uuidString ?? "none"]
    }
    var managedBytes: Int {
        var seen: Set<ObjectIdentifier> = []
        let glyphBytes = ([scene].compactMap { $0 } + flights.compactMap(\.scene)).reduce(0) { sum, value in
            seen.insert(ObjectIdentifier(value)).inserted ? sum + value.managedBytes : sum
        }
        return glyphBytes + ([rasterScene].compactMap { $0 } + flights.compactMap(\.raster)).reduce(0) { sum, value in
            seen.insert(ObjectIdentifier(value)).inserted ? sum + value.managedBytes : sum
        }
    }
    var accept: ((Frame, Set<Int>) -> Void)?

    init?(renderingMode: RenderingMode = .retainedCoverage) {
        guard let device = MTLCreateSystemDefaultDevice(), device.hasUnifiedMemory,
              let queue = device.makeCommandQueue() else { return nil }
        self.device = device; self.queue = queue; self.renderingMode = renderingMode
        layer.device = device; layer.pixelFormat = .bgra8Unorm
        layer.framebufferOnly = true; layer.isOpaque = false
        layer.maximumDrawableCount = 3; layer.allowsNextDrawableTimeout = true
        layer.presentsWithTransaction = true
        layer.colorspace = CGColorSpace(name: CGColorSpace.sRGB)
        layer.isHidden = true
        clockTarget.owner = self
        let clock = CAMetalDisplayLink(metalLayer: layer)
        clock.delegate = clockTarget
        clock.isPaused = true
        clock.add(to: .main, forMode: .common)
        self.clock = clock
    }

    deinit { clock?.invalidate() }

    func prepare(tiles: [AtlasTextTile], revision: UUID) {
        guard self.revision != revision else { return }
        self.revision = revision; pending = nil
        let previous = managedBytes
        // Reserve cold CoreText-to-glyph staging before building any row arrays.
        // Warm prepared runs are shared, not copied. This reservation overlaps
        // old scenes, new GPU resources and the renderer's own upload staging.
        let limit = AtlasMetalGlyphRenderer.maximumManagedBytes
        var preparationBytes = 64 * 1024 * 1024
        // Preserve glyph admission at higher zoom; overview uses the remaining
        // budget. Even a refused glyph scene can still have bounded raster coverage.
        defer {
            if rasterEnabled {
                rasterScene = try? AtlasMetalRasterRenderer(device: device, tiles: tiles,
                    previousManagedBytes: previous + preparationBytes + (scene?.managedBytes ?? 0))
            } else {
                rasterScene = nil
            }
        }
        func charge(_ count: Int, _ stride: Int) -> Bool {
            guard count >= 0, preparationBytes <= limit - previous,
                  count <= (limit - previous - preparationBytes) / stride else { return false }
            preparationBytes += count * stride
            return true
        }
        func chargeCold(_ line: CTLine) -> Bool {
            charge(CTLineGetGlyphCount(line), 40) && charge(CFArrayGetCount(CTLineGetGlyphRuns(line)), 256)
        }
        var admitted = true
        for tile in tiles {
            guard charge(tile.lineCount + 1, 256) else { admitted = false; break }
            if tile.preparedLines == nil {
                for line in tile.lines where !chargeCold(line) { admitted = false; break }
            }
            if tile.preparedHeader == nil, let header = tile.header, !chargeCold(header) { admitted = false }
            if !admitted { break }
        }
        guard admitted else { scene = nil; batchIDs = []; failures += 1; return }
        var source: [AtlasMetalGlyphRenderer.SceneTile] = []
        source.reserveCapacity(tiles.count)
        for (index, tile) in tiles.enumerated() {
            var lines: [AtlasMetalGlyphRenderer.Line] = []
            let header = tile.preparedHeader ?? tile.header.flatMap { AtlasPreparedLine($0) }
            let rows = tile.preparedLines ?? tile.lines.compactMap { AtlasPreparedLine($0) }
            guard let header, rows.count == tile.lineCount else { continue }
            lines.reserveCapacity(rows.count + 1)
            lines.append(.init(text: header, origin: CGPoint(x: 4, y: 12), clip: tile.rect))
            for (row, text) in rows.enumerated() {
                lines.append(.init(text: text, origin: CGPoint(x: 4, y: 34 + Double(row) * AtlasTextTile.lineHeight), clip: tile.rect))
            }
            source.append(.init(id: index, rect: tile.rect, sourceScale: tile.contentScale, lines: lines))
        }
        do {
            // Fixed source captures retain native colored pixels across camera changes.
            // Their qualified minification interval differs from display-grid hinting.
            let retainedCapture = renderingMode != .nativeDisplay
            let tileMemory = renderingMode == .retainedCoverage && device.supportsFamily(.apple2)
            scene = try AtlasMetalGlyphRenderer(device: device, tiles: source,
                pixelsPerPoint: retainedCapture ? 4 : 8,
                previousManagedBytes: previous + preparationBytes,
                minimumPixelsPerPoint: tileMemory ? AtlasMetalGlyphRenderer.tileMemoryMinimumDensity : (retainedCapture ? 0.1 : 4),
                maskRepresentation: renderingMode == .retainedCapture ? .opaqueRGBPhases : .grayscale,
                maskPlacement: retainedCapture ? .captureGrid : .displayGrid, tileMemoryAccumulation: tileMemory)
        } catch { scene = nil; failures += 1 }
        batchIDs = scene?.batches.keys.sorted() ?? []
    }

    /// Called by input. Only retained geometry is inspected here. Fixed source
    /// coverage stays on the GPU throughout its qualified minification range;
    /// cached source images and CoreText cover the remaining densities.
    func request(_ frame: Frame) {
        latestFrame = frame
        serial &+= 1
        let viewport = CGRect(x: -frame.offset.x / frame.scale, y: -frame.offset.y / frame.scale,
            width: frame.size.width / frame.scale, height: frame.size.height / frame.scale)
        let density = frame.scale * frame.backing
        let ids = batchIDs.filter { index in
            guard let batch = scene?.batches[index], batch.rect.intersects(viewport),
                  batch.supportsDisplayDensity(density), frame.tiles.indices.contains(index) else { return false }
            let tile = frame.tiles[index]
            if let image = tile.raster, tile.rect.width > 0, tile.rect.height > 0,
               Double(image.width) / tile.rect.width >= density * 1.15,
               Double(image.height) / tile.rect.height >= density * 1.15 { return false }
            return true
        }
        var rasterIDs: [Int] = []
        if rasterEnabled {
            let glyphIDs = Set(ids)
            let rasterViewport = viewport.insetBy(dx: -1 / density, dy: -1 / density)
            for index in frame.tiles.indices {
                let tile = frame.tiles[index]
                guard let image = tile.raster, tile.rect.intersects(rasterViewport), !glyphIDs.contains(index) else { continue }
                // All Metal content sits below the CPU image tree. Move only an
                // original-order prefix, so a refused image cannot be reordered
                // beneath a later cached image at fractional shared-edge pixels.
                guard let retained = rasterScene?.tiles[index], retained.rect == tile.rect,
                      retained.image === image, retained.supports(density) else { break }
                rasterIDs.append(index)
            }
        }
        let request = Request(serial: serial, frame: frame, tileIDs: ids, rasterIDs: rasterIDs)
        if (ids.isEmpty && rasterIDs.isEmpty) || frame.size.width <= 0 || frame.size.height <= 0 {
            pending = nil
            acceptCPU(request)
        } else {
            pending = request
            let physical = CGSize(width: ceil(frame.size.width * frame.backing), height: ceil(frame.size.height * frame.backing))
            guard physical.width * physical.height <= 32 * 1024 * 1024 else {
                pending = nil; failures += 1; acceptCPU(request); return
            }
            layer.frame = CGRect(origin: .zero, size: frame.size)
            if layer.drawableSize != physical { layer.drawableSize = physical }
        }
        clock?.isPaused = !hasWork
    }

    /// Fence every queued receipt when the host switches to CPU or detaches.
    /// GPU resources remain retained until their commands reach a terminal state.
    func invalidatePresentation() {
        serial &+= 1; acceptedSerial = serial
        pending = nil; latestFrame = nil; coveredTiles = []; coveredGlyphTiles = []; coveredRasterTiles = []; acceptedFrame = nil
        layer.isHidden = true
        clock?.isPaused = true
    }

    func resize(size: CGSize, backing: Double) {
        guard let old = latestFrame, old.size != size || old.backing != backing else { return }
        let frame = Frame(tiles: old.tiles, revision: old.revision, scale: old.scale, offset: old.offset,
            selectedPath: old.selectedPath, hitPaths: old.hitPaths, matchRows: old.matchRows,
            size: size, backing: backing)
        // Expose complete sharp CPU coverage during a drawable resize. A newly
        // submitted frame receives a later serial than this temporary fallback.
        invalidatePresentation()
        acceptCPU(Request(serial: serial, frame: frame, tileIDs: [], rasterIDs: []))
        request(frame)
    }

    private func acceptCPU(_ request: Request) {
        acceptedSerial = request.serial; coveredTiles = []; coveredGlyphTiles = []; coveredRasterTiles = []; acceptedFrame = request.frame
        CATransaction.begin(); CATransaction.setDisableActions(true)
        layer.isHidden = true
        accept?(request.frame, [])
        CATransaction.commit()
    }

    // A separate scope releases superseded completed drawables before acquiring
    // the next one. Holding them through nextDrawable can exhaust its pool.
    private func acceptCompleted(size: CGSize, backing: Double) -> Bool {
        var terminal: [Flight] = []
        flights.removeAll { flight in
            // GPU completion can race this main-thread scan. Collect and remove
            // using the same status observation: a second independent scan can
            // otherwise remove a newly completed flight without presenting it.
            let status = flight.command.status
            guard status == .completed || status == .error else { return false }
            terminal.append(flight)
            return true
        }
        for flight in terminal where flight.command.status == .completed {
            let elapsed = flight.command.gpuEndTime - flight.command.gpuStartTime
            if elapsed > 0, elapsed.isFinite {
                if gpuSeconds.count == 240 { gpuSeconds.removeFirst() }
                gpuSeconds.append(elapsed)
            }
        }
        // Scheduled acceptance does not retire its Flight. The terminal scan
        // above remains the sole resource-retirement owner, including detach.
        let ready = scheduledPresentationEnabled ? flights.filter {
            $0.command.status == .scheduled || $0.command.status == .completed || $0.command.status == .error
        } + terminal : terminal
        if let newest = ready.filter({
            ($0.request.serial > acceptedSerial ||
                ($0.request.serial == acceptedSerial && $0.command.status == .error)) &&
            $0.request.frame.revision == revision &&
            $0.request.frame.size == size && $0.request.frame.backing == backing
        }).max(by: { $0.request.serial < $1.request.serial }) {
            let rasterStillCurrent = newest.request.rasterIDs.allSatisfy { id in
                guard newest.request.frame.tiles.indices.contains(id), let retained = newest.raster?.tiles[id],
                      let image = newest.request.frame.tiles[id].raster else { return false }
                return retained.image === image && retained.rect == newest.request.frame.tiles[id].rect
            }
            if (newest.command.status == .completed ||
                (scheduledPresentationEnabled && newest.command.status == .scheduled)) && rasterStillCurrent {
                let frame = newest.request.frame
                acceptedSerial = newest.request.serial; acceptedFrame = frame
                coveredGlyphTiles = Set(newest.request.tileIDs)
                coveredRasterTiles = Set(newest.request.rasterIDs)
                coveredTiles = coveredGlyphTiles.union(coveredRasterTiles)
                CATransaction.begin(); CATransaction.setDisableActions(true)
                layer.frame = CGRect(origin: .zero, size: frame.size)
                layer.isHidden = false
                accept?(frame, coveredTiles)
                if recordPresentationTiming {
                    let serial = newest.request.serial
                    let revision = frame.revision
                    newest.drawable.addPresentedHandler { [weak self] drawable in
                        let time = drawable.presentedTime
                        Task { @MainActor [weak self] in
                            self?.recordPresentation(serial: serial, revision: revision, time: time)
                        }
                    }
                }
                if var sample = newest.delivery, deliverySamples.count < 20_000 {
                    sample.gpuStart = newest.command.gpuStartTime
                    sample.gpuEnd = newest.command.gpuEndTime
                    sample.presentCall = CACurrentMediaTime()
                    deliverySamples.append(sample)
                }
                newest.drawable.present()
                CATransaction.commit()
                presentations += 1
                if loggedRevision != frame.revision {
                    loggedRevision = frame.revision
                    Self.logger.notice("Presented Metal text: tiles=\(self.coveredTiles.count) instances=\(newest.scene?.instanceCount ?? 0) managedBytes=\(self.managedBytes)")
                }
            } else { failures += 1; acceptCPU(newest.request) }
            return true
        }
        return false
    }

    /// Diagnostic-only display receipts, distinct from command execution and
    /// CPU acceptance. A zero timestamp means the drawable was not displayed.
    private func recordPresentation(serial: UInt64, revision: UUID, time: Double) {
        presentationCallbacks += 1
        guard time.isFinite, time > 0 else { skippedPresentationCallbacks += 1; return }
        if presentationSamples.count == 4096 { presentationSamples.removeFirst() }
        presentationSamples.append(.init(serial: serial, revision: revision, time: time))
    }

    /// The CPU detail clock may drain receipts, but never acquires a drawable.
    func advance(size: CGSize, backing: Double) {
        resize(size: size, backing: backing)
        autoreleasepool { _ = acceptCompleted(size: size, backing: backing) }
        clock?.isPaused = !hasWork
    }

    private func recordGPUCompletion(serial: UInt64, start: Double, end: Double) {
        guard recordPresentationTiming,
              let index = deliverySamples.lastIndex(where: { $0.serial == serial }) else { return }
        // A scheduled presentation can precede valid GPU timestamps. Complete
        // that diagnostic receipt when the GPU is done, never guess a duration.
        deliverySamples[index].gpuStart = start
        deliverySamples[index].gpuEnd = end
    }

    /// A completed command releases its drawable lease without waiting for
    /// another display callback (which may itself need an available drawable).
    private func commandCompleted() {
        guard let frame = latestFrame else {
            // Detached/superseded frames may retire resources but never present.
            flights.removeAll { $0.command.status == .completed || $0.command.status == .error }
            return
        }
        advance(size: frame.size, backing: frame.backing)
    }

    /// Core Animation supplies an available drawable. There is no blocking
    /// nextDrawable call, and accepting a frame does not consume an extra tick.
    private func display(drawable: CAMetalDrawable, deadline: Double, targetPresentation: Double) {
        let callbackTime = recordPresentationTiming ? CACurrentMediaTime() : 0
        guard let frame = latestFrame else { clock?.isPaused = true; return }
        autoreleasepool {
            _ = acceptCompleted(size: frame.size, backing: frame.backing)
            submitPending(drawable: drawable, size: frame.size, backing: frame.backing, callbackTime: callbackTime, deadline: deadline, targetPresentation: targetPresentation)
        }
        clock?.isPaused = !hasWork
    }

    private func submitPending(drawable: CAMetalDrawable, size: CGSize, backing: Double, callbackTime: Double, deadline: Double, targetPresentation: Double) {
        guard flights.count < 2, let request = pending,
              request.frame.revision == revision, request.frame.size == size,
              request.frame.backing == backing else { return }
        // A resize can leave one callback carrying the previous drawable size.
        // Keep the request pending until the layer supplies the current target.
        guard drawable.texture.width == Int(ceil(size.width * backing)),
              drawable.texture.height == Int(ceil(size.height * backing)) else { return }
        pending = nil
        guard let command = queue.makeCommandBuffer() else {
            failures += 1; acceptCPU(request); return
        }
        do {
            if let scene {
                try scene.encode(into: drawable.texture, commandBuffer: command,
                    scale: request.frame.scale * backing,
                    offset: CGPoint(x: request.frame.offset.x * backing, y: request.frame.offset.y * backing),
                    tileIDs: request.tileIDs, raster: rasterScene, rasterIDs: request.rasterIDs)
            } else if let raster = rasterScene {
                try raster.encode(into: drawable.texture, commandBuffer: command, scale: request.frame.scale * backing,
                    offset: CGPoint(x: request.frame.offset.x * backing, y: request.frame.offset.y * backing),
                    tileIDs: request.rasterIDs)
            } else { throw AtlasMetalGlyphRenderer.Fallback.allocation }
            let delivery = recordPresentationTiming ? DeliverySample(serial: request.serial,
                callbackTime: callbackTime, deadline: deadline, targetPresentation: targetPresentation,
                encodedTime: CACurrentMediaTime()) : nil
            flights.append(Flight(request: request, scene: scene, raster: rasterScene, command: command, drawable: drawable, delivery: delivery))
            maximumInFlight = max(maximumInFlight, flights.count)
            if scheduledPresentationEnabled {
                command.addScheduledHandler { [weak self] _ in
                    // No main-thread wait. Apple's presentsWithTransaction
                    // contract requires scheduled, not completed, GPU work.
                    Task { @MainActor [weak self] in self?.commandCompleted() }
                }
            }
            let serial = request.serial
            command.addCompletedHandler { [weak self] command in
                let start = command.gpuStartTime, end = command.gpuEndTime
                Task { @MainActor [weak self] in
                    self?.recordGPUCompletion(serial: serial, start: start, end: end)
                    self?.commandCompleted()
                }
            }
            command.commit(); submissions += 1
        } catch { failures += 1; acceptCPU(request) }
    }
}
