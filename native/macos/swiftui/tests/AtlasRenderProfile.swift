import Foundation
import AppKit
import CryptoKit
import CoreGraphics
import CoreText
import Metal

@_silgen_name("fcb_atlas_layout")
func renderAtlas(_ root: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_source_document")
func renderSource(_ path: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_text_tile_positions")
func renderPositions(_ heights: UnsafePointer<Double>?, _ count: UInt64, _ columns: UInt64,
                     _ width: Double, _ gap: Double) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_free_string")
func renderFree(_ pointer: UnsafeMutablePointer<CChar>?)

@main struct AtlasRenderProfile {
    static let viewport: CGSize = {
        let large = ProcessInfo.processInfo.environment["FCB_RENDER_VIEWPORT"] == "2952x1488"
        return large ? CGSize(width: 2952, height: 1488) : CGSize(width: 1476, height: 744)
    }()
    static var scales: [Double] {
        if ProcessInfo.processInfo.environment["FCB_RENDER_OVERVIEW"] == "1" { return [0.03, 0.05] }
        return ProcessInfo.processInfo.environment["FCB_RENDER_HIGH_ZOOM"] == "1" ? [4.9, 6.9] : [0.2, 0.4, 0.8, 1.6]
    }
    static let backing = 2.0
    static func reply(_ pointer: UnsafeMutablePointer<CChar>?) -> Data? {
        guard let pointer else { return nil }; defer { renderFree(pointer) }
        return Data(String(cString: pointer).utf8)
    }
    static func digest(_ hash: SHA256) -> String { hash.finalize().map { String(format: "%02x", $0) }.joined() }
    static func now() -> Double { ProcessInfo.processInfo.systemUptime }
    @MainActor static func pumpEvents(until deadline: Date) -> Bool {
        // A bare RunLoop pump does not dispatch AppKit activation/occlusion
        // events. Drive the native event queue as well, as the real app does.
        let app = NSApplication.shared
        let event = app.nextEvent(matching: .any, until: deadline, inMode: .default, dequeue: true)
        if let event { app.sendEvent(event) }
        app.updateWindows()
        return event != nil
    }
    static func output(_ value: [String: Any]) throws {
        print(String(data: try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), encoding: .utf8)!)
        fflush(stdout)
    }
    // Mirrors 890525b App.swift:712-735 culling/row selection. Production
    // AtlasTextTile.draw performs the real CoreText/prepared-glyph work.
    // SwiftUI Canvas wrapping and display/GPU presentation are not measured.
    static func draw(_ context: CGContext, tiles: [AtlasTextTile], scale: Double, offset: CGPoint) {
        context.setFillColor(CGColor(srgbRed: 0.153, green: 0.157, blue: 0.133, alpha: 1))
        context.fill(CGRect(origin: .zero, size: viewport))
        for tile in tiles {
            let rect = CGRect(x: tile.rect.minX * scale + offset.x, y: tile.rect.minY * scale + offset.y,
                              width: tile.rect.width * scale, height: tile.rect.height * scale)
            guard rect.intersects(CGRect(origin: .zero, size: viewport)) else { continue }
            if scale * 13 >= 1.5 {
                let rows = AtlasSource.visibleRows(count: tile.lineCount, top: rect.minY + 22 * scale,
                    lineHeight: AtlasTextTile.lineHeight * scale, viewportHeight: viewport.height)
                context.saveGState(); context.clip(to: rect)
                context.translateBy(x: rect.minX, y: rect.minY); context.scaleBy(x: scale, y: scale)
                tile.draw(in: context, visibleRows: rows); context.restoreGState()
            } else if let image = tile.raster {
                context.saveGState(); context.translateBy(x: rect.minX, y: rect.maxY)
                context.scaleBy(x: 1, y: -1)
                context.draw(image, in: CGRect(origin: .zero, size: rect.size)); context.restoreGState()
            }
        }
    }
    // Independent source/geometry oracle over the real frozen corpus. Original
    // archive tiles remain the reference; display-column boundaries may change.
    static func verifyReflow(_ documents: [AtlasDocument]) {
        func rows(_ tile: AtlasTextTile) -> [NSRange] {
            if let prepared = tile.preparedLines { return prepared.map(\.sourceRange) }
            return tile.lines.map { let r = CTLineGetStringRange($0); return NSRange(location: r.location, length: r.length) }
        }
        func contained(_ a: CGRect, _ b: CGRect) -> Bool {
            a.minX >= b.minX - 1e-7 && a.minY >= b.minY - 1e-7 && a.maxX <= b.maxX + 1e-7 && a.maxY <= b.maxY + 1e-7
        }
        func disjoint(_ rects: [CGRect]) {
            for (i, rect) in rects.enumerated() { for other in rects[..<i] {
                let overlap = rect.intersection(other)
                precondition(overlap.isNull || overlap.width <= 1e-7 || overlap.height <= 1e-7, "source and sibling parcels never overlap")
            } }
        }
        var hierarchy: [String: [String: CGRect]] = [:]
        for document in documents {
            let display = document.renderTiles
            precondition(display.flatMap(rows) == document.tiles.flatMap(rows), "every original shaped row survives exactly once in source order")
            let parcel = display[0].parcelRect!
            var cursor = 0
            for tile in display {
                precondition(tile.sourceRange.location == cursor, "display source ranges are contiguous")
                cursor += tile.sourceRange.length
                precondition(tile.parcelRect == parcel && contained(tile.rect, parcel), "complete display tile inside its file parcel")
                precondition(abs(tile.rect.height / tile.height - tile.contentScale) <= 1e-7, "glyph proportions preserved")
            }
            precondition(cursor == document.source.text.utf16.count, "complete UTF16 source retained exactly once")
            disjoint(display.map(\.rect))
            let components = document.source.path.split(separator: "/").map(String.init)
            for depth in components.indices {
                let parent = components.prefix(depth).joined(separator: "/")
                let child = components.prefix(depth + 1).joined(separator: "/")
                hierarchy[parent, default: [:]][child] = hierarchy[parent]?[child].map { $0.union(parcel) } ?? parcel
            }
            // Resolve an actual captured source row at a changed display-column
            // boundary, then compare the retained match band to its glyph row.
            if let tile = display.last, let row = rows(tile).firstIndex(where: { $0.length > 0 }) {
                let range = rows(tile)[row], text = document.source.text
                guard let indices = Range(range, in: text) else { preconditionFailure("source row must preserve Unicode boundaries") }
                let start = text[..<indices.lowerBound].utf8.count
                let length = text[indices].utf8.count
                let hash = SHA256.hash(data: Data(text.utf8)).map { String(format: "%02x", $0) }.joined()
                let hit = AtlasMatch.resolve(document: document, byteStart: UInt64(start), byteEnd: UInt64(start + length),
                    expectedSHA256: hash, expectedByteCount: UInt64(text.utf8.count))!
                let scale = tile.contentScale
                let expected = CGRect(x: tile.rect.minX + 4 * scale,
                    y: tile.rect.minY + (22 + Double(row) * AtlasTextTile.lineHeight) * scale,
                    width: (AtlasTextTile.width - 8) * scale, height: AtlasTextTile.lineHeight * scale).intersection(tile.rect)
                precondition(hit.sourceRange == range && hit.rowRects == [expected], "capture-verified search band follows the real reflowed glyph row")
            }
        }
        for siblings in hierarchy.values { disjoint(Array(siblings.values)) }
    }

    @MainActor static func main() throws {
        if ProcessInfo.processInfo.environment["FCB_RASTER_LIFECYCLE"] == "1" {
            try AtlasMetalRasterTests.lifecycle(); return
        }
        if ProcessInfo.processInfo.environment["FCB_ZOOM_ENVELOPE_REGRESSION"] == "1" {
            try transitionEnvelopeRegression(); return
        }
        if ProcessInfo.processInfo.environment["FCB_ZOOM_SHARP_REUSE_REGRESSION"] == "1" {
            try sharpFallbackReuseRegression(); return
        }
        var args = CommandLine.arguments
        // A real app-bundle launch has no CLI arguments. Keep the same explicit
        // corpus/oracle/cache contract for foreground presentation qualification.
        if args.count == 1 {
            args += ["FCB_PROFILE_CORPUS", "FCB_PROFILE_ORACLE", "FCB_PROFILE_CACHE"].compactMap {
                ProcessInfo.processInfo.environment[$0]
            }
        }
        precondition(args.count == 4)
        let root = args[1]
        var cache: AtlasProjectCache? = AtlasProjectCache(root: args[1], cacheDirectory: args[3])
        precondition(cache!.engineStatsJSON != nil, "real native cache required")
        cache!.beginRefresh()
        let atlas = try JSONSerialization.jsonObject(with: reply(renderAtlas(root))!) as! [String: Any]
        let files = atlas["files"] as! [[String: Any]]
        var documents: [AtlasDocument] = [], tiles: [AtlasTextTile] = [], remaining = 64 * 1024 * 1024
        var source = SHA256(), raster = SHA256(), geometry = SHA256()
        for file in files {
            let path = file["path"] as! String
            guard ((file["bytes"] as? NSNumber)?.intValue ?? 0) <= remaining else { continue }
            guard let document = cache!.document(path: path, fallback: {
                guard let data = reply(renderSource((root as NSString).appendingPathComponent(path))) else { return nil }
                return try? JSONDecoder().decode(AtlasHighlightCapture.self, from: data)
            }), document.source.text.utf8.count <= remaining else { continue }
            remaining -= document.source.text.utf8.count
            source.update(data: Data(path.utf8)); source.update(data: Data([0]))
            source.update(data: Data(document.source.text.utf8)); source.update(data: Data([0]))
            documents.append(document); tiles.append(contentsOf: document.tiles)
        }
        let heights = tiles.map(\.height)
        let columns = max(1, min(256, Int(sqrt(heights.reduce(0, +) * 1.9 / AtlasTextTile.width).rounded())))
        let packed = heights.withUnsafeBufferPointer {
            reply(renderPositions($0.baseAddress, UInt64($0.count), UInt64(columns), AtlasTextTile.width, 6))!
        }
        geometry.update(data: packed)
        let positions = try JSONDecoder().decode([[Double]].self, from: packed)
        precondition(positions.count == tiles.count)
        // A fresh host has no prepared archives. Use the same bounded base
        // raster preparation as AtlasLoadProfile and the production packer.
        let baseBudget = max(1, 16 * 1024 * 1024 / max(1, tiles.count))
        var remainingBasePixels = 16 * 1024 * 1024
        for (index, tile) in tiles.enumerated() {
            let available = max(1, remainingBasePixels - (tiles.count - index - 1))
            if tile.raster.map({ $0.width * $0.height > available }) ?? true {
                tile.prepareRaster(pixelBudget: min(baseBudget, available))
            }
            remainingBasePixels -= tile.raster.map { $0.width * $0.height } ?? 0
            precondition(remainingBasePixels >= 0, "bounded original raster preparation")
        }
        var bounds = CGRect.null
        for (tile, rect) in zip(tiles, positions) {
            tile.rect = CGRect(x: rect[0], y: rect[1], width: rect[2], height: rect[3]); bounds = bounds.union(tile.rect)
            guard let image = tile.raster, let data = image.dataProvider?.data else { preconditionFailure("real persisted raster required") }
            raster.update(data: data as Data)
        }
        let expected = try JSONDecoder().decode([String: String].self, from: Data(contentsOf: URL(fileURLWithPath: args[2])))
        try output(["phase": "original-oracle-observed", "source": digest(source),
                    "geometry": digest(geometry), "raster": digest(raster),
                    "os": ProcessInfo.processInfo.operatingSystemVersionString])
        precondition(digest(source) == expected["source"] && digest(geometry) == expected["geometry"] && digest(raster) == expected["raster"], "original corpus/layout/raster oracle")
        try output(["phase": "loaded", "documents": documents.count, "tiles": tiles.count,
            "disk_hits": cache!.diskHits, "rebuilt": cache!.rebuilt, "rejections": cache!.rejectionReasons,
            "bounds": [bounds.minX, bounds.minY, bounds.width, bounds.height]])
        if ProcessInfo.processInfo.environment["FCB_RENDER_LAYOUT"] == "parcel" {
            bounds = AtlasParcelLayout.place(tiles)!
            tiles = AtlasParcelLayout.reflow(Dictionary(uniqueKeysWithValues: documents.map { ($0.source.path, $0) }))!
            verifyReflow(documents)
            let parcels = tiles.filter(\.parcelFirst).map { $0.parcelRect! }
            let tileArea = tiles.reduce(0.0) { $0 + $1.rect.width * $1.rect.height }
            let parcelArea = parcels.reduce(0.0) { $0 + $1.width * $1.height }
            let phi = (1.0 + sqrt(5.0)) / 2.0
            let errors = parcels.map { rect -> (error: Double, area: Double) in
                let aspect = max(rect.width / rect.height, rect.height / rect.width)
                return (abs(log(aspect / phi)), rect.width * rect.height)
            }.sorted { $0.error < $1.error }
            var cumulativeArea = 0.0, weightedP95 = 0.0
            for item in errors {
                cumulativeArea += item.area
                weightedP95 = item.error
                if cumulativeArea >= parcelArea * 0.95 { break }
            }
            try output(["phase": "parcel-layout", "bounds": [bounds.minX, bounds.minY, bounds.width, bounds.height],
                        "parcels": parcels.count, "tile_area": tileArea, "parcel_area": parcelArea,
                        "tile_parcel_occupancy": tileArea / parcelArea,
                        "tile_bounds_occupancy": tileArea / (bounds.width * bounds.height),
                        "golden_log_error_area_mean": errors.reduce(0.0) { $0 + $1.error * $1.area } / parcelArea,
                        "golden_log_error_area_p95": weightedP95])
        }
        if ProcessInfo.processInfo.environment["FCB_RGB_SCENE_PREPARE"] == "1" {
            let start = now()
            let device = MTLCreateSystemDefaultDevice()!
            var preparationBytes = 64 * 1024 * 1024
            var source: [AtlasMetalGlyphRenderer.SceneTile] = []
            for (index, tile) in tiles.enumerated() {
                preparationBytes += (tile.lineCount + 1) * 256
                if tile.preparedLines == nil {
                    for line in tile.lines { preparationBytes += CTLineGetGlyphCount(line) * 40 + CFArrayGetCount(CTLineGetGlyphRuns(line)) * 256 }
                }
                if tile.preparedHeader == nil, let header = tile.header {
                    preparationBytes += CTLineGetGlyphCount(header) * 40 + CFArrayGetCount(CTLineGetGlyphRuns(header)) * 256
                }
                precondition(preparationBytes < AtlasMetalGlyphRenderer.maximumManagedBytes)
                let header = tile.preparedHeader ?? tile.header.flatMap { AtlasPreparedLine($0) }
                let rows = tile.preparedLines ?? tile.lines.compactMap { AtlasPreparedLine($0) }
                guard let header, rows.count == tile.lineCount else { continue }
                var lines: [AtlasMetalGlyphRenderer.Line] = [.init(text: header, origin: CGPoint(x: 4, y: 12), clip: tile.rect)]
                for (row, text) in rows.enumerated() {
                    lines.append(.init(text: text, origin: CGPoint(x: 4, y: 34 + Double(row) * AtlasTextTile.lineHeight), clip: tile.rect))
                }
                source.append(.init(id: index, rect: tile.rect, sourceScale: tile.contentScale, lines: lines))
            }
            try output(["phase": "rgb-scene-input", "source_tiles": tiles.count, "prepared_tiles": source.count,
                        "source_glyphs": source.reduce(0) { $0 + $1.lines.reduce(0) { $0 + $1.text.runs.reduce(0) { $0 + $1.glyphs.count } } },
                        "preparation_reserved_bytes": preparationBytes])
            let scalarCapture = ProcessInfo.processInfo.environment["FCB_CAPTURE_SCALAR"] == "1"
            let lowDensity = ProcessInfo.processInfo.environment["FCB_LOW_DENSITY_QUALIFICATION"] == "1"
            precondition(!lowDensity || scalarCapture, "Low-density qualification exercises retained grayscale coverage")
            let scene = try AtlasMetalGlyphRenderer(device: device, tiles: source, pixelsPerPoint: 4,
                previousManagedBytes: preparationBytes, maskRepresentation: scalarCapture ? .grayscale : .opaqueRGBPhases,
                maskPlacement: .captureGrid, tileMemoryAccumulation: lowDensity)
            let reasons = Dictionary(grouping: scene.fallbackTiles.values, by: { String(describing: $0) }).mapValues { $0.count }
            let inputGlyphs = Dictionary(uniqueKeysWithValues: source.map { tile in
                (tile.id, tile.lines.reduce(0) { $0 + $1.text.runs.reduce(0) { $0 + $1.glyphs.count } }) })
            let admittedInputGlyphs = scene.batches.keys.reduce(0) { $0 + inputGlyphs[$1]! }
            let fallbackInputGlyphs = Dictionary(grouping: scene.fallbackTiles, by: { String(describing: $0.value) })
                .mapValues { entries in entries.reduce(0) { $0 + inputGlyphs[$1.key]! } }
            precondition(admittedInputGlyphs + fallbackInputGlyphs.values.reduce(0, +) == inputGlyphs.values.reduce(0, +))
            try output(["phase": "rgb-scene-prepare", "scalar_capture": scalarCapture, "device": device.name, "source_tiles": tiles.count,
                        "prepared_tiles": source.count, "admitted_tiles": scene.batches.count, "fallback_reasons": reasons,
                        "admitted_input_glyphs_including_spaces": admittedInputGlyphs,
                        "fallback_input_glyphs_including_spaces": fallbackInputGlyphs,
                        "glyph_instances": scene.instanceCount, "managed_bytes": scene.managedBytes, "mask_capacity": scene.admittedMaskCapacity,
                        "preparation_reserved_bytes": preparationBytes, "prepare_ms": (now() - start) * 1000])
            if ProcessInfo.processInfo.environment["FCB_RGB_CORPUS_COST"] == "1" {
                let paired = ProcessInfo.processInfo.environment["FCB_CAPTURE_PAIR"] == "1"
                let other: AtlasMetalGlyphRenderer?
                if paired {
                    other = try AtlasMetalGlyphRenderer(device: device, tiles: source, pixelsPerPoint: 4,
                        previousManagedBytes: preparationBytes + scene.managedBytes,
                        maskRepresentation: scalarCapture ? .opaqueRGBPhases : .grayscale, maskPlacement: .captureGrid)
                } else { other = nil }
                if let other {
                    try output(["phase": "paired-capture-admission", "primary_scalar": scalarCapture,
                        "other_tiles": other.batches.count, "other_managed_bytes": other.managedBytes,
                        "combined_managed_bytes": scene.managedBytes + other.managedBytes])
                }
                let queue = device.makeCommandQueue()!
                let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .bgra8Unorm,
                    width: 2952, height: 1488, mipmapped: false)
                descriptor.storageMode = .private; descriptor.usage = .renderTarget
                let texture = device.makeTexture(descriptor: descriptor)!
                for scale in [0.05, 0.1, 0.2] {
                    let offset = CGPoint(x: 1476 - bounds.midX * scale * 2, y: 744 - bounds.midY * scale * 2)
                    let visible = CGRect(x: -offset.x / (scale * 2), y: -offset.y / (scale * 2),
                        width: 2952 / (scale * 2), height: 1488 / (scale * 2))
                    let ids = scene.batches.keys.sorted().filter { index in
                        let density = scale * 2 * tiles[index].contentScale
                        return density >= 0.1 && density <= 4 && tiles[index].rect.intersects(visible)
                            && (other == nil || other!.batches[index] != nil)
                    }
                    let instances = ids.reduce(0) { $0 + scene.batches[$1]!.instanceRange.count }
                    if let other {
                        let rgb = scalarCapture ? other : scene, scalar = scalarCapture ? scene : other
                        var timings: [String: [(Double, Double)]] = [:]
                        for frame in 0..<25 {
                            for label in (frame.isMultiple(of: 2) ? ["empty", "rgb", "scalar"] : ["scalar", "rgb", "empty"]) {
                                let active = label == "scalar" ? scalar : rgb
                                let selected = label == "empty" ? [] : ids
                                let command = queue.makeCommandBuffer()!, start = now()
                                try active.encode(into: texture, commandBuffer: command, scale: scale * 2, offset: offset, tileIDs: selected)
                                let cpuMS = (now() - start) * 1000
                                command.commit(); command.waitUntilCompleted()
                                precondition(command.status == .completed && command.error == nil)
                                let gpuMS = (command.gpuEndTime - command.gpuStartTime) * 1000
                                if frame >= 5 { timings[label, default: []].append((cpuMS, gpuMS)) }
                                try output(["phase": "paired-capture-cost-raw", "representation": label, "scale": scale,
                                    "frame": frame, "warmup": frame < 5, "encode_cpu_ms": cpuMS, "gpu_ms": gpuMS,
                                    "selected_tiles": selected.count, "submitted_instances": selected.isEmpty ? 0 : instances])
                            }
                        }
                        for label in ["empty", "rgb", "scalar"] {
                            let cpu = timings[label]!.map { $0.0 }.sorted(), gpu = timings[label]!.map { $0.1 }.sorted()
                            try output(["phase": "paired-capture-cost", "representation": label, "scale": scale,
                                "encode_cpu_p95_ms": cpu[18], "gpu_min_ms": gpu[0], "gpu_median_ms": (gpu[9] + gpu[10]) / 2,
                                "gpu_p95_ms": gpu[18], "measured_frames": 20, "warmup_frames": 5,
                                "serial_offscreen_not_fps": true])
                        }
                        continue
                    }
                    var encodeTimes: [Double] = [], gpuTimes: [Double] = []
                    for frame in 0..<25 {
                        let command = queue.makeCommandBuffer()!, encodeStart = now()
                        try scene.encode(into: texture, commandBuffer: command, scale: scale * 2, offset: offset, tileIDs: ids)
                        let encodeMS = (now() - encodeStart) * 1000
                        command.commit(); command.waitUntilCompleted()
                        precondition(command.status == .completed && command.error == nil)
                        let gpuMS = (command.gpuEndTime - command.gpuStartTime) * 1000
                        if frame >= 5 { encodeTimes.append(encodeMS); gpuTimes.append(gpuMS) }
                        try output(["phase": "rgb-corpus-offscreen-cost-raw", "scale": scale, "frame": frame,
                            "warmup": frame < 5, "encode_cpu_ms": encodeMS, "gpu_ms": gpuMS])
                    }
                    try output(["phase": "rgb-corpus-offscreen-cost", "scale": scale, "selected_tiles": ids.count,
                        "submitted_instances": instances, "encode_cpu_p95_ms": encodeTimes.sorted()[18],
                        "gpu_p95_ms": gpuTimes.sorted()[18], "gpu_min_ms": gpuTimes.min()!, "gpu_max_ms": gpuTimes.max()!,
                        "width": 2952, "height": 1488, "warmup_frames": 5, "measured_frames": 20,
                        "serial_offscreen_not_fps": true])
                }
            }
            if ProcessInfo.processInfo.environment["FCB_RGB_CORPUS_PIXELS"] == "1" {
                let candidates = scene.batches.keys.sorted().filter { tiles[$0].lineCount >= 8 &&
                    Int(ceil(AtlasTextTile.width * 4)) * Int(ceil(tiles[$0].height * 4)) <= 8 * 1024 * 1024 }
                precondition(candidates.count >= 3)
                let selected: [Int]
                if lowDensity {
                    let sampleCount = min(12, candidates.count)
                    let lastIndex = candidates.count - 1
                    selected = (0..<sampleCount).map { sample in
                        let index = sample * lastIndex / (sampleCount - 1)
                        return candidates[index]
                    }
                } else {
                    selected = [candidates[0], candidates[candidates.count / 2], candidates.last!]
                }
                let densities = lowDensity ? [0.1, 0.08, 0.06, 0.04] : [4.0, 2, 1, 0.5, 0.2, 0.1]
                for id in selected { for scale in densities { for phase in [0.0, 0.25, 0.5, 0.75] {
                    try output(AtlasMetalImageOracle.compareFixedCapture(scene: scene, tiles: tiles, id: id,
                        sourceDensity: scale, phase: phase))
                } } }
            }
            return
        }
        if ProcessInfo.processInfo.environment["FCB_RENDER_MODE"] == "retained" {
            let byPath = Dictionary(uniqueKeysWithValues: documents.map { ($0.source.path, $0) })
            cache!.finishRefresh(documents: byPath) // Keep the original archive independent of the new tier.
            func signatures(_ documents: [AtlasDocument], verifyPixels: Bool) -> [String] {
                var pixels = 0, result: [String] = []
                for document in documents { for tile in document.renderTiles {
                    guard let image = tile.raster else { preconditionFailure("prepared overview must contain actual source") }
                    pixels += image.width * image.height
                    precondition(pixels <= 64 * 1024 * 1024, "overview project pixel budget")
                    if verifyPixels {
                        let bitmap = CGContext(data: nil, width: image.width, height: image.height,
                            bitsPerComponent: 8, bytesPerRow: image.width * 4,
                            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
                        bitmap.setFillColor(Monokai.background.cgColor)
                        bitmap.fill(CGRect(x: 0, y: 0, width: image.width, height: image.height))
                        bitmap.translateBy(x: 0, y: Double(image.height))
                        bitmap.scaleBy(x: Double(image.width) / AtlasTextTile.width, y: -Double(image.height) / tile.height)
                        tile.draw(in: bitmap)
                        precondition(bitmap.makeImage()!.dataProvider!.data! as Data == image.dataProvider!.data! as Data,
                                     "prepared overview equals independently rendered complete source")
                    }
                    var hash = SHA256(); hash.update(data: image.dataProvider!.data! as Data)
                    result.append("\(image.width)x\(image.height):\(digest(hash))")
                } }
                return result
            }
            func prepare(_ target: AtlasProjectCache, _ values: [String: AtlasDocument], phase: String) throws {
                let start = now(); target.prepareOverview(documents: values)
                let elapsed = (now() - start) * 1000
                try output(["phase": phase, "prepare_ms": elapsed, "disk_hits": target.overviewDiskHits,
                    "ram_hits": target.overviewRAMHits, "rasters": target.overviewRasters, "saved": target.overviewSaved])
            }
            try prepare(cache!, byPath, phase: "overview-initial-observed")
            let initialOverview = signatures(documents, verifyPixels: true)
            try prepare(cache!, byPath, phase: "overview-ram")
            precondition(cache!.overviewRAMHits == documents.count && cache!.overviewRasters == 0,
                         "same geometry RAM refresh must not rasterize")
            precondition(signatures(documents, verifyPixels: false) == initialOverview)
            cache = nil // The source store has one writer owner; reopen only after releasing it.
            do {
                let fresh = AtlasProjectCache(root: root, cacheDirectory: args[3]); fresh.beginRefresh()
                let restored = documents.map { document in
                    fresh.document(path: document.source.path, fallback: { preconditionFailure("persisted base capture required") })!
                }
                let restoredTiles = restored.flatMap(\.tiles)
                if ProcessInfo.processInfo.environment["FCB_RENDER_LAYOUT"] == "parcel" {
                    precondition(AtlasParcelLayout.place(restoredTiles) == bounds)
                    precondition(AtlasParcelLayout.reflow(Dictionary(uniqueKeysWithValues: restored.map { ($0.source.path, $0) })) != nil)
                    verifyReflow(restored)
                } else {
                    for (tile, original) in zip(restoredTiles, tiles) { tile.rect = original.rect }
                }
                let restoredByPath = Dictionary(uniqueKeysWithValues: restored.map { ($0.source.path, $0) })
                try prepare(fresh, restoredByPath, phase: "overview-new-cache-ssd")
                precondition(fresh.overviewDiskHits == documents.count && fresh.overviewRasters == 0,
                             "fresh cache must restore every persisted overview without rasterization")
                precondition(signatures(restored, verifyPixels: false) == initialOverview,
                             "SSD overview must preserve complete pixel bytes and dimensions")
            }
            if ProcessInfo.processInfo.environment["FCB_RASTER_PROOF"] == "1" {
                try AtlasMetalRasterTests.run(tiles: tiles)
                return
            }
            if ProcessInfo.processInfo.environment["FCB_RENDER_GPU"] == "1" {
                try connectedMetal(tiles: tiles, bounds: bounds)
            } else { try retained(tiles: tiles, bounds: bounds) }
            withExtendedLifetime(documents) {}
            return
        }
        let center = bounds
        let bitmap = CGContext(data: nil, width: Int(viewport.width * backing), height: Int(viewport.height * backing),
            bitsPerComponent: 8, bytesPerRow: Int(viewport.width * backing) * 4,
            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        bitmap.translateBy(x: 0, y: viewport.height * backing); bitmap.scaleBy(x: backing, y: -backing)
        for scale in scales {
            for moving in [false, true] {
                var samples: [Double] = [], visibleCounts: [Int] = [], rowCounts: [Int] = [], runCounts: [Int] = []
                var signatures: [String] = []
                for frame in -3..<120 {
                    let t = Double(max(0, frame)) / 119
                    let offset = CGPoint(x: viewport.width / 2 - center.midX * scale + (moving ? sin(t * .pi * 2) * 160 : 0),
                                         y: viewport.height / 2 - center.midY * scale + (moving ? cos(t * .pi * 2) * 120 : 0))
                    let start = now(); draw(bitmap, tiles: tiles, scale: scale, offset: offset)
                    let elapsed = (now() - start) * 1000
                    guard frame >= 0 else { continue }; samples.append(elapsed)
                    var visible = 0, rowsTotal = 0, runs = 0
                    for tile in tiles {
                        let rect = CGRect(x: tile.rect.minX * scale + offset.x, y: tile.rect.minY * scale + offset.y,
                            width: tile.rect.width * scale, height: tile.rect.height * scale)
                        guard rect.intersects(CGRect(origin: .zero, size: viewport)) else { continue }; visible += 1
                        let rows = AtlasSource.visibleRows(count: tile.lineCount, top: rect.minY + 22 * scale,
                            lineHeight: AtlasTextTile.lineHeight * scale, viewportHeight: viewport.height)
                        rowsTotal += rows.count
                        if let lines = tile.preparedLines { runs += (tile.preparedHeader?.runs.count ?? 0) + rows.reduce(0) { $0 + lines[$1].runs.count } }
                        else { runs += CFArrayGetCount(CTLineGetGlyphRuns(tile.header!)) + rows.reduce(0) { $0 + CFArrayGetCount(CTLineGetGlyphRuns(tile.lines[$1])) } }
                    }
                    visibleCounts.append(visible); rowCounts.append(rowsTotal); runCounts.append(runs)
                    if frame == 0 || frame == 119 {
                        var hash = SHA256(); hash.update(data: bitmap.makeImage()!.dataProvider!.data! as Data); signatures.append(digest(hash))
                    }
                }
                let ordered = samples.sorted()
                let percentile: (Double) -> Double = { ordered[min(ordered.count - 1, Int(ceil($0 * Double(ordered.count))) - 1)] }
                try output(["phase": "cpu-bitmap-replay", "scale": scale, "panning": moving, "frames": samples.count,
                    "viewport": [viewport.width, viewport.height], "backing_scale": backing,
                    "p50_ms": percentile(0.5), "p95_ms": percentile(0.95), "p99_ms": percentile(0.99),
                    "min_ms": ordered.first!, "max_ms": ordered.last!, "visible_tiles_range": [visibleCounts.min()!, visibleCounts.max()!],
                    "visible_rows_range": [rowCounts.min()!, rowCounts.max()!], "scheduled_runs_range": [runCounts.min()!, runCounts.max()!],
                    "first_last_pixels": signatures, "scan_tiles_per_frame": tiles.count])
            }
        }
        withExtendedLifetime(documents) {}
    }
    @MainActor static func retained(tiles: [AtlasTextTile], bounds: CGRect) throws {
        let surface = AtlasRetainedSurface(metalEnabled: false) // Preserve the independent CPU oracle.
        surface.frame = CGRect(origin: .zero, size: viewport)
        surface.setBackingScale(backing)
        let revision = UUID()
        let focusPath = ProcessInfo.processInfo.environment["FCB_RENDER_FOCUS"]
        let focusTile = focusPath == "smallest" ? tiles.filter { $0.parcelFirst && $0.lineCount > 0 }.min {
            $0.rect.width * $0.rect.height < $1.rect.width * $1.rect.height
        } : tiles.first { $0.path == focusPath }
        let center = focusTile?.rect ?? bounds
        try output(["phase": "profile-viewport", "viewport": [viewport.width, viewport.height],
                    "backing": backing, "focus": focusTile?.path ?? "atlas-center", "scales": scales])
        func update(_ scale: Double, _ offset: CGPoint) {
            let rasters = surface.metrics.detailRasters
            let captions = surface.metrics.captionRasters
            surface.update(tiles: tiles, revision: revision, scale: scale, offset: offset,
                           selectedPath: nil, hitPaths: [])
            precondition(surface.metrics.detailRasters == rasters, "camera update must not replay glyphs")
            precondition(surface.metrics.captionRasters == captions, "camera update must not shape captions")
            precondition(surface.metrics.captionBytes <= AtlasRetainedSurface.captionByteLimit)
            precondition(surface.metrics.detailBytes <= AtlasRetainedSurface.residentByteLimit)
        }
        func drain() {
            var passes = 0
            while surface.hasPendingDetail {
                let count = surface.metrics.detailRasters
                surface.prepareDetailPass(); passes += 1
                precondition(surface.metrics.detailRasters - count <= 16, "per-pass raster admission")
                precondition(passes <= AtlasRetainedSurface.maximumRequests + AtlasRetainedSurface.maximumVisibleCaptions, "bounded pending work must settle within request and caption admission")
            }
        }
        func verifyPatches() throws -> Int {
            var checked = 0
            for patch in surface.retainedDetailImages {
                let tile = tiles[patch.tile], image = patch.image
                let context = CGContext(data: nil, width: image.width, height: image.height, bitsPerComponent: 8,
                    bytesPerRow: image.width * 4, space: CGColorSpaceCreateDeviceRGB(),
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
                context.translateBy(x: 0, y: Double(image.height))
                context.scaleBy(x: Double(image.width) / patch.rect.width, y: -Double(image.height) / patch.rect.height)
                context.translateBy(x: -patch.rect.minX, y: -patch.rect.minY)
                context.scaleBy(x: tile.contentScale, y: tile.contentScale)
                // Independent full-line oracle: no candidate row-culling formula.
                tile.draw(in: context)
                let expected = context.makeImage()!.dataProvider!.data! as Data
                let actual = image.dataProvider!.data! as Data
                if expected != actual {
                    FileHandle.standardError.write(Data("PATCH_PIXEL_MISMATCH tile=\(patch.tile) rect=\(patch.rect) level=\(patch.level)\n".utf8))
                    preconditionFailure("detail patch must equal full production text clipped to identical pixels")
                }
                checked += 1
            }
            return checked
        }
        var adequateOverviewTiles = 0
        func verifyVisibleCoverage(scale: Double, offset: CGPoint) -> Int {
            let view = CGRect(x: -offset.x / scale, y: -offset.y / scale,
                              width: viewport.width / scale, height: viewport.height / scale)
            let patches = surface.retainedDetailImages.filter { $0.visible }
            var checked = 0
            adequateOverviewTiles = 0
            let overviews = Dictionary(uniqueKeysWithValues: surface.retainedOverviewImages.map { ($0.tile, $0) })
            for patch in patches {
                let target = patch.rect.offsetBy(dx: tiles[patch.tile].rect.minX, dy: tiles[patch.tile].rect.minY).intersection(view)
                guard !target.isNull, target.width > 0, target.height > 0 else { continue }
                let densityX = Double(patch.image.width) / patch.rect.width
                let densityY = Double(patch.image.height) / patch.rect.height
                if min(densityX, densityY) + 1e-8 < scale * backing {
                    FileHandle.standardError.write(Data("VISIBLE_DENSITY_MISSING actual=\(min(densityX, densityY)) expected=\(scale * backing) tile=\(patch.tile)\n".utf8))
                    preconditionFailure("visible detail must never magnify an undersampled raster")
                }
            }
            for (index, tile) in tiles.enumerated() {
                let target = tile.rect.intersection(view)
                guard !target.isNull, target.width > 0, target.height > 0 else { continue }
                // Credit a cached overview only from its actual image dimensions
                // and world frame. Its contents must still equal the immutable
                // source-raster oracle checked before this profile began.
                if let overview = overviews[index], overview.visible,
                   overview.rect.contains(target), overview.rect.width > 0, overview.rect.height > 0,
                   min(Double(overview.image.width) / overview.rect.width,
                       Double(overview.image.height) / overview.rect.height) + 1e-8 >= scale * backing {
                    guard let original = tile.raster else { preconditionFailure("original raster required") }
                    precondition(original.width == overview.image.width && original.height == overview.image.height &&
                        original.dataProvider!.data! as Data == overview.image.dataProvider!.data! as Data,
                        "adequate overview must be the actual original text raster")
                    if min(Double(overview.image.width) / overview.rect.width,
                           Double(overview.image.height) / overview.rect.height) >= scale * backing * 1.15 {
                        precondition(!surface.retainedDetailImages.contains { $0.tile == index && $0.visible },
                                     "adequate cached text must not be obscured by a fallback detail image")
                    }
                    adequateOverviewTiles += 1
                    checked += 1
                    continue
                }
                if !sharpPatchCoverage(patches, tile: tile, index: index, target: target, density: scale * backing) {
                    FileHandle.standardError.write(Data("VISIBLE_COVERAGE_MISSING tile=\(index) scale=\(scale)\n".utf8))
                    preconditionFailure("settled readable source must have complete visible detail coverage")
                }
                checked += 1
            }
            return checked
        }
        func require(_ condition: Bool, _ message: String = "caption geometry assertion") {
            guard condition else {
                FileHandle.standardError.write(Data("PRESENTATION_ASSERTION: \(message)\n".utf8))
                preconditionFailure(message)
            }
        }
        func verifyCaptions(scale: Double, offset: CGPoint) {
            let screen = CGRect(origin: .zero, size: viewport)
            var expected: Set<String> = []
            for tile in tiles where tile.parcelFirst {
                guard let parcel = tile.parcelRect else { continue }
                let projected = CGRect(x: parcel.minX * scale + offset.x, y: parcel.minY * scale + offset.y,
                                       width: parcel.width * scale, height: parcel.height * scale)
                let clipped = projected.intersection(screen)
                if !clipped.isNull && clipped.width >= 48 && clipped.height >= 24 { expected.insert(tile.path) }
            }
            let shown = surface.retainedCaptionImages.filter(\.visible)
            require(Set(shown.map(\.path)) == expected, "every eligible visible file must have a filename caption")
            for caption in shown {
                require(screen.contains(caption.frame), "sticky captions must remain inside the viewport")
                require(caption.frame.width > 0 && caption.frame.height == 22)
                require(caption.image.height >= Int(22 * backing), "filename image retains display density")
                let image = caption.image
                let bitmap = CGContext(data: nil, width: image.width, height: image.height, bitsPerComponent: 8,
                    bytesPerRow: image.width * 4, space: CGColorSpaceCreateDeviceRGB(),
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
                bitmap.setFillColor(Monokai.background.withAlphaComponent(0.94).cgColor)
                bitmap.fill(CGRect(x: 0, y: 0, width: image.width, height: image.height))
                bitmap.translateBy(x: 0, y: Double(image.height))
                bitmap.scaleBy(x: Double(caption.backing), y: -Double(caption.backing))
                bitmap.textMatrix = CGAffineTransform(scaleX: 1, y: -1)
                bitmap.textPosition = CGPoint(x: 6, y: 16)
                let line = CTLineCreateWithAttributedString(NSAttributedString(string: (caption.path as NSString).lastPathComponent,
                    attributes: [.font: NSFont.monospacedSystemFont(ofSize: 13, weight: .medium), .foregroundColor: NSColor.white]))
                CTLineDraw(line, bitmap)
                require(bitmap.makeImage()!.dataProvider!.data! as Data == image.dataProvider!.data! as Data,
                             "filename bitmap preserves the actual filename glyphs")
            }
        }
        func verifyDirectories(scale: Double, offset: CGPoint) {
            var expected: [String: CGRect] = [:]
            for tile in tiles where tile.parcelFirst {
                guard let parcel = tile.parcelRect else { continue }
                var parts = tile.path.split(separator: "/").map(String.init)
                parts.removeLast()
                while !parts.isEmpty {
                    let path = parts.joined(separator: "/")
                    expected[path] = (expected[path] ?? .null).union(parcel)
                    parts.removeLast()
                }
            }
            let actual = surface.retainedDirectoryRects
            require(actual.count == expected.count && Set(actual.map(\.path)).count == expected.count,
                         "directory outlines retain distinct authoritative path identities")
            let view = CGRect(x: -offset.x / scale, y: -offset.y / scale,
                              width: viewport.width / scale, height: viewport.height / scale)
            let visiblePaths = Set(expected.filter {
                $0.value.intersects(view) && min($0.value.width, $0.value.height) * scale >= 12
            }.keys)
            require(surface.retainedDirectoryDrawnPaths == visiblePaths, "every eligible visible directory has an actual stroked path")
            for directory in actual {
                if expected[directory.path] != directory.rect {
                    FileHandle.standardError.write(Data("DIRECTORY_BOUNDS path=\(directory.path) expected=\(String(describing: expected[directory.path])) actual=\(directory.rect)\n".utf8))
                }
                require(expected[directory.path] == directory.rect, "directory bounds contain the exact descendant parcel union")
            }
        }
        func verifyTransition(scale: Double, offset: CGPoint) throws {
            let view = CGRect(x: -offset.x / scale, y: -offset.y / scale,
                width: viewport.width / scale, height: viewport.height / scale)
            let snapshot = surface.retainedTransitionImage
            if let snapshot {
                let image = snapshot.image
                require(snapshot.rect.contains(view), "transition contains the current viewport")
                let canvas = snapshot.rect
                require(Double(image.width) / canvas.width >= scale * backing &&
                    Double(image.height) / canvas.height >= scale * backing, "transition has physical display density")
                let bitmap = CGContext(data: nil, width: image.width, height: image.height,
                    bitsPerComponent: 8, bytesPerRow: image.width * 4, space: CGColorSpaceCreateDeviceRGB(),
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
                bitmap.translateBy(x: 0, y: Double(image.height))
                bitmap.scaleBy(x: Double(image.width) / canvas.width, y: -Double(image.height) / canvas.height)
                bitmap.translateBy(x: -canvas.minX, y: -canvas.minY)
                for index in snapshot.tiles {
                    let tile = tiles[index], clipped = tiles[index].rect.intersection(canvas)
                    bitmap.saveGState(); bitmap.clip(to: clipped)
                    bitmap.setFillColor(Monokai.background.cgColor); bitmap.fill(clipped)
                    bitmap.translateBy(x: tile.rect.minX, y: tile.rect.minY)
                    bitmap.scaleBy(x: tile.contentScale, y: tile.contentScale)
                    tile.draw(in: bitmap) // Independent full-row oracle, no candidate row culling.
                    bitmap.restoreGState()
                }
                require(bitmap.makeImage()!.dataProvider!.data! as Data == image.dataProvider!.data! as Data,
                        "transition pixels equal complete shaped source at current resolution")
            }
            let transitioned = Set(snapshot?.tiles ?? [])
            let overviews = Dictionary(uniqueKeysWithValues: surface.retainedOverviewImages.map { ($0.tile, $0) })
            let patches = surface.retainedDetailImages.filter { $0.visible }
            for (index, tile) in tiles.enumerated() {
                let target = tile.rect.intersection(view)
                guard !target.isNull, target.width > 0, target.height > 0 else { continue }
                if transitioned.contains(index) { continue }
                if let overview = overviews[index], overview.visible,
                    min(Double(overview.image.width) / overview.rect.width,
                        Double(overview.image.height) / overview.rect.height) >= scale * backing { continue }
                require(sharpPatchCoverage(patches, tile: tile, index: index, target: target,
                    density: scale * backing),
                        "every visible source tile must be sharp BEFORE detail preparation")
            }
            require(surface.metrics.transitionFailures == 0, "transition allocation must succeed on test viewport")
            require(surface.metrics.transitionBytes * 3 <= AtlasRetainedSurface.transitionByteLimit,
                    "transition replacement allocation remains bounded")
        }
        for scale in scales {
            for moving in [false, true] {
                let initial = surface.metrics
                var samples: [Double] = [], passTimes: [Double] = []
                var finalOffset = CGPoint.zero
                for frame in 0..<120 {
                    let t = Double(frame) / 119
                    finalOffset = CGPoint(x: viewport.width / 2 - center.midX * scale + (moving ? sin(t * .pi * 2) * 160 : 0),
                                          y: viewport.height / 2 - center.midY * scale + (moving ? cos(t * .pi * 2) * 120 : 0))
                    let start = now(); update(scale, finalOffset); samples.append((now() - start) * 1000)
                    if frame == 0 { try verifyTransition(scale: scale, offset: finalOffset) }
                    if surface.hasPendingDetail {
                        let pass = now(); surface.prepareDetailPass(); passTimes.append((now() - pass) * 1000)
                    }
                }
                drain()
                require(surface.retainedTransitionImage == nil, "settled camera retires transition image")
                let beforeStable = surface.metrics
                for _ in 0..<20 { update(scale, finalOffset) }
                precondition(!surface.hasPendingDetail && surface.metrics.detailRasters == beforeStable.detailRasters,
                             "stationary settled camera must reuse prepared images")
                require(surface.metrics.transitionRasters == beforeStable.transitionRasters,
                        "settled camera never replays transition glyphs")
                precondition(surface.metrics.detailHits == beforeStable.detailHits, "stationary camera must not repeat detail admission")
                precondition(surface.metrics.overviewAssignments == tiles.count, "one overview assignment per installed tile")
                try output(["phase": "coverage-input", "scale": scale, "panning": moving,
                    "viewport": [viewport.width, viewport.height], "requested": surface.metrics.requestedPatches,
                    "visible_requested": surface.metrics.visibleRequestedPatches,
                    "visible_refused": surface.metrics.visibleRefusedPatches,
                    "allocation_refused": surface.metrics.visibleAllocationRefusals,
                    "resident_bytes": surface.metrics.detailBytes])
                let verified = try verifyPatches()
                let coveredTiles = verifyVisibleCoverage(scale: scale, offset: finalOffset)
                verifyCaptions(scale: scale, offset: finalOffset)
                verifyDirectories(scale: scale, offset: finalOffset)
                precondition(surface.metrics.visibleRefusedPatches == 0 && surface.metrics.visibleAllocationRefusals == 0, "ordinary viewport must admit all visible detail")
                let ordered = samples.sorted()
                let percentile: (Double) -> Double = { ordered[min(ordered.count - 1, Int(ceil($0 * Double(ordered.count))) - 1)] }
                let metrics = surface.metrics
                try output(["phase": "retained-camera-update", "scale": scale, "panning": moving,
                    "frames": samples.count, "covered_visible_tiles": coveredTiles, "adequate_cached_overview_tiles": adequateOverviewTiles, "p50_ms": percentile(0.5), "p95_ms": percentile(0.95), "p99_ms": percentile(0.99),
                    "first_update_ms": samples[0], "transition_rasters": metrics.transitionRasters - initial.transitionRasters,
                    "transition_cpu_ms": (metrics.transitionSeconds - initial.transitionSeconds) * 1000,
                    "detail_rasters": metrics.detailRasters - initial.detailRasters, "detail_hits": metrics.detailHits - initial.detailHits,
                    "detail_evictions": metrics.detailEvictions - initial.detailEvictions, "detail_failures": metrics.detailFailures,
                    "detail_bytes": metrics.detailBytes, "peak_detail_bytes": metrics.peakDetailBytes,
                    "detail_pass_max_ms": (passTimes.max() ?? 0), "detail_total_ms": (metrics.detailSeconds - initial.detailSeconds) * 1000,
                    "detail_passes": metrics.detailPasses - initial.detailPasses, "verified_patch_images": verified,
                    "requested_patches": metrics.requestedPatches,
                    "visible_requested": metrics.visibleRequestedPatches, "visible_refused": metrics.visibleRefusedPatches,
                    "visible_allocation_refusals": metrics.visibleAllocationRefusals, "prefetch_requested": metrics.prefetchRequestedPatches, "admission_limited_queries": metrics.admissionLimitedQueries,
                    "overview_assignments": metrics.overviewAssignments,
                    "caption_rasters": metrics.captionRasters - initial.captionRasters, "caption_bytes": metrics.captionBytes,
                    "caption_failures": metrics.captionFailures, "visible_captions": metrics.visibleCaptions,
                    "visible_current_patches": surface.retainedDetailImages.filter { $0.visible && $0.currentTier }.count,
                    "visible_fallback_patches": surface.retainedDetailImages.filter { $0.visible && !$0.currentTier }.count])
            }
        }
        let beforeBacking = surface.metrics.detailRasters
        surface.setBackingScale(1); drain()
        surface.setBackingScale(2); drain()
        let verified = try verifyPatches()
        precondition(surface.metrics.detailBytes <= AtlasRetainedSurface.residentByteLimit)
        try output(["phase": "backing-change", "additional_rasters": surface.metrics.detailRasters - beforeBacking,
                    "verified_patch_images": verified, "resident_bytes": surface.metrics.detailBytes])
        let band = CGRect(x: tiles[0].rect.minX, y: tiles[0].rect.minY + 22 * tiles[0].contentScale,
                          width: tiles[0].rect.width, height: 16 * tiles[0].contentScale)
        let beforeMatches = surface.metrics.detailRasters
        for scale in [0.2, 0.4, 1.6] {
            surface.update(tiles: tiles, revision: revision, scale: scale,
                           offset: CGPoint(x: 19 * scale, y: -31 * scale),
                           selectedPath: tiles[0].path, hitPaths: [], matchRows: [band])
            precondition(surface.retainedMatchRects == [band], "retained occurrence follows source world coordinates")
            precondition(surface.metrics.detailRasters == beforeMatches, "highlight and camera updates do not rasterize glyphs")
        }
        surface.update(tiles: tiles, revision: revision, scale: 0.4, offset: .zero,
                       selectedPath: nil, hitPaths: [], matchRows: [])
        precondition(surface.retainedMatchRects.isEmpty, "clearing selection removes occurrence bands")
        surface.update(tiles: tiles, revision: revision, scale: 0.4, offset: .zero,
                       selectedPath: tiles[0].path, hitPaths: [], matchRows: Array(repeating: band, count: 257))
        precondition(surface.retainedMatchRects.count == 256, "overlay resource admission is bounded")
        surface.update(tiles: [], revision: UUID(), scale: 0.4, offset: .zero, selectedPath: nil, hitPaths: [])
        precondition(surface.retainedMatchRects.isEmpty, "project revision retires occurrence bands")
        try output(["phase": "retained-matches", "camera_updates": 3, "max_bands": 256,
                    "additional_rasters": surface.metrics.detailRasters - beforeMatches])
        surface.stopClock()
    }

}
