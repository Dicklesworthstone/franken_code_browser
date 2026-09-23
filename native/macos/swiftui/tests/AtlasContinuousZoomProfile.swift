import Foundation
import AppKit
import QuartzCore

extension AtlasRenderProfile {
    @MainActor static func continuousZoom(tiles: [AtlasTextTile], bounds: CGRect) throws {
        if ProcessInfo.processInfo.environment["FCB_ZOOM_SHARP_REUSE_REGRESSION"] == "1" {
            try sharpFallbackReuseRegression(); return
        }
        if ProcessInfo.processInfo.environment["FCB_NATIVE_CADENCE"] == "1" {
            nativeCadence(tiles: tiles, bounds: bounds); return
        }
        let mode = ProcessInfo.processInfo.environment["FCB_CONTINUOUS_ZOOM"]!
        precondition(["fallback", "cpu", "gpu"].contains(mode))
        let app = NSApplication.shared
        app.setActivationPolicy(ProcessInfo.processInfo.environment["FCB_PROFILE_APP"] == "1" ? .regular : .accessory)
        app.finishLaunching()
        let window = NSWindow(contentRect: CGRect(origin: .zero, size: viewport), styleMask: [.titled], backing: .buffered, defer: false)
        let captureMode = ProcessInfo.processInfo.environment["FCB_RETAINED_CAPTURE"]
        let surface = captureMode == "default" && mode != "cpu" ? AtlasRetainedSurface() :
            AtlasRetainedSurface(metalEnabled: mode != "cpu", metalRenderingMode:
                captureMode == "scalar" ? .retainedCoverage : (captureMode == nil ? .nativeDisplay : .retainedCapture))
        if captureMode == "default" && mode != "cpu" {
            precondition(surface.metal?.renderingMode == .retainedCoverage,
                "The unconfigured production surface must select qualified retained coverage")
        }
        surface.frame = CGRect(origin: .zero, size: viewport)
        window.contentView = surface; window.center(); window.makeKeyAndOrderFront(nil)
        app.activate(ignoringOtherApps: true); window.orderFrontRegardless(); surface.setBackingScale(backing)
        if ProcessInfo.processInfo.environment["FCB_PROFILE_APP"] == "1" {
            let visibleDeadline = now() + 30
            while (!app.isActive || !window.occlusionState.contains(.visible)) && now() < visibleDeadline {
                _ = pumpEvents(until: Date(timeIntervalSinceNow: 0.01))
            }
            precondition(app.isActive && window.occlusionState.contains(.visible),
                "App presentation qualification requires an active, visible window")
        }
        let revision = UUID()
        var accepted: (Double, CGPoint, Double)?
        surface.onPresentedFrame = { _, scale, offset in accepted = (scale, offset, now()) }
        var center = CGPoint(x: bounds.midX, y: bounds.midY)
        surface.update(tiles: tiles, revision: revision, scale: 0.05,
            offset: CGPoint(x: viewport.width / 2 - center.x * 0.05, y: viewport.height / 2 - center.y * 0.05), selectedPath: nil, hitPaths: [])
        var low = 0.05, high = 1.6
        if mode == "gpu" {
            let index = surface.metalScene!.batches.keys.min { a, b in
                hypot(tiles[a].rect.midX - bounds.midX, tiles[a].rect.midY - bounds.midY) <
                    hypot(tiles[b].rect.midX - bounds.midX, tiles[b].rect.midY - bounds.midY)
            }!
            let tile = tiles[index]
            center = CGPoint(x: tile.rect.midX, y: tile.rect.midY)
            low = (captureMode == nil ? 4.05 : 0.105) / tile.contentScale / backing
            high = (captureMode == nil ? 7.95 : 3.95) / tile.contentScale / backing
        }
        func verify(_ scale: Double, _ offset: CGPoint, exact: Bool) throws {
            let view = CGRect(x: -offset.x / scale, y: -offset.y / scale, width: viewport.width / scale, height: viewport.height / scale)
            let snapshot = surface.retainedTransitionImage
            if let snapshot {
                precondition(snapshot.rect.contains(view))
                precondition(Double(snapshot.image.width) / snapshot.rect.width >= scale * backing && Double(snapshot.image.height) / snapshot.rect.height >= scale * backing)
                if exact {
                    let canvas = snapshot.rect, image = snapshot.image
                    let bitmap = CGContext(data: nil, width: image.width, height: image.height, bitsPerComponent: 8,
                        bytesPerRow: image.width * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
                    bitmap.translateBy(x: 0, y: Double(image.height))
                    bitmap.scaleBy(x: Double(image.width) / canvas.width, y: -Double(image.height) / canvas.height)
                    bitmap.translateBy(x: -canvas.minX, y: -canvas.minY)
                    for index in snapshot.tiles {
                        let tile = tiles[index], clip = tile.rect.intersection(canvas)
                        bitmap.saveGState(); bitmap.clip(to: clip); bitmap.setFillColor(Monokai.background.cgColor); bitmap.fill(clip)
                        bitmap.translateBy(x: tile.rect.minX, y: tile.rect.minY); bitmap.scaleBy(x: tile.contentScale, y: tile.contentScale)
                        tile.draw(in: bitmap); bitmap.restoreGState()
                    }
                    precondition(bitmap.makeImage()!.dataProvider!.data! as Data == image.dataProvider!.data! as Data)
                }
            }
            let overview = Dictionary(uniqueKeysWithValues: surface.retainedOverviewImages.map { ($0.tile, $0) })
            let patches = surface.retainedDetailImages.filter { $0.visible }
            for (index, tile) in tiles.enumerated() {
                let target = tile.rect.intersection(view)
                if target.isNull || target.isEmpty || surface.gpuCoveredTiles.contains(index) { continue }
                if let snapshot, snapshot.tiles.contains(index), snapshot.rect.contains(target) { continue }
                if let image = overview[index], image.visible,
                    min(Double(image.image.width) / image.rect.width, Double(image.image.height) / image.rect.height) >= scale * backing { continue }
                precondition(sharpPatchCoverage(patches, tile: tile, index: index, target: target,
                    density: scale * backing), "moving source must retain complete sharp coverage")
            }
            precondition(surface.metrics.transitionFailures == 0 && surface.metrics.transitionBytes * 3 <= AtlasRetainedSurface.transitionByteLimit)
            if mode == "gpu" {
                precondition(!surface.gpuCoveredTiles.isEmpty, "GPU-domain trajectory must actually render source on GPU")
                if exact { try AtlasMetalRasterTests.verifyAccepted(surface) }
                if exact, let scene = surface.metalScene, !surface.gpuGlyphTiles.isEmpty {
                    if captureMode != nil {
                        try output(AtlasMetalImageOracle.compareFixedVisibleControl(scene: scene, tiles: tiles,
                            ids: surface.gpuGlyphTiles.sorted(), scale: scale, backing: backing, phase: 0.25))
                    } else {
                        try output(AtlasMetalImageOracle.compare(scene: scene, tiles: tiles,
                            ids: surface.gpuGlyphTiles.sorted(), scale: scale, offset: offset, viewport: viewport, backing: backing))
                    }
                }
            }
        }
        let residualDiagnostic = ProcessInfo.processInfo.environment["FCB_ZOOM_RESIDUAL_DIAGNOSTIC"] == "1"
        let repetitions = residualDiagnostic ? 3 : 20
        try output(["phase": "zoom-protocol", "mode": mode, "rendering_mode": captureMode ?? "nativeDisplay", "repetitions": repetitions, "frames": 120,
            "scale_low": low, "scale_high": high, "max_step_log_scale": log(high / low) * 2 / 119,
            "viewport": [viewport.width, viewport.height], "backing": backing,
            "warm_retained_surface": true, "pid": ProcessInfo.processInfo.processIdentifier])
        let sampleRequested = ProcessInfo.processInfo.environment["FCB_ZOOM_SAMPLE"] == "1"
        var sampler: Process?
        for run in -1..<(repetitions + (sampleRequested ? 3 : 0)) {
            if run == repetitions {
                let path = FileManager.default.temporaryDirectory.appendingPathComponent("fcb-zoom-sample-\(UUID()).txt").path
                let process = Process(); process.executableURL = URL(fileURLWithPath: "/usr/bin/sample")
                process.arguments = [String(ProcessInfo.processInfo.processIdentifier), "5", "10", "-file", path]
                try process.run(); sampler = process
                try output(["phase": "zoom-sample", "path": path, "seconds": 5, "interval_ms": 10])
            }
            var rows: [[String: Any]] = []
            let runStart = now()
            for frame in 0..<120 {
                let t = Double(frame) / 119, ramp = t <= 0.5 ? t * 2 : (1 - t) * 2
                let scale = low * pow(high / low, ramp)
                let offset = CGPoint(x: viewport.width / 2 - center.x * scale + sin(t * .pi * 2) * 80,
                                     y: viewport.height / 2 - center.y * scale + cos(t * .pi * 2) * 60)
                // Copy only geometry and IDs; do not extend the old CGImage's
                // lifetime across the measured request or replacement allocation.
                let prior: (CGRect, Double, [Int])? = {
                    guard residualDiagnostic, let image = surface.retainedTransitionImage else { return nil }
                    return (image.rect, min(Double(image.image.width) / image.rect.width,
                        Double(image.image.height) / image.rect.height), image.tiles)
                }()
                let priorContains = prior.map { $0.0.contains(CGRect(x: -offset.x / scale, y: -offset.y / scale,
                    width: viewport.width / scale, height: viewport.height / scale)) } ?? false
                let priorDense = (prior?.1 ?? 0) >= scale * backing
                let before = surface.metrics, start = now()
                surface.update(tiles: tiles, revision: revision, scale: scale, offset: offset, selectedPath: nil, hitPaths: [])
                let requestEnd = now()
                let deadline = now() + 10
                while !(accepted?.0 == scale && accepted?.1 == offset && (accepted?.2 ?? 0) >= start) {
                    _ = pumpEvents(until: Date(timeIntervalSinceNow: 0.001))
                    if accepted?.0 == scale && accepted?.1 == offset,
                       let callbackTime = accepted?.2, callbackTime >= start && callbackTime <= deadline { break }
                    if now() >= deadline {
                        try output(["phase": "zoom-accept-deadline", "run": run, "frame": frame,
                            "presenter": surface.metal?.diagnosticState ?? [:],
                            "scale": scale, "visible": window.isVisible,
                            "occlusion_visible": window.occlusionState.contains(.visible),
                            "app_active": app.isActive, "key_window": window.isKeyWindow,
                            "submissions": surface.metalSubmissions, "presentations": surface.metalPresentations,
                            "gpu_tiles": surface.gpuCoveredTiles.count])
                        preconditionFailure("production callback must accept camera")
                    }
                }
                let drainedAt = now(), acceptedAt = accepted!.2, after = surface.metrics
                var row: [String: Any] = ["frame": frame, "timestamp_s": start, "scale": scale, "offset": [offset.x, offset.y],
                    "request_ms": (requestEnd - start) * 1000, "accept_ms": (acceptedAt - start) * 1000,
                    "wait_return_ms": (drainedAt - start) * 1000,
                    "transition_rasters": after.transitionRasters - before.transitionRasters,
                    "transition_ms": (after.transitionSeconds - before.transitionSeconds) * 1000,
                    "detail_rasters": after.detailRasters - before.detailRasters,
                    "detail_ms": (after.detailSeconds - before.detailSeconds) * 1000,
                    "transition_bytes": after.transitionBytes, "gpu_tiles": surface.gpuCoveredTiles.count]
                if residualDiagnostic {
                    let newTiles = surface.retainedTransitionImage?.tiles ?? []
                    row["prior_transition"] = prior != nil; row["prior_contains_next"] = priorContains
                    row["prior_density_adequate"] = priorDense; row["prior_transition_density"] = prior?.1 ?? 0
                    row["prior_transition_rect"] = prior.map { [$0.0.minX, $0.0.minY, $0.0.width, $0.0.height] } ?? []
                    row["prior_transition_tiles"] = prior?.2.count ?? 0; row["new_transition_tiles"] = newTiles.count
                    row["new_transition_tile_ids"] = Array(Set(newTiles).subtracting(prior?.2 ?? [])).sorted()
                }
                rows.append(row)
                if run == -1 && frame % 15 == 0 { try verify(scale, offset, exact: true) }
                // Fixed request pacing when work fits; misses remain visible in raw timestamps.
                let due = start + 1.0 / 60
                while now() < due { _ = pumpEvents(until: Date(timeIntervalSinceNow: due - now())) }
            }
            if run == -1 {
                let settle = now() + 10
                while surface.hasPendingDetail && now() < settle {
                    _ = pumpEvents(until: Date(timeIntervalSinceNow: 0.001))
                }
                precondition(!surface.hasPendingDetail, "validation warmup must settle before measurement")
            }
            try output(["phase": "zoom-trajectory", "mode": mode, "run": run, "sampled": run >= repetitions, "validation": run == -1,
                "wall_ms": (now() - runStart) * 1000, "frames": rows,
                "window_active": app.isActive, "window_occlusion_visible": window.occlusionState.contains(.visible)])
        }
        if let sampler { sampler.waitUntilExit(); precondition(sampler.terminationStatus == 0) }
        try reportMetalPresentationTimes(surface)
        surface.stopClock(); window.orderOut(nil)
    }
}

extension AtlasRenderProfile {
    // Independent rectangle-union oracle: credit only actual adequate visible
    // images; overlapping levels never double-count coverage.
    static func sharpPatchCoverage(_ patches: [(tile: Int, rect: CGRect, level: Int, image: CGImage, visible: Bool, currentTier: Bool)],
                                   tile: AtlasTextTile, index: Int, target: CGRect, density: Double) -> Bool {
        let adequate = patches.filter { $0.visible && $0.tile == index &&
            min(Double($0.image.width) / $0.rect.width, Double($0.image.height) / $0.rect.height) >= density }
        for group in [adequate] {
            let rects = group.map { $0.rect.offsetBy(dx: tile.rect.minX, dy: tile.rect.minY).intersection(target) }
                .filter { !$0.isNull && !$0.isEmpty }
            let xs = Array(Set([target.minX, target.maxX] + rects.flatMap { [$0.minX, $0.maxX] })).sorted()
            var area = 0.0
            for (a, b) in zip(xs, xs.dropFirst()) {
                let mid = (a + b) / 2
                let spans = rects.filter { $0.minX <= mid && $0.maxX >= mid }.sorted { $0.minY < $1.minY }
                var covered = 0.0, end = target.minY
                for rect in spans {
                    covered += max(0, rect.maxY - max(end, rect.minY)); end = max(end, rect.maxY)
                }
                area += (b - a) * covered
            }
            if area >= target.width * target.height * (1 - 1e-9) { return true }
        }
        return false
    }

    @MainActor static func sharpFallbackReuseRegression() throws {
        let pressure = ProcessInfo.processInfo.environment["FCB_ZOOM_SHARP_REUSE_PRESSURE"] == "1"
        let text = String(repeating: "let preserved_source = 123 // Monokai\n", count: pressure ? 60 : 40)
        let capture = AtlasHighlightCapture(schema: "fcb.source-document/1", text: text,
            runs: [.init(start: "0", length: String(text.utf16.count), role: "keyword")])
        let tile = AtlasDocument(path: "real-shaped.swift", capture: capture)!.tiles[0]
        let width = pressure ? 6000.0 : AtlasTextTile.width
        tile.rect = CGRect(x: 0, y: 0, width: width, height: tile.height * width / AtlasTextTile.width)
        tile.prepareRaster(pixelBudget: 1)
        let surface = AtlasRetainedSurface(metalEnabled: false)
        surface.frame = CGRect(x: 0, y: 0, width: pressure ? 5200 : 200, height: pressure ? 5200 : 100)
        surface.setBackingScale(1)
        let revision = UUID(), center = pressure ? CGPoint(x: 3000, y: 4000) : CGPoint(x: 250, y: 200)
        func offset(_ scale: Double) -> CGPoint {
            CGPoint(x: surface.bounds.width / 2 - center.x * scale, y: surface.bounds.height / 2 - center.y * scale)
        }
        surface.update(tiles: [tile], revision: revision, scale: 1, offset: offset(1), selectedPath: nil, hitPaths: [])
        for _ in 0..<10000 where surface.hasPendingDetail { surface.prepareDetailPass() }
        precondition(!surface.hasPendingDetail)
        let before = surface.metrics
        if pressure { surface.frame.size = CGSize(width: 3900, height: 3900) }
        surface.update(tiles: [tile], revision: revision, scale: 0.75, offset: offset(0.75), selectedPath: nil, hitPaths: [])
        let target = CGRect(x: -offset(0.75).x / 0.75, y: -offset(0.75).y / 0.75, width: surface.bounds.width / 0.75, height: surface.bounds.height / 0.75)
        let fallback = surface.retainedDetailImages.filter { $0.visible && !$0.currentTier }
        precondition(!fallback.isEmpty && Dictionary(grouping: fallback, by: { $0.level }).values.contains {
            sharpPatchCoverage($0, tile: tile, index: 0, target: target, density: 0.75)
        },
                     "real sharper retained level must cover the new viewport")
        for patch in fallback {
            let image = patch.image
            let bitmap = CGContext(data: nil, width: image.width, height: image.height, bitsPerComponent: 8,
                bytesPerRow: image.width * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
            bitmap.translateBy(x: 0, y: Double(image.height))
            bitmap.scaleBy(x: Double(image.width) / patch.rect.width, y: -Double(image.height) / patch.rect.height)
            bitmap.translateBy(x: -patch.rect.minX, y: -patch.rect.minY)
            bitmap.scaleBy(x: tile.contentScale, y: tile.contentScale)
            tile.draw(in: bitmap)
            precondition(bitmap.makeImage()!.dataProvider!.data! as Data == image.dataProvider!.data! as Data,
                         "credited fallback must equal complete original text pixels")
        }
        try output(["phase": "sharp-fallback-reuse", "pressure": pressure, "fallback_patches": fallback.count,
                    "transition_rasters": surface.metrics.transitionRasters - before.transitionRasters])
        precondition(surface.metrics.transitionRasters == before.transitionRasters,
                     "fully sharp retained source must avoid redundant transition raster")
        precondition(surface.retainedTransitionImage == nil)
        var pressureWitness = false
        for _ in 0..<10000 where surface.hasPendingDetail {
            surface.prepareDetailPass()
            if surface.metrics.detailEvictions > before.detailEvictions ||
               surface.metrics.visibleAllocationRefusals > before.visibleAllocationRefusals ||
               surface.metrics.prefetchAllocationRefusals > before.prefetchAllocationRefusals {
                pressureWitness = pressureWitness || surface.retainedDetailImages.contains { $0.visible && !$0.currentTier }
            }
            precondition(surface.metrics.detailBytes <= AtlasRetainedSurface.residentByteLimit)
            precondition(sharpPatchCoverage(surface.retainedDetailImages, tile: tile, index: 0,
                target: target, density: 0.75), "detail replacement must retain complete sharp coverage")
        }
        precondition(!surface.hasPendingDetail)
        if pressure { precondition(pressureWitness, "real pressure must select victims or refuse while sharper fallback remains visible") }
        try output(["phase": "sharp-fallback-reuse-complete", "pressure": pressure,
                    "pressure_witness": pressureWitness, "detail_bytes": surface.metrics.detailBytes,
                    "evictions": surface.metrics.detailEvictions - before.detailEvictions,
                    "visible_refusals": surface.metrics.visibleAllocationRefusals - before.visibleAllocationRefusals,
                    "prefetch_refusals": surface.metrics.prefetchAllocationRefusals - before.prefetchAllocationRefusals])
        surface.stopClock()
    }
}

extension AtlasRenderProfile {
    @MainActor static func transitionEnvelopeRegression() throws {
        for large in [false, true] {
            let size = large ? CGSize(width: 3290, height: 1660) : CGSize(width: 1476, height: 744)
            let density = large ? 2.0 : 1.0
            let text = String(repeating: "let actual_neighbor = 123 // colored source\n", count: 70)
            let capture = AtlasHighlightCapture(schema: "fcb.source-document/1", text: text,
                runs: [.init(start: "0", length: String(text.utf16.count), role: "keyword")])
            let tiles = ["first.swift", "neighbor.swift"].map { AtlasDocument(path: $0, capture: capture)!.tiles[0] }
            tiles[0].rect = CGRect(x: 0, y: 0, width: size.width, height: tiles[0].height * size.width / AtlasTextTile.width)
            tiles[1].rect = CGRect(x: size.width + 4, y: 0, width: 648, height: tiles[1].height)
            for tile in tiles { tile.prepareRaster(pixelBudget: 1) }
            let surface = AtlasRetainedSurface(metalEnabled: false)
            surface.frame = CGRect(origin: .zero, size: size); surface.setBackingScale(density)
            let revision = UUID()
            surface.update(tiles: tiles, revision: revision, scale: 1, offset: .zero, selectedPath: nil, hitPaths: [])
            guard let first = surface.retainedTransitionImage else { preconditionFailure("sharp initial transition required") }
            func verify(_ scale: Double) {
                guard let snapshot = surface.retainedTransitionImage else { preconditionFailure("sharp transition retained") }
                let view = CGRect(x: 0, y: 0, width: size.width / scale, height: size.height / scale)
                precondition(snapshot.rect.contains(view))
                let image = snapshot.image, canvas = snapshot.rect
                precondition(min(Double(image.width) / canvas.width, Double(image.height) / canvas.height) >= scale * density)
                for (index, tile) in tiles.enumerated() where tile.rect.intersects(view) {
                    precondition(snapshot.tiles.contains(index), "newly exposed neighbor must contain source text")
                }
                let bitmap = CGContext(data: nil, width: image.width, height: image.height, bitsPerComponent: 8,
                    bytesPerRow: image.width * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
                bitmap.translateBy(x: 0, y: Double(image.height))
                bitmap.scaleBy(x: Double(image.width) / canvas.width, y: -Double(image.height) / canvas.height)
                bitmap.translateBy(x: -canvas.minX, y: -canvas.minY)
                for index in snapshot.tiles {
                    let tile = tiles[index], clip = tile.rect.intersection(canvas)
                    bitmap.saveGState(); bitmap.clip(to: clip); bitmap.setFillColor(Monokai.background.cgColor); bitmap.fill(clip)
                    bitmap.translateBy(x: tile.rect.minX, y: tile.rect.minY)
                    bitmap.scaleBy(x: tile.contentScale, y: tile.contentScale); tile.draw(in: bitmap); bitmap.restoreGState()
                }
                precondition(bitmap.makeImage()!.dataProvider!.data! as Data == image.dataProvider!.data! as Data,
                             "entire guarded canvas contains exact complete source pixels")
                precondition(surface.metrics.transitionFailures == 0 &&
                    surface.metrics.transitionBytes * 3 <= AtlasRetainedSurface.transitionByteLimit)
            }
            verify(1)
            let inBandOffset = CGPoint(x: 12, y: 0)
            let inBandView = CGRect(x: -12, y: 0, width: size.width, height: size.height)
            precondition(first.rect.contains(inBandView), "small pan must stay within original guardband")
            let initialRasters = surface.metrics.transitionRasters
            surface.update(tiles: tiles, revision: revision, scale: 1, offset: inBandOffset, selectedPath: nil, hitPaths: [])
            precondition(surface.metrics.transitionRasters == initialRasters &&
                surface.retainedTransitionImage!.image === first.image,
                "in-envelope pan with unchanged visible source must reuse the exact bitmap")
            let before = surface.metrics
            surface.update(tiles: tiles, revision: revision, scale: 0.94, offset: .zero, selectedPath: nil, hitPaths: [])
            verify(0.94)
            let expandedView = CGRect(x: 0, y: 0, width: size.width / 0.94, height: size.height / 0.94)
            precondition(!first.rect.contains(expandedView),
                         "six-percent zoom-out must exceed the original 32-point guardband")
            precondition(surface.metrics.transitionRasters > before.transitionRasters,
                         "exceeding the guardband requires a fresh exact raster")
            precondition(surface.retainedTransitionImage!.image !== first.image,
                         "newly visible source must not reuse an incomplete image")
            if large {
                precondition(first.rect.width <= size.width + 64.000001,
                             "oversized envelope must use bounded original guardband")
            } else {
                precondition(!first.tiles.contains(1),
                             "initial viewport must not count an offscreen neighbor as painted")
                precondition(surface.retainedTransitionImage!.tiles.contains(1),
                             "fresh image must paint the newly visible neighbor")
            }
            try output(["phase": "transition-envelope", "large_admission_fallback": large,
                        "reraster_delta": surface.metrics.transitionRasters - before.transitionRasters,
                        "bytes": surface.metrics.transitionBytes, "painted_tiles": surface.retainedTransitionImage!.tiles.count])
            surface.stopClock()
        }
    }
}

// Profiling-only, opt-in real AppKit event loop. Unlike the acceptance-gated
// correctness replay, missed timer deadlines advance camera time and expose stalls.

@MainActor final class AtlasCadenceDriver: NSObject, NSApplicationDelegate {
    let tiles: [AtlasTextTile]
    let bounds: CGRect
    let surface = AtlasRetainedSurface()
    let revision = UUID()
    var window: NSWindow!
    var timer: Timer?
    var start = 0.0
    var ready = 0.0
    var finishing = false
    var sampler: Process?
    var frames: [[String: Any]] = []
    var accepted: [[String: Any]] = []
    let repetitions = max(1, min(100, Int(ProcessInfo.processInfo.environment["FCB_CADENCE_RUNS"] ?? "20") ?? 20))
    var lastScale = 0.05
    var lastOffset = CGPoint.zero
    init(tiles: [AtlasTextTile], bounds: CGRect) { self.tiles = tiles; self.bounds = bounds }
    func applicationDidFinishLaunching(_ notification: Notification) {
        let size = AtlasRenderProfile.viewport
        window = NSWindow(contentRect: CGRect(origin: .zero, size: size), styleMask: [.titled], backing: .buffered, defer: false)
        window.title = "FCB presentation cadence"
        surface.frame = CGRect(origin: .zero, size: size)
        window.contentView = surface
        window.center(); window.makeKeyAndOrderFront(nil)
        NSApplication.shared.activate(ignoringOtherApps: true)
        precondition(window.backingScaleFactor == AtlasRenderProfile.backing,
            "Profile backing must match the actual display")
        surface.setBackingScale(AtlasRenderProfile.backing)
        surface.onPresentedFrame = { [weak self] revision, scale, offset in
            guard let self else { return }
            self.accepted.append(["time": AtlasRenderProfile.now(), "scale": scale,
                "offset": [offset.x, offset.y], "revision": revision.uuidString])
        }
        // Cold scene/upload work must not consume the warmup or measured runs.
        surface.update(tiles: tiles, revision: revision, scale: 0.05,
            offset: CGPoint(x: size.width / 2 - bounds.midX * 0.05,
                            y: size.height / 2 - bounds.midY * 0.05), selectedPath: nil, hitPaths: [])
        ready = AtlasRenderProfile.now()
        timer = Timer(timeInterval: 1.0 / 60, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.tick() }
        }
        RunLoop.main.add(timer!, forMode: .common)
    }
    func tick() {
        guard !finishing else { return }
        let now = AtlasRenderProfile.now()
        let active = NSApplication.shared.isActive
        let visible = window.occlusionState.contains(.visible)
        if start == 0 {
            guard active && visible else {
                if now - ready > 120 { finish(failure: "activation deadline") }
                return
            }
            start = now
        }
        let elapsed = now - start
        // One complete four-second trajectory warms the retained surface. Camera targets
        // advance by wall time even if the main run loop misses timer deadlines.
        if elapsed >= 4 + Double(repetitions) * 4 { finish(failure: nil); return }
        if elapsed >= 4, sampler == nil,
           let path = ProcessInfo.processInfo.environment["FCB_CADENCE_SAMPLE"] {
            let process = Process(); process.executableURL = URL(fileURLWithPath: "/usr/bin/sample")
            process.arguments = [String(ProcessInfo.processInfo.processIdentifier), "15", "2", "-file", path]
            try! process.run(); sampler = process
        }
        let phase = elapsed.truncatingRemainder(dividingBy: 4) / 4
        let ramp = phase <= 0.5 ? phase * 2 : (1 - phase) * 2
        let scale = 0.05 * pow(1.6 / 0.05, ramp)
        let size = AtlasRenderProfile.viewport
        let offset = CGPoint(x: size.width / 2 - bounds.midX * scale + sin(phase * .pi * 2) * 80,
                             y: size.height / 2 - bounds.midY * scale + cos(phase * .pi * 2) * 60)
        let metrics = surface.metrics
        let before = AtlasRenderProfile.now()
        surface.update(tiles: tiles, revision: revision, scale: scale, offset: offset,
                       selectedPath: nil, hitPaths: [])
        let after = AtlasRenderProfile.now()
        lastScale = scale; lastOffset = offset
        frames.append(["time": now, "elapsed": elapsed, "request_ms": (after - before) * 1000,
            "scale": scale, "active": active, "visible": visible, "gpu_tiles": surface.gpuCoveredTiles.count,
            "run": Int(elapsed / 4) - 1,
            "transition_ms": (surface.metrics.transitionSeconds - metrics.transitionSeconds) * 1000,
            "detail_ms": (surface.metrics.detailSeconds - metrics.detailSeconds) * 1000,
            "transition_rasters": surface.metrics.transitionRasters - metrics.transitionRasters,
            "detail_rasters": surface.metrics.detailRasters - metrics.detailRasters,
            "offset": [offset.x, offset.y]])
    }
    func finish(failure: String?) {
        guard !finishing else { return }
        finishing = true
        timer?.invalidate(); timer = nil
        // Allow already-submitted work and actual displayed callbacks to retire.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [self] in
            let metal = surface.metal!
            let result: [String: Any] = ["phase": "native-cadence", "failure": failure ?? "none",
                "start": start, "repetitions": repetitions, "period_seconds": 4,
                "warmup_seconds": 4, "frames": frames, "accepted": accepted,
                "measurement": "CPU request and acceptance timing; positive presentedTime only is presentation timing",
                "viewport": [AtlasRenderProfile.viewport.width, AtlasRenderProfile.viewport.height],
                "backing": AtlasRenderProfile.backing,
                "presented": metal.presentationSamples.map { ["time": $0.time, "serial": $0.serial] },
                "delivery": metal.deliverySamples.map { ["serial": Double($0.serial),
                    "callback": $0.callbackTime, "deadline": $0.deadline,
                    "target_presentation": $0.targetPresentation, "encoded": $0.encodedTime,
                    "gpu_start": $0.gpuStart, "gpu_end": $0.gpuEnd, "present_call": $0.presentCall] },
                "present_calls": metal.presentations, "callbacks": metal.presentationCallbacks,
                "zero_callbacks": metal.skippedPresentationCallbacks, "gpu_seconds": metal.gpuSeconds,
                "maximum_in_flight": metal.maximumInFlight, "failures": metal.failures,
                "diagnostic": metal.diagnosticState,
                "transition_failures": surface.metrics.transitionFailures,
                "transition_rasters": surface.metrics.transitionRasters,
                "detail_rasters": surface.metrics.detailRasters]
            try! AtlasRenderProfile.output(result)
            fflush(stdout)
            surface.stopClock()
            NSApplication.shared.terminate(nil)
        }
    }
}

extension AtlasRenderProfile {
    @MainActor static func nativeCadence(tiles: [AtlasTextTile], bounds: CGRect) {
        let app = NSApplication.shared
        app.setActivationPolicy(.regular)
        let driver = AtlasCadenceDriver(tiles: tiles, bounds: bounds)
        app.delegate = driver
        withExtendedLifetime(driver) { app.run() }
    }
}
