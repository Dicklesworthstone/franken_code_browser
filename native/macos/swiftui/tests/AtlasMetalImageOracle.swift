import Foundation
import AppKit
import CoreGraphics
import Metal

/// Test-only readback of the same retained scene used by the native surface.
/// The CPU reference draws original prepared source, not GPU atlas data.
@MainActor enum AtlasMetalImageOracle {
    static func compareFixedCapture(scene: AtlasMetalGlyphRenderer, tiles: [AtlasTextTile], id: Int,
                                    sourceDensity: Double, phase: Double) throws -> [String: Any] {
        let tile = tiles[id], captureDensity = 4.0
        let captureWidth = Int(ceil(AtlasTextTile.width * captureDensity))
        let captureHeight = Int(ceil(tile.height * captureDensity))
        precondition(captureWidth * captureHeight <= 8 * 1024 * 1024)
        let bitmap = CGContext(data: nil, width: captureWidth, height: captureHeight, bitsPerComponent: 8,
            bytesPerRow: captureWidth * 4, space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        bitmap.setFillColor(Monokai.background.cgColor)
        bitmap.fill(CGRect(x: 0, y: 0, width: captureWidth, height: captureHeight))
        bitmap.translateBy(x: 0, y: Double(captureHeight)); bitmap.scaleBy(x: captureDensity, y: -captureDensity)
        bitmap.clip(to: CGRect(x: 0, y: 0, width: AtlasTextTile.width, height: tile.height))
        tile.draw(in: bitmap)
        let source = Array(bitmap.makeImage()!.dataProvider!.data! as Data)
        let scale = sourceDensity / tile.contentScale
        let offset = CGPoint(x: 12 + phase - tile.rect.minX * scale, y: 8 + phase - tile.rect.minY * scale)
        let device = MTLCreateSystemDefaultDevice()!, queue = device.makeCommandQueue()!
        let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .bgra8Unorm, width: 512, height: 256, mipmapped: false)
        descriptor.storageMode = .shared; descriptor.usage = .renderTarget
        let texture = device.makeTexture(descriptor: descriptor)!, command = queue.makeCommandBuffer()!
        try scene.encode(into: texture, commandBuffer: command, scale: scale, offset: offset, tileIDs: [id])
        command.commit(); command.waitUntilCompleted()
        precondition(command.status == .completed && command.error == nil)
        var actual = [UInt8](repeating: 0, count: 512 * 256 * 4), expected = actual
        actual.withUnsafeMutableBytes { texture.getBytes($0.baseAddress!, bytesPerRow: 512 * 4,
            from: MTLRegionMake2D(0, 0, 512, 256), mipmapLevel: 0) }
        var error = 0.0, energy = 0.0, actualEnergy = 0.0
        var rowErrors = [Double](repeating: 0, count: tile.lineCount + 2), rowEnergies = rowErrors
        for y in 0..<256 { for x in 0..<512 {
            let x0 = (Double(x) - 12 - phase) * captureDensity / sourceDensity
            let y0 = (Double(y) - 8 - phase) * captureDensity / sourceDensity
            let x1 = x0 + captureDensity / sourceDensity, y1 = y0 + captureDensity / sourceDensity
            guard x0 >= 0, y0 >= 0, x1 <= Double(captureWidth), y1 <= tile.height * captureDensity else { continue }
            let pixel = (y * 512 + x) * 4
            precondition(actual[pixel + 3] == 255, "Admitted source interior remains opaque")
            var sums = [Double](repeating: 0, count: 3)
            for sy in Int(floor(y0))..<Int(ceil(y1)) {
                let wy = min(y1, Double(sy + 1)) - max(y0, Double(sy))
                for sx in Int(floor(x0))..<Int(ceil(x1)) {
                    let area = wy * (min(x1, Double(sx + 1)) - max(x0, Double(sx)))
                    for channel in 0..<3 { sums[channel] += Double(source[(sy * captureWidth + sx) * 4 + channel]) * area }
                }
            }
            let row = max(0, min(tile.lineCount + 1, Int(((y0 + y1) / (2 * captureDensity) - 22) / AtlasTextTile.lineHeight) + 1))
            for channel in 0..<3 {
                let value = (sums[channel] / ((x1 - x0) * (y1 - y0))).rounded()
                expected[pixel + channel] = UInt8(max(0, min(255, value.rounded())))
                let delta = abs(Double(actual[pixel + 2 - channel]) - value)
                let ink = abs(value - Double([22, 26, 29][channel]))
                error += delta; energy += ink; rowErrors[row] += delta; rowEnergies[row] += ink
                actualEnergy += abs(Double(actual[pixel + 2 - channel]) - Double([22, 26, 29][channel]))
            }
            expected[pixel + 3] = 255
        } }
        precondition(energy > 0)
        let maximum = rowErrors.indices.filter { rowEnergies[$0] > 100 }.map { rowErrors[$0] / rowEnergies[$0] }.max()!
        let result: [String: Any] = ["phase": "corpus-fixed-capture", "tile": id, "path": tile.path,
            "source_density": sourceDensity, "camera_phase": phase, "rgb_error": error / energy,
            "ink_ratio": actualEnergy / energy, "max_row_error": maximum, "source_rows": tile.lineCount,
            "capture_pixels": captureWidth * captureHeight, "source_scale": tile.contentScale,
            "tile_rect": [tile.rect.minX, tile.rect.minY, tile.rect.width, tile.rect.height]]
        let failed = error / energy > 0.12 || maximum > 0.12
        let retainPreview = ProcessInfo.processInfo.environment["FCB_RGB_CORPUS_ARTIFACTS"] == "1"
            && phase == 0 && (sourceDensity == 1 || sourceDensity == 0.2)
        if failed || retainPreview {
            let directory = FileManager.default.temporaryDirectory.appendingPathComponent("fcb-corpus-fixed-\(UUID())")
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
            try Data(actual).write(to: directory.appendingPathComponent("gpu.bgra"))
            try Data(expected).write(to: directory.appendingPathComponent("reference.rgba"))
            try Data(source).write(to: directory.appendingPathComponent("capture.rgba"))
            try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys]).write(to: directory.appendingPathComponent("result.json"))
            FileHandle.standardError.write(Data("CORPUS_FIXED_ARTIFACT failed=\(failed) raw=\(directory.path) width=512 height=256 capture_width=\(captureWidth) capture_height=\(captureHeight) result=\(result)\n".utf8))
        }
        precondition(!failed, "Actual project glyphs must match full captured source under camera filtering")
        return result
    }

    static func compareFixedComposition(scene: AtlasMetalGlyphRenderer, tiles: [AtlasTextTile], ids: [Int],
                                        scale: Double, offset: CGPoint, viewport: CGSize, backing: Double) throws -> [String: Any] {
        precondition(ids.count >= 2 && ids.count <= 4)
        let width = Int(ceil(viewport.width * backing)), height = Int(ceil(viewport.height * backing))
        let device = MTLCreateSystemDefaultDevice()!, queue = device.makeCommandQueue()!
        let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .bgra8Unorm, width: width, height: height, mipmapped: false)
        descriptor.storageMode = .shared; descriptor.usage = .renderTarget
        let texture = device.makeTexture(descriptor: descriptor)!, command = queue.makeCommandBuffer()!
        try scene.encode(into: texture, commandBuffer: command, scale: scale * backing,
            offset: CGPoint(x: offset.x * backing, y: offset.y * backing), tileIDs: ids)
        command.commit(); command.waitUntilCompleted()
        precondition(command.status == .completed && command.error == nil)
        var actual = [UInt8](repeating: 0, count: width * height * 4), expected = actual
        actual.withUnsafeMutableBytes { texture.getBytes($0.baseAddress!, bytesPerRow: width * 4,
            from: MTLRegionMake2D(0, 0, width, height), mipmapLevel: 0) }
        var ratios: [Double] = [], boundaryRatios: [Double] = []
        var totalError = 0.0, totalEnergy = 0.0, boundaryPixels = 0
        var sourcePixels = 0
        for id in ids {
            let tile = tiles[id], density = scale * backing * tile.contentScale
            let cw = Int(ceil(AtlasTextTile.width * 4)), ch = Int(ceil(tile.height * 4))
            precondition(cw * ch <= 8 * 1024 * 1024)
            sourcePixels += cw * ch
            let context = CGContext(data: nil, width: cw, height: ch, bitsPerComponent: 8, bytesPerRow: cw * 4,
                space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
            context.setFillColor(Monokai.background.cgColor); context.fill(CGRect(x: 0, y: 0, width: cw, height: ch))
            context.translateBy(x: 0, y: Double(ch)); context.scaleBy(x: 4, y: -4)
            context.clip(to: CGRect(x: 0, y: 0, width: AtlasTextTile.width, height: tile.height)); tile.draw(in: context)
            let source = Array(context.makeImage()!.dataProvider!.data! as Data)
            let left = (tile.rect.minX * scale + offset.x) * backing
            let top = (tile.rect.minY * scale + offset.y) * backing
            let right = (tile.rect.maxX * scale + offset.x) * backing
            let bottom = (tile.rect.maxY * scale + offset.y) * backing
            let loX = max(0, min(width, Int(floor(left)))), hiX = max(0, min(width, Int(ceil(right))))
            let loY = max(0, min(height, Int(floor(top)))), hiY = max(0, min(height, Int(ceil(bottom))))
            var error = 0.0, energy = 0.0, edgeError = 0.0, edgeEnergy = 0.0
            for y in loY..<hiY { for x in loX..<hiX {
                // Full output support is integrated, but ownership is the same
                // pixel-center half-open parcel rule as the compositor.
                guard Double(x) + 0.5 >= left, Double(x) + 0.5 < right,
                      Double(y) + 0.5 >= top, Double(y) + 0.5 < bottom else { continue }
                let pixel = (y * width + x) * 4
                precondition(expected[pixel + 3] == 0, "Selected parcel interiors must not overlap")
                let x0 = (Double(x) - left) * 4 / density, y0 = (Double(y) - top) * 4 / density
                let x1 = x0 + 4 / density, y1 = y0 + 4 / density
                let area = (x1 - x0) * (y1 - y0)
                let edge = x0 < 0 || y0 < 0 || x1 > Double(cw) || y1 > tile.height * 4
                if edge { boundaryPixels += 1 }
                var sums = [22.0 * area, 26.0 * area, 29.0 * area]
                let sx0 = max(0, min(cw, Int(floor(x0)))), sx1 = max(0, min(cw, Int(ceil(x1))))
                let sy0 = max(0, min(ch, Int(floor(y0)))), sy1 = max(0, min(ch, Int(ceil(y1))))
                for sy in sy0..<sy1 {
                    let wy = max(0, min(y1, Double(sy + 1), tile.height * 4) - max(y0, Double(sy)))
                    for sx in sx0..<sx1 {
                        let weight = wy * max(0, min(x1, Double(sx + 1)) - max(x0, Double(sx)))
                        for c in 0..<3 { sums[c] += (Double(source[(sy * cw + sx) * 4 + c]) - Double([22, 26, 29][c])) * weight }
                    }
                }
                for c in 0..<3 {
                    let value = max(0, min(255, (sums[c] / area).rounded()))
                    expected[pixel + c] = UInt8(value)
                    let delta = abs(Double(actual[pixel + 2 - c]) - value), ink = abs(value - Double([22, 26, 29][c]))
                    error += delta; energy += ink
                    if edge { edgeError += delta; edgeEnergy += ink }
                }
                expected[pixel + 3] = 255
            } }
            precondition(energy > 0, "Every selected composition tile must contribute actual source ink")
            ratios.append(error / energy); boundaryRatios.append(edgeError / max(1, edgeEnergy))
            totalError += error; totalEnergy += energy
        }
        var alphaMismatches = 0, leakedPixels = 0
        for pixel in 0..<(width * height) {
            let p = pixel * 4
            if actual[p + 3] != expected[p + 3] { alphaMismatches += 1 }
            if expected[p + 3] == 0 && actual[p..<(p + 4)].contains(where: { $0 != 0 }) { leakedPixels += 1 }
        }
        precondition(boundaryPixels > 0, "Composition must exercise source-boundary filter footprints")
        let result: [String: Any] = ["phase": "accepted-camera-fixed-composition", "tile_ids": ids,
            "scale": scale, "offset": [offset.x, offset.y], "backing": backing,
            "rgb_error": totalError / max(1, totalEnergy), "tile_errors": ratios,
            "boundary_errors": boundaryRatios, "boundary_pixels": boundaryPixels,
            "alpha_mismatches": alphaMismatches, "outside_leaked_pixels": leakedPixels,
            "capture_pixels": sourcePixels, "actual_presented_frame_comparison": false]
        let failed = alphaMismatches != 0 || leakedPixels != 0 || ratios.contains { $0 > 0.12 } || boundaryRatios.contains { $0 > 0.12 }
        if failed || ProcessInfo.processInfo.environment["FCB_RGB_CORPUS_ARTIFACTS"] == "1" {
            let directory = FileManager.default.temporaryDirectory.appendingPathComponent("fcb-fixed-composition-\(UUID())")
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
            try Data(actual).write(to: directory.appendingPathComponent("gpu.bgra"))
            try Data(expected).write(to: directory.appendingPathComponent("reference.rgba"))
            try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys]).write(to: directory.appendingPathComponent("result.json"))
            FileHandle.standardError.write(Data("FIXED_COMPOSITION_ARTIFACT failed=\(failed) raw=\(directory.path) width=\(width) height=\(height) result=\(result)\n".utf8))
        }
        precondition(!failed, "Actual-camera bounded composition must preserve source edges and reject outside ink")
        return result
    }

    static func compareFixedVisibleControl(scene: AtlasMetalGlyphRenderer, tiles: [AtlasTextTile], ids: [Int],
                                           scale: Double, backing: Double, phase: Double) throws -> [String: Any] {
        let bounded = ids.filter { Int(ceil(AtlasTextTile.width * 4)) * Int(ceil(tiles[$0].height * 4)) <= 8 * 1024 * 1024 }
        guard let id = bounded.first else {
            return ["phase": "connected-fixed-source-control", "selected_tiles": 0,
                    "limitation": "No visible admitted tile fits the bounded full-source capture oracle"]
        }
        var result = try compareFixedCapture(scene: scene, tiles: tiles, id: id,
            sourceDensity: scale * backing * tiles[id].contentScale, phase: phase)
        result["phase"] = "connected-fixed-source-control"
        result["actual_presented_frame_comparison"] = false
        return result
    }

    static func compare(scene: AtlasMetalGlyphRenderer, tiles: [AtlasTextTile], ids: [Int],
                        scale: Double, offset: CGPoint, viewport: CGSize, backing: Double) throws -> [String: Any] {
        let width = Int(ceil(viewport.width * backing)), height = Int(ceil(viewport.height * backing))
        let device = MTLCreateSystemDefaultDevice()!, queue = device.makeCommandQueue()!
        let desc = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .bgra8Unorm, width: width, height: height, mipmapped: false)
        desc.storageMode = .shared; desc.usage = .renderTarget
        let target = device.makeTexture(descriptor: desc)!, command = queue.makeCommandBuffer()!
        try scene.encode(into: target, commandBuffer: command, scale: scale * backing,
            offset: CGPoint(x: offset.x * backing, y: offset.y * backing), tileIDs: ids)
        command.commit(); command.waitUntilCompleted() // Test-only independent pixel oracle.
        precondition(command.status == .completed && command.error == nil)
        var gpu = [UInt8](repeating: 0, count: width * height * 4)
        gpu.withUnsafeMutableBytes { target.getBytes($0.baseAddress!, bytesPerRow: width * 4,
            from: MTLRegionMake2D(0, 0, width, height), mipmapLevel: 0) }
        let bitmap = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
            bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        bitmap.translateBy(x: 0, y: Double(height)); bitmap.scaleBy(x: backing, y: -backing)
        bitmap.translateBy(x: offset.x, y: offset.y); bitmap.scaleBy(x: scale, y: scale)
        for id in ids {
            let tile = tiles[id]
            bitmap.saveGState(); bitmap.clip(to: tile.rect)
            bitmap.setFillColor(Monokai.background.cgColor); bitmap.fill(tile.rect)
            bitmap.translateBy(x: tile.rect.minX, y: tile.rect.minY)
            bitmap.scaleBy(x: tile.contentScale, y: tile.contentScale)
            tile.draw(in: bitmap); bitmap.restoreGState()
        }
        let cpu = Array(bitmap.makeImage()!.dataProvider!.data! as Data)
        var gpuInk = [Bool](repeating: false, count: width * height)
        var cpuInk = gpuInk
        var absoluteError = 0, referenceInkEnergy = 0
        for p in gpuInk.indices {
            let i = p * 4
            let reference = abs(Int(cpu[i]) - 22) + abs(Int(cpu[i + 1]) - 26) + abs(Int(cpu[i + 2]) - 29)
            let actual = abs(Int(gpu[i + 2]) - 22) + abs(Int(gpu[i + 1]) - 26) + abs(Int(gpu[i]) - 29)
            cpuInk[p] = cpu[i + 3] > 240 && reference > 48
            gpuInk[p] = gpu[i + 3] > 240 && actual > 48
            if cpu[i + 3] > 240 && gpu[i + 3] > 240 {
                absoluteError += abs(Int(cpu[i]) - Int(gpu[i + 2])) + abs(Int(cpu[i + 1]) - Int(gpu[i + 1])) + abs(Int(cpu[i + 2]) - Int(gpu[i]))
                referenceInkEnergy += reference
            }
        }
        func nearby(_ bits: [Bool], _ p: Int) -> Bool {
            let x = p % width, y = p / width
            for dy in -1...1 { for dx in -1...1 {
                let a = x + dx, b = y + dy
                if a >= 0 && a < width && b >= 0 && b < height && bits[b * width + a] { return true }
            } }
            return false
        }
        var referenceCount = 0, actualCount = 0, referenceCovered = 0, actualCovered = 0
        for p in cpuInk.indices {
            if cpuInk[p] { referenceCount += 1; if nearby(gpuInk, p) { referenceCovered += 1 } }
            if gpuInk[p] { actualCount += 1; if nearby(cpuInk, p) { actualCovered += 1 } }
        }
        let recall = Double(referenceCovered) / Double(max(1, referenceCount))
        let precision = Double(actualCovered) / Double(max(1, actualCount))
        let error = Double(absoluteError) / Double(max(1, referenceInkEnergy))
        let result: [String: Any] = ["phase": "connected-metal-pixels", "scale": scale, "selected_tiles": ids.count,
            "reference_ink_pixels": referenceCount, "gpu_ink_pixels": actualCount,
            "ink_recall_one_pixel": recall, "ink_precision_one_pixel": precision, "normalized_rgb_error": error,
            "gpu_execution_ms": (command.gpuEndTime - command.gpuStartTime) * 1000]
        if !ids.isEmpty && (referenceCount == 0 || actualCount == 0 || recall < 0.90 || precision < 0.90 || error > 0.12) {
            let directory = FileManager.default.temporaryDirectory.appendingPathComponent("fcb-connected-metal-\(UUID())")
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
            try Data(gpu).write(to: directory.appendingPathComponent("gpu.bgra"))
            try Data(cpu).write(to: directory.appendingPathComponent("reference.rgba"))
            let selected: [[String: Any]] = ids.map { index in
                let tile = tiles[index]
                let lines = tile.preparedLines ?? tile.lines.compactMap { AtlasPreparedLine($0) }
                let runs: [[String: Any]] = lines.prefix(3).flatMap { $0.runs }.prefix(12).map { run in
                    ["font": CTFontCopyPostScriptName(run.font) as String,
                     "font_size": CTFontGetSize(run.font), "color": run.color.components ?? [],
                     "glyph_count": run.glyphs.count]
                }
                return ["id": index, "path": tile.path, "source_start": tile.sourceRange.location,
                        "source_length": tile.sourceRange.length, "source_scale": tile.contentScale,
                        "rect": [tile.rect.minX, tile.rect.minY, tile.rect.width, tile.rect.height], "runs": runs]
            }
            let metadata = try JSONSerialization.data(withJSONObject: ["scale": scale, "backing": backing,
                "offset": [offset.x, offset.y], "selected": selected], options: [.prettyPrinted, .sortedKeys])
            try metadata.write(to: directory.appendingPathComponent("selected-tiles.json"))
            let json = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
            FileHandle.standardError.write(json)
            FileHandle.standardError.write(Data("\nCONNECTED_METAL_PIXEL_MISMATCH raw=\(directory.path) width=\(width) height=\(height)\n".utf8))
            preconditionFailure("actual GPU scene must preserve source ink geometry and color")
        }
        return result
    }
}

extension AtlasRenderProfile {
    @MainActor static func reportMetalPresentationTimes(_ surface: AtlasRetainedSurface) throws {
        guard ProcessInfo.processInfo.environment["FCB_METAL_PRESENTATION_TIMING"] == "1", let metal = surface.metal else { return }
        let deadline = now() + 0.5
        while metal.presentationCallbacks < metal.presentations && now() < deadline {
            _ = pumpEvents(until: Date(timeIntervalSinceNow: 0.001))
        }
        let samples = metal.presentationSamples.sorted {
            $0.revision == $1.revision ? $0.time < $1.time : $0.revision.uuidString < $1.revision.uuidString }
        try output(["phase": "metal-presented-times", "accepted_present_calls": metal.presentations,
            "presentation_callbacks": metal.presentationCallbacks, "skipped_callbacks": metal.skippedPresentationCallbacks,
            "pending_callbacks": max(0, metal.presentations - metal.presentationCallbacks),
            "evicted_diagnostic_samples": max(0, metal.presentationCallbacks - metal.skippedPresentationCallbacks - samples.count),
            "retained_positive_samples": samples.count, "unique_presented_timestamps": Set(samples.map { $0.time }).count,
            "samples": samples.map { ["revision": $0.revision.uuidString, "serial": $0.serial, "presented_time_s": $0.time] as [String: Any] },
            "includes_oracle_and_idle_time": true, "whole_screen_fps": false])
    }

    @MainActor static func connectedMetal(tiles: [AtlasTextTile], bounds: CGRect) throws {
        if ProcessInfo.processInfo.environment["FCB_CONTINUOUS_ZOOM"] != nil {
            try continuousZoom(tiles: tiles, bounds: bounds)
            return
        }
        if ProcessInfo.processInfo.environment["FCB_RENDER_CPU_WINDOW"] == "1" {
            try connectedCPUWindow(tiles: tiles, bounds: bounds)
            return
        }
        let app = NSApplication.shared
        app.setActivationPolicy(.accessory); app.finishLaunching()
        let window = NSWindow(contentRect: CGRect(origin: .zero, size: viewport),
            styleMask: [.titled], backing: .buffered, defer: false)
        let captureMode = ProcessInfo.processInfo.environment["FCB_RETAINED_CAPTURE"]
        let surface = captureMode == "default" ? AtlasRetainedSurface() :
            AtlasRetainedSurface(metalRenderingMode: captureMode == "scalar" ? .retainedCoverage :
                (captureMode == nil ? .nativeDisplay : .retainedCapture))
        if captureMode == "default" {
            precondition(surface.metal?.renderingMode == .retainedCoverage,
                "The unconfigured production surface must select qualified retained coverage")
        }
        surface.frame = CGRect(origin: .zero, size: viewport)
        window.contentView = surface; window.center(); window.makeKeyAndOrderFront(nil)
        app.activate(ignoringOtherApps: true); window.orderFrontRegardless()
        surface.setBackingScale(backing)
        _ = pumpEvents(until: Date(timeIntervalSinceNow: 0.02))
        try output(["phase": "connected-metal-window", "visible": window.isVisible,
                    "miniaturized": window.isMiniaturized,
                    "occlusion_visible": window.occlusionState.contains(.visible),
                    "screen_name": window.screen?.localizedName ?? "none",
                    "screen_backing": window.screen?.backingScaleFactor ?? 0,
                    "screen_width": window.screen?.frame.width ?? 0,
                    "screen_height": window.screen?.frame.height ?? 0])
        let displayLinkOnly = ProcessInfo.processInfo.environment["FCB_GPU_DISPLAYLINK"] == "1"
        let revision = UUID()
        var receipt: (UUID, Double, CGPoint, Double)?
        surface.onPresentedFrame = { receipt = ($0, $1, $2, now()) }
        func waitForFrame(_ scale: Double, _ offset: CGPoint) {
            let deadline = now() + 10
            repeat {
                if let frame = receipt, frame.0 == revision && frame.1 == scale && frame.2 == offset { return }
                if !displayLinkOnly { surface.advanceMetalFrame() }
                _ = pumpEvents(until: Date(timeIntervalSinceNow: 0.001))
            } while now() < deadline
            if let frame = receipt, frame.0 == revision && frame.1 == scale && frame.2 == offset,
               frame.3 <= deadline { return }
            try? output(["phase": "connected-accept-deadline", "scale": scale,
                "presenter": surface.metal?.diagnosticState ?? [:],
                "visible": window.isVisible, "occlusion_visible": window.occlusionState.contains(.visible),
                "app_active": app.isActive, "key_window": window.isKeyWindow,
                "submissions": surface.metalSubmissions, "presentations": surface.metalPresentations,
                "gpu_tiles": surface.gpuCoveredTiles.count])
            preconditionFailure("real attached Metal/CPU frame was not accepted before deadline")
        }
        func verifyFallbackCoverage(_ scale: Double, _ offset: CGPoint) {
            let view = CGRect(x: -offset.x / scale, y: -offset.y / scale,
                width: viewport.width / scale, height: viewport.height / scale)
            let covered = surface.gpuCoveredTiles
            let overview = Dictionary(uniqueKeysWithValues: surface.retainedOverviewImages.map { ($0.tile, $0) })
            let patches = surface.retainedDetailImages.filter { $0.visible }
            let transition = surface.retainedTransitionImage
            for (index, tile) in tiles.enumerated() {
                let target = tile.rect.intersection(view)
                guard !target.isNull && !target.isEmpty else { continue }
                if covered.contains(index) {
                    precondition(overview[index]?.visible == false, "GPU-covered tile must not retain CPU text on top")
                    precondition(!patches.contains { $0.tile == index }, "GPU glyphs must not be doubled by CPU patches")
                    continue
                }
                if let image = overview[index], image.visible,
                   min(Double(image.image.width) / image.rect.width, Double(image.image.height) / image.rect.height) >= scale * backing { continue }
                if let transition, transition.tiles.contains(index), transition.rect.contains(target) { continue }
                precondition(sharpPatchCoverage(patches, tile: tile, index: index, target: target,
                    density: scale * backing), "every visible non-GPU source tile keeps complete density-correct fallback")
            }
        }
        var positiveGPUFrames = 0, compositionCases = 0
        var scenarios = scales.map { (scale: $0, center: CGPoint(x: bounds.midX, y: bounds.midY), sourceDensity: 0.0) }
        var scenarioIndex = 0
        var lifecycleTile: AtlasTextTile?
        var lifecycleIndex: Int?
        while scenarioIndex < scenarios.count {
            let scenario = scenarios[scenarioIndex]
            let scale = scenario.scale
            for moving in [false, true] {
                var requests: [Double] = [], latencies: [Double] = []
                var finalOffset = CGPoint.zero
                let beforeSubmissions = surface.metalSubmissions, beforePresentations = surface.metalPresentations
                let sceneIdentity = surface.metalScene.map(ObjectIdentifier.init)
                for frame in 0..<120 {
                    let t = Double(frame) / 119
                    finalOffset = CGPoint(x: viewport.width / 2 - scenario.center.x * scale + (moving ? sin(t * .pi * 2) * 160 : 0),
                        y: viewport.height / 2 - scenario.center.y * scale + (moving ? cos(t * .pi * 2) * 120 : 0))
                    let start = now()
                    surface.update(tiles: tiles, revision: revision, scale: scale, offset: finalOffset,
                        selectedPath: nil, hitPaths: [])
                    requests.append((now() - start) * 1000)
                    waitForFrame(scale, finalOffset)
                    latencies.append(max(0, ((receipt?.3 ?? now()) - start) * 1000))
                    if !surface.gpuCoveredTiles.isEmpty { positiveGPUFrames += 1 }
                    if frame == 0 { verifyFallbackCoverage(scale, finalOffset) }
                }
                if let sceneIdentity { precondition(surface.metalScene.map(ObjectIdentifier.init) == sceneIdentity, "camera movement keeps the retained scene") }
                verifyFallbackCoverage(scale, finalOffset)
                let sorted = requests.sorted(), accepted = latencies.sorted()
                let metal = surface.metal!
                precondition(metal.maximumInFlight <= 2 && metal.managedBytes <= AtlasMetalGlyphRenderer.maximumManagedBytes)
                precondition(metal.failures == 0, "actual production GPU submission/presentation must not fail")
                try output(["phase": "connected-metal-camera", "scale": scale, "panning": moving, "frames": requests.count,
                    "qualified_source_density": scenario.sourceDensity, "rendering_mode": captureMode ?? "nativeDisplay",
                    "first_request_ms": requests[0], "request_cpu_p95_ms": sorted[113],
                    "production_completion_only": displayLinkOnly,
                    "test_wait_accept_p95_ms": accepted[113], "submissions": surface.metalSubmissions - beforeSubmissions,
                    "presentations": surface.metalPresentations - beforePresentations,
                    "covered_tiles": surface.gpuCoveredTiles.count, "scene_batches": surface.metalScene?.batches.count ?? 0,
                    "scene_fallback_tiles": surface.metalScene?.fallbackTiles.count ?? 0,
                    "maximum_in_flight": metal.maximumInFlight, "managed_bytes": metal.managedBytes,
                    "drawable_acquisition": "system_display_link",
                    "gpu_command_p95_ms": metal.gpuSeconds.sorted().dropLast(metal.gpuSeconds.count / 20).last.map { $0 * 1000 } ?? 0])
                try AtlasMetalRasterTests.verifyAccepted(surface)
                if let scene = surface.metalScene, !surface.gpuGlyphTiles.isEmpty {
                    if captureMode != nil {
                        try output(AtlasMetalImageOracle.compareFixedVisibleControl(scene: scene, tiles: tiles,
                            ids: surface.gpuGlyphTiles.sorted(), scale: scale, backing: backing, phase: 0))
                    } else {
                        try output(AtlasMetalImageOracle.compare(scene: scene, tiles: tiles,
                            ids: surface.gpuGlyphTiles.sorted(), scale: scale, offset: finalOffset, viewport: viewport, backing: backing))
                    }
                }
                if captureMode != nil, !moving, compositionCases == 0, let scene = surface.metalScene {
                    let bounded = surface.gpuGlyphTiles.sorted().filter {
                        Int(ceil(AtlasTextTile.width * 4)) * Int(ceil(tiles[$0].height * 4)) <= 8 * 1024 * 1024 }
                    if bounded.count >= 2 {
                        let first = bounded[0]
                        let second = bounded.dropFirst().min {
                            hypot(tiles[$0].rect.midX - tiles[first].rect.midX, tiles[$0].rect.midY - tiles[first].rect.midY) <
                            hypot(tiles[$1].rect.midX - tiles[first].rect.midX, tiles[$1].rect.midY - tiles[first].rect.midY) }!
                        let shifted = CGPoint(x: finalOffset.x + 0.25 / backing, y: finalOffset.y + 0.75 / backing)
                        surface.update(tiles: tiles, revision: revision, scale: scale, offset: shifted, selectedPath: nil, hitPaths: [])
                        waitForFrame(scale, shifted); verifyFallbackCoverage(scale, shifted)
                        try output(AtlasMetalImageOracle.compareFixedComposition(scene: scene, tiles: tiles, ids: [first, second],
                            scale: scale, offset: shifted, viewport: viewport, backing: backing))
                        compositionCases += 1
                    }
                }
                if scenario.sourceDensity > 0 {
                    func requireQualifiedSourceRoute(at offset: CGPoint) throws {
                        if !surface.gpuCoveredTiles.isEmpty { return }
                        precondition(captureMode != nil, "Native-display qualification must actually use GPU text")
                        let view = CGRect(x: -offset.x / scale, y: -offset.y / scale,
                            width: viewport.width / scale, height: viewport.height / scale)
                        let candidates = surface.metalScene!.batches.filter {
                            $0.value.rect.intersects(view) && $0.value.supportsDisplayDensity(scale * backing) }
                        precondition(candidates[lifecycleIndex!] != nil,
                                     "Dedicated source camera must remain inside its actual admitted density range")
                        let images = Dictionary(uniqueKeysWithValues: surface.retainedOverviewImages.map { ($0.tile, $0) })
                        for (index, _) in candidates {
                            guard let image = images[index] else { preconditionFailure("Actual retained overview required") }
                            precondition(min(Double(image.image.width) / image.rect.width,
                                Double(image.image.height) / image.rect.height) >= scale * backing * 1.15,
                                "Every visible eligible tile may skip GPU only with the actual adequate overview")
                        }
                        try output(["phase": "qualified-cached-overview-route", "source_density": scenario.sourceDensity,
                            "eligible_tiles": candidates.count, "gpu_tiles": 0, "headroom": 1.15])
                    }
                    try requireQualifiedSourceRoute(at: finalOffset)
                    if !moving, let scene = surface.metalScene {
                        for phase in [0.0, 0.25, 0.5, 0.75] {
                            let shifted = CGPoint(x: finalOffset.x + phase / backing, y: finalOffset.y + phase / backing)
                            surface.update(tiles: tiles, revision: revision, scale: scale, offset: shifted,
                                           selectedPath: nil, hitPaths: [])
                            waitForFrame(scale, shifted)
                            verifyFallbackCoverage(scale, shifted)
                            try requireQualifiedSourceRoute(at: shifted)
                            try AtlasMetalRasterTests.verifyAccepted(surface)
                            if captureMode != nil && !surface.gpuGlyphTiles.isEmpty {
                                try output(AtlasMetalImageOracle.compareFixedVisibleControl(scene: scene, tiles: tiles,
                                    ids: surface.gpuGlyphTiles.sorted(), scale: scale, backing: backing, phase: phase))
                            } else if captureMode == nil && !surface.gpuGlyphTiles.isEmpty {
                                try output(AtlasMetalImageOracle.compare(scene: scene, tiles: tiles,
                                    ids: surface.gpuGlyphTiles.sorted(), scale: scale, offset: shifted, viewport: viewport, backing: backing))
                            }
                        }
                    }
                }
            }
            scenarioIndex += 1
            if scenarioIndex == scales.count {
                let candidates = surface.metalScene!.batches.keys.sorted().filter { captureMode == nil ||
                    Int(ceil(AtlasTextTile.width * 4)) * Int(ceil(tiles[$0].height * 4)) <= 8 * 1024 * 1024 }
                let index = candidates.min { a, b in
                    let aRect = tiles[a].rect, bRect = tiles[b].rect
                    return hypot(aRect.midX - bounds.midX, aRect.midY - bounds.midY) <
                        hypot(bRect.midX - bounds.midX, bRect.midY - bounds.midY)
                }!
                let tile = tiles[index]
                lifecycleTile = tile
                lifecycleIndex = index
                for density in (captureMode == nil ? [4.0, 6.0, 8.0] : [0.1, 0.2, 0.5, 1, 2, 4]) {
                    scenarios.append((density / tile.contentScale / backing,
                                      CGPoint(x: tile.rect.midX, y: tile.rect.midY), density))
                }
            }
        }
        do {
            precondition(captureMode == nil || compositionCases > 0, "Capture mode requires an actual-camera multi-tile composition oracle")
            precondition(positiveGPUFrames > 0 && surface.metalSubmissions > 0 && surface.metalPresentations > 0,
                         "this real project must actually submit and present GPU source text")
        }
        // A settled camera must redraw after backing/viewport changes without
        // another input event. Require an actual new GPU presentation receipt.
        let tile = lifecycleTile!
        let lifecycleScale = (captureMode == nil ? 6.0 : 2.0) / tile.contentScale / backing
        let lifecycleOffset = CGPoint(x: viewport.width / 2 - tile.rect.midX * lifecycleScale,
                                      y: viewport.height / 2 - tile.rect.midY * lifecycleScale)
        surface.update(tiles: tiles, revision: revision, scale: lifecycleScale, offset: lifecycleOffset,
                       selectedPath: nil, hitPaths: [])
        waitForFrame(lifecycleScale, lifecycleOffset)
        // A warmed GPU-only camera must not need the CPU detail display link
        // to notice command completion. Keep source and accepted-frame fences.
        let cpuSettleDeadline = now() + 10
        while (surface.hasPendingCPUDetail || surface.isCPUDetailClockActive) && now() < cpuSettleDeadline {
            _ = pumpEvents(until: Date(timeIntervalSinceNow: 0.001))
        }
        precondition(!surface.hasPendingCPUDetail && !surface.isCPUDetailClockActive,
            "GPU camera setup must first settle actual CPU detail and captions")
        let clockFreeBefore = surface.metalPresentations
        let clockFreeOffset = CGPoint(x: lifecycleOffset.x + 0.125, y: lifecycleOffset.y)
        surface.update(tiles: tiles, revision: revision, scale: lifecycleScale, offset: clockFreeOffset,
                       selectedPath: nil, hitPaths: [])
        precondition(!surface.hasPendingCPUDetail && !surface.isCPUDetailClockActive,
            "GPU-only request must not start the CPU detail display link")
        waitForFrame(lifecycleScale, clockFreeOffset)
        precondition(surface.metalPresentations > clockFreeBefore && !surface.gpuCoveredTiles.isEmpty)
        precondition(!surface.hasPendingCPUDetail && !surface.isCPUDetailClockActive,
            "GPU completion must accept without CPU display-link polling")
        try output(["phase": "gpu-completion-without-cpu-clock", "accepted": true,
                    "gpu_tiles": surface.gpuCoveredTiles.count])
        surface.update(tiles: tiles, revision: revision, scale: lifecycleScale, offset: lifecycleOffset,
                       selectedPath: nil, hitPaths: [])
        waitForFrame(lifecycleScale, lifecycleOffset)
        func waitForNewPresentation(after count: Int, size: CGSize, density: Double) {
            let deadline = now() + 10
            repeat {
                surface.advanceMetalFrame()
                _ = pumpEvents(until: Date(timeIntervalSinceNow: 0.001))
                if surface.metalPresentations > count, let accepted = surface.metal?.acceptedFrame,
                   accepted.revision == revision && accepted.scale == lifecycleScale && accepted.offset == lifecycleOffset &&
                   accepted.size == size && accepted.backing == density && !surface.gpuCoveredTiles.isEmpty { return }
            } while now() < deadline
            preconditionFailure("settled viewport/backing/reattach must resume real GPU presentation without input")
        }
        let resized = CGSize(width: viewport.width - 31, height: viewport.height - 17)
        var presented = surface.metalPresentations
        surface.frame.size = resized
        surface.layoutSubtreeIfNeeded()
        surface.setBackingScale(2.5)
        waitForNewPresentation(after: presented, size: resized, density: 2.5)
        presented = surface.metalPresentations
        surface.frame.size = viewport
        surface.layoutSubtreeIfNeeded()
        surface.setBackingScale(backing)
        waitForNewPresentation(after: presented, size: viewport, density: backing)
        func waitForSubmission(after count: Int) {
            let deadline = now() + 10
            while surface.metalSubmissions <= count && now() < deadline {
                _ = pumpEvents(until: Date(timeIntervalSinceNow: 0.001))
            }
            precondition(surface.metalSubmissions > count, "lifecycle race requires a real submitted GPU command")
        }
        // Submit a different camera, detach immediately, and use the CPU path.
        // Its queued completion must not replace the accepted CPU camera.
        var submitted = surface.metalSubmissions
        surface.update(tiles: tiles, revision: revision, scale: lifecycleScale,
                       offset: CGPoint(x: lifecycleOffset.x + 41, y: lifecycleOffset.y - 13),
                       selectedPath: nil, hitPaths: [])
        waitForSubmission(after: submitted)
        window.contentView = nil
        surface.update(tiles: tiles, revision: revision, scale: lifecycleScale, offset: lifecycleOffset,
                       selectedPath: nil, hitPaths: [])
        for _ in 0..<100 {
            surface.advanceMetalFrame()
            _ = pumpEvents(until: Date(timeIntervalSinceNow: 0.001))
            precondition(surface.metal?.acceptedFrame == nil && surface.gpuCoveredTiles.isEmpty)
            precondition(receipt?.0 == revision && receipt?.1 == lifecycleScale && receipt?.2 == lifecycleOffset,
                         "detached GPU completion cannot overwrite accepted CPU camera")
        }
        presented = surface.metalPresentations
        window.contentView = surface
        surface.frame.size = viewport
        surface.setBackingScale(backing)
        waitForNewPresentation(after: presented, size: viewport, density: backing)
        try output(["phase": "connected-metal-lifecycle", "resize_backing_reattach": true,
                    "maximum_in_flight": surface.metal!.maximumInFlight])
        // Retire a real submitted project frame, then immediately replace the
        // project. A late completion must never resurrect its source/camera.
        let pendingOffset = CGPoint(x: lifecycleOffset.x + 19, y: lifecycleOffset.y - 23)
        submitted = surface.metalSubmissions
        surface.update(tiles: tiles, revision: revision, scale: lifecycleScale, offset: pendingOffset,
                       selectedPath: nil, hitPaths: [])
        waitForSubmission(after: submitted)
        let replacement = UUID()
        surface.update(tiles: [], revision: replacement, scale: 1, offset: .zero,
                       selectedPath: nil, hitPaths: [])
        for _ in 0..<100 {
            surface.advanceMetalFrame()
            _ = pumpEvents(until: Date(timeIntervalSinceNow: 0.001))
            precondition(receipt?.0 == replacement && surface.gpuCoveredTiles.isEmpty,
                         "late GPU completion cannot resurrect an obsolete project or hit-test camera")
        }
        try reportMetalPresentationTimes(surface)
        surface.stopClock()
        let cpuFallback = AtlasRetainedSurface(metalEnabled: false)
        cpuFallback.frame = CGRect(origin: .zero, size: viewport)
        window.contentView = cpuFallback; cpuFallback.setBackingScale(backing)
        let fallbackScale = 8.0 / tile.contentScale / backing
        let fallbackOffset = CGPoint(x: viewport.width / 2 - tile.rect.midX * fallbackScale,
                                     y: viewport.height / 2 - tile.rect.midY * fallbackScale)
        cpuFallback.update(tiles: tiles, revision: UUID(), scale: fallbackScale, offset: fallbackOffset,
                           selectedPath: nil, hitPaths: [])
        precondition(cpuFallback.hasPendingCPUDetail && cpuFallback.isCPUDetailClockActive,
            "Real CPU fallback must still schedule detail work")
        let fallbackBefore = cpuFallback.metrics.detailRasters
        let fallbackDeadline = now() + 10
        while cpuFallback.metrics.detailRasters == fallbackBefore && now() < fallbackDeadline {
            _ = pumpEvents(until: Date(timeIntervalSinceNow: 0.001))
        }
        precondition(cpuFallback.metrics.detailRasters > fallbackBefore,
            "CPU fallback clock must perform real detail raster work")
        try output(["phase": "cpu-fallback-clock-progress", "detail_rasters": cpuFallback.metrics.detailRasters - fallbackBefore])
        window.contentView = nil
        precondition(!cpuFallback.isCPUDetailClockActive, "Detached CPU detail work must stop its clock")
        let detachedRasters = cpuFallback.metrics.detailRasters
        let detachedDeadline = now() + 0.1
        while now() < detachedDeadline {
            _ = pumpEvents(until: Date(timeIntervalSinceNow: 0.001))
        }
        precondition(cpuFallback.metrics.detailRasters == detachedRasters,
            "An invalidated CPU clock must not continue preparing detached detail")
        window.contentView = cpuFallback
        let resumeDeadline = now() + 10
        while cpuFallback.hasPendingCPUDetail && now() < resumeDeadline {
            _ = pumpEvents(until: Date(timeIntervalSinceNow: 0.001))
        }
        precondition(!cpuFallback.hasPendingCPUDetail && !cpuFallback.isCPUDetailClockActive,
            "Reattached CPU detail must finish its queue and stop waking the idle app")
        try output(["phase": "cpu-fallback-clock-lifecycle", "detach_stopped": true, "reattach_settled": true])
        cpuFallback.stopClock(); window.orderOut(nil)
    }
}

// Timing control only: same attached window, source and run-loop protocol as
// the connected profile, with the production CPU renderer explicitly selected.
extension AtlasRenderProfile {
    @MainActor static func connectedCPUWindow(tiles: [AtlasTextTile], bounds: CGRect) throws {
        let app = NSApplication.shared
        app.setActivationPolicy(.accessory); app.finishLaunching()
        let window = NSWindow(contentRect: CGRect(origin: .zero, size: viewport),
                              styleMask: [.titled], backing: .buffered, defer: false)
        let surface = AtlasRetainedSurface(metalEnabled: false)
        surface.frame = CGRect(origin: .zero, size: viewport)
        window.contentView = surface; window.orderFrontRegardless(); surface.setBackingScale(backing)
        _ = pumpEvents(until: Date(timeIntervalSinceNow: 0.02))
        let revision = UUID(), scale = 0.2
        for moving in [false, true] {
            var requests: [Double] = [], elapsed: [Double] = []
            for frame in 0..<120 {
                let t = Double(frame) / 119
                let offset = CGPoint(x: viewport.width / 2 - bounds.midX * scale + (moving ? sin(t * .pi * 2) * 160 : 0),
                                     y: viewport.height / 2 - bounds.midY * scale + (moving ? cos(t * .pi * 2) * 120 : 0))
                let start = now()
                surface.update(tiles: tiles, revision: revision, scale: scale, offset: offset,
                               selectedPath: nil, hitPaths: [])
                requests.append((now() - start) * 1000)
                _ = pumpEvents(until: Date(timeIntervalSinceNow: 0.001))
                elapsed.append((now() - start) * 1000)
                precondition(surface.metal == nil && surface.gpuCoveredTiles.isEmpty)
            }
            try output(["phase": "connected-cpu-baseline", "scale": scale, "panning": moving,
                        "frames": requests.count, "first_request_ms": requests[0],
                        "request_cpu_p95_ms": requests.sorted()[113], "test_wait_accept_p95_ms": elapsed.sorted()[113]])
        }
        surface.stopClock(); window.orderOut(nil)
    }
}
