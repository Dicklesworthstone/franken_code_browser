import AppKit
import CoreText
import Metal

@main struct AtlasMetalGlyphTests {
    @MainActor static func main() throws {
        guard let device = MTLCreateSystemDefaultDevice(), let queue = device.makeCommandQueue() else {
            fatalError("Real Metal device and command queue required")
        }
        print("Metal device: \(device.name), unifiedMemory=\(device.hasUnifiedMemory), registryID=\(device.registryID)")
        func prepared(_ text: String, font: NSFont, color: NSColor) -> (AtlasPreparedLine, CTLine) {
            let line = CTLineCreateWithAttributedString(NSAttributedString(string: text,
                attributes: [.font: font, .foregroundColor: color]))
            guard let value = AtlasPreparedLine(line, source: text) else { fatalError("Actual shaped input required") }
            return (value, line)
        }
        let (red, original) = prepared("MMMMMMMMMM", font: .monospacedSystemFont(ofSize: 18, weight: .regular), color: .red)
        let (green, _) = prepared("iiiiiiiiii", font: .systemFont(ofSize: 20), color: .green)
        let clip = CGRect(x: 10, y: 10, width: 60, height: 65)
        let renderer = try AtlasMetalGlyphRenderer(device: device, lines: [
            .init(text: red, origin: CGPoint(x: 12, y: 34), clip: clip),
            .init(text: green, origin: CGPoint(x: 12, y: 64), clip: clip)
        ], pixelsPerPoint: 2)
        precondition(renderer.instanceCount == 20 && renderer.distinctGlyphCount == 2,
                     "Actual font/glyph keys deduplicate repeated glyphs across retained instances")
        precondition(renderer.managedBytes < AtlasMetalGlyphRenderer.maximumManagedBytes)
        let identity = renderer.retainedInstanceIdentity
        func render(scale: Double, offset: CGPoint) throws -> [UInt8] {
            let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .bgra8Unorm,
                width: 256, height: 160, mipmapped: false)
            descriptor.storageMode = .shared; descriptor.usage = .renderTarget
            guard let target = device.makeTexture(descriptor: descriptor), let command = queue.makeCommandBuffer() else {
                fatalError("Real GPU resources required")
            }
            try renderer.encode(into: target, commandBuffer: command, scale: scale, offset: offset)
            command.commit(); command.waitUntilCompleted() // Qualification only, never production.
            precondition(command.status == .completed && command.error == nil, "GPU execution must complete")
            var pixels = [UInt8](repeating: 0, count: 256 * 160 * 4)
            pixels.withUnsafeMutableBytes { target.getBytes($0.baseAddress!, bytesPerRow: 256 * 4,
                from: MTLRegionMake2D(0, 0, 256, 160), mipmapLevel: 0) }
            return pixels
        }
        let first = try render(scale: 1, offset: .zero)
        let moved = try render(scale: 1, offset: CGPoint(x: 8, y: 4))
        let zoomed = try render(scale: 2, offset: .zero)
        precondition(renderer.retainedInstanceIdentity == identity, "Camera transforms retain the same GPU instance buffer")
        var redPixels = 0, greenPixels = 0, zoomedInk = 0
        for y in 0..<160 { for x in 0..<256 {
            let i = (y * 256 + x) * 4
            if first[i + 3] > 0 {
                precondition(x >= 10 && x < 70 && y >= 10 && y < 75, "Source clipping is exact in world coordinates")
                if first[i + 2] > 0 { redPixels += 1; precondition(first[i] == 0 && first[i + 1] == 0) }
                if first[i + 1] > 0 { greenPixels += 1; precondition(first[i] == 0 && first[i + 2] == 0) }
                precondition(first[i + 2] <= first[i + 3] && first[i + 1] <= first[i + 3], "Premultiplied color")
            }
            if x >= 8 && y >= 4 {
                let prior = ((y - 4) * 256 + x - 8) * 4
                precondition(Array(moved[i..<i + 4]) == Array(first[prior..<prior + 4]), "Integer camera translation moves actual glyph pixels")
            } else { precondition(moved[i + 3] == 0) }
            if zoomed[i + 3] > 0 {
                zoomedInk += 1
                precondition(x >= 20 && x < 140 && y >= 20 && y < 150)
            }
        } }
        precondition(redPixels > 30 && greenPixels > 30 && zoomedInk > redPixels + greenPixels,
                     "Both real fonts/colors contain GPU-rendered mask coverage at both scales")
        do { _ = try render(scale: 2.01, offset: .zero); fatalError("Must not magnify masks") }
        catch AtlasMetalGlyphRenderer.Fallback.undersampled { }
        var unsupported = red; unsupported.retainedColorLine = original
        do {
            _ = try AtlasMetalGlyphRenderer(device: device, lines: [.init(text: unsupported, origin: .zero, clip: clip)], pixelsPerPoint: 2)
            fatalError("Unsupported color-line route must request whole-scene CPU fallback")
        } catch AtlasMetalGlyphRenderer.Fallback.unsupportedFontOrTransform { }
        let (emoji, _) = prepared("😀", font: .systemFont(ofSize: 20), color: .white)
        precondition(emoji.hasColorGlyphs, "Actual system fallback must select a color font")
        do {
            _ = try AtlasMetalGlyphRenderer(device: device, lines: [.init(text: emoji, origin: .zero, clip: clip)], pixelsPerPoint: 2)
            fatalError("Actual color font requires explicit CPU fallback")
        } catch AtlasMetalGlyphRenderer.Fallback.unsupportedFontOrTransform { }
        // Independent full-line CoreText reference at exactly the prepared density.
        // Asymmetric ascenders/descenders detect flipped, mirrored or wrong masks.
        for character in ["F", "g", "j", "R"] {
            let (shape, referenceLine) = prepared(character, font: .monospacedSystemFont(ofSize: 24, weight: .regular), color: .white)
            let glyphRenderer = try AtlasMetalGlyphRenderer(device: device, lines: [
                .init(text: shape, origin: CGPoint(x: 20, y: 40), clip: CGRect(x: 0, y: 0, width: 64, height: 64))
            ], pixelsPerPoint: 2)
            let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .bgra8Unorm, width: 128, height: 128, mipmapped: false)
            descriptor.storageMode = .shared; descriptor.usage = .renderTarget
            let target = device.makeTexture(descriptor: descriptor)!
            let command = queue.makeCommandBuffer()!
            try glyphRenderer.encode(into: target, commandBuffer: command, scale: 2, offset: .zero)
            command.commit(); command.waitUntilCompleted()
            precondition(command.status == .completed && command.error == nil)
            var actual = [UInt8](repeating: 0, count: 128 * 128 * 4)
            actual.withUnsafeMutableBytes { target.getBytes($0.baseAddress!, bytesPerRow: 128 * 4,
                from: MTLRegionMake2D(0, 0, 128, 128), mipmapLevel: 0) }
            var expected = [UInt8](repeating: 0, count: actual.count)
            expected.withUnsafeMutableBytes { raw in
                let bitmap = CGContext(data: raw.baseAddress, width: 128, height: 128, bitsPerComponent: 8,
                    bytesPerRow: 128 * 4, space: CGColorSpaceCreateDeviceRGB(),
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
                bitmap.translateBy(x: 40, y: 128 - 80)
                bitmap.scaleBy(x: 2, y: 2)
                bitmap.textMatrix = .identity
                bitmap.textPosition = .zero
                CTLineDraw(referenceLine, bitmap)
            }
            if character == "F" {
                for pixels in [actual, expected] {
                    let rows = (0..<128).map { row in
                        (0..<128).filter { pixels[(row * 128 + $0) * 4 + 3] >= 32 }.count
                    }
                    let inkRows = rows.indices.filter { rows[$0] > 0 }
                    precondition(!inkRows.isEmpty)
                    let top = inkRows.first!, bottom = inkRows.last!
                    precondition(rows[top...(top + 5)].max()! > rows[(bottom - 5)...bottom].max()! * 2,
                                 "Upright F has a wide top bar and a narrow bottom stem")
                }
            }
            var overlap = 0, union = 0, error = 0, referenceInk = 0
            for pixel in 0..<(128 * 128) {
                let a = Int(actual[pixel * 4 + 3]), e = Int(expected[pixel * 4 + 3])
                if a >= 32 && e >= 32 { overlap += 1 }
                if a >= 32 || e >= 32 { union += 1 }
                error += abs(a - e); referenceInk += e
            }
            // These are explicit shape-quality gates, not byte-equality claims.
            // Different CoreText raster surfaces can differ at antialiased edges.
            let iou = Double(overlap) / Double(max(1, union))
            let normalizedError = Double(error) / Double(max(1, referenceInk))
            let failedQuality = iou < 0.90 || normalizedError > 0.12 || referenceInk == 0
            if failedQuality || ProcessInfo.processInfo.environment["FCB_METAL_GLYPH_ARTIFACTS"] == "1" {
                let directory = FileManager.default.temporaryDirectory.appendingPathComponent("fcb-metal-glyph-" + UUID().uuidString)
                try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
                try Data(actual).write(to: directory.appendingPathComponent(character + "-gpu.bgra"))
                try Data(expected).write(to: directory.appendingPathComponent(character + "-coretext.rgba"))
                FileHandle.standardError.write(Data("METAL_GLYPH_QUALITY glyph=\(character) iou=\(iou) alpha_error=\(normalizedError) raw128x128=\(directory.path)\n".utf8))
                precondition(!failedQuality, "Actual Metal glyph must match independent CoreText orientation and coverage")
            }
            print("Metal glyph \(character): IoU=\(iou), normalized alpha error=\(normalizedError)")
        }
        let run = red.runs[0]
        let transformed = AtlasPreparedLine(runs: [AtlasPreparedRun(font: run.font, color: run.color,
            matrix: CGAffineTransform(scaleX: 1.2, y: 1), glyphs: run.glyphs,
            positions: run.positions, fallbackText: run.fallbackText)])
        do {
            _ = try AtlasMetalGlyphRenderer(device: device, lines: [.init(text: transformed, origin: .zero, clip: clip)], pixelsPerPoint: 2)
            fatalError("Nonidentity run matrix requires explicit CPU fallback")
        } catch AtlasMetalGlyphRenderer.Fallback.unsupportedFontOrTransform { }
        let (sceneRed, _) = prepared("MMMMMMMMMM", font: .monospacedSystemFont(ofSize: 18, weight: .regular),
            color: NSColor(srgbRed: 249.0 / 255, green: 38.0 / 255, blue: 114.0 / 255, alpha: 1))
        let (sceneGreen, _) = prepared("iiiiiiiiii", font: .systemFont(ofSize: 20),
            color: NSColor(srgbRed: 166.0 / 255, green: 226.0 / 255, blue: 46.0 / 255, alpha: 1))
        let firstRect = CGRect(x: 10, y: 10, width: 100, height: 100)
        let secondRect = CGRect(x: 140, y: 10, width: 100, height: 100)
        let scene = try AtlasMetalGlyphRenderer(device: device, tiles: [
            .init(id: 7, rect: firstRect, sourceScale: 2,
                  lines: [.init(text: sceneRed, origin: CGPoint(x: 4, y: 24), clip: firstRect)]),
            .init(id: 8, rect: secondRect, sourceScale: 1,
                  lines: [.init(text: emoji, origin: CGPoint(x: 4, y: 24), clip: secondRect)]),
            .init(id: 9, rect: secondRect, sourceScale: 1,
                  lines: [.init(text: sceneGreen, origin: CGPoint(x: 4, y: 24), clip: secondRect)])
        ], pixelsPerPoint: 2, previousManagedBytes: renderer.managedBytes)
        precondition(Set(scene.batches.keys) == Set([7, 9]) && Set(scene.fallbackTiles.keys) == Set([8]),
                     "Unsupported tile falls back atomically while supported siblings remain GPU eligible")
        precondition(scene.batches[7]!.pixelsPerWorldPoint == 1 && scene.batches[9]!.pixelsPerWorldPoint == 2)
        precondition(scene.instanceCount == 22, "Only two complete backgrounds plus twenty glyphs survive")
        let sceneDescriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .bgra8Unorm,
            width: 256, height: 160, mipmapped: false)
        sceneDescriptor.storageMode = .shared; sceneDescriptor.usage = .renderTarget
        let sceneTarget = device.makeTexture(descriptor: sceneDescriptor)!
        let sceneCommand = queue.makeCommandBuffer()!
        try scene.encode(into: sceneTarget, commandBuffer: sceneCommand, scale: 1, offset: .zero, tileIDs: [7])
        sceneCommand.commit(); sceneCommand.waitUntilCompleted()
        precondition(sceneCommand.status == .completed && sceneCommand.error == nil)
        var scenePixels = [UInt8](repeating: 0, count: 256 * 160 * 4)
        scenePixels.withUnsafeMutableBytes { sceneTarget.getBytes($0.baseAddress!, bytesPerRow: 256 * 4,
            from: MTLRegionMake2D(0, 0, 256, 160), mipmapLevel: 0) }
        let background = (11 * 256 + 11) * 4
        precondition(Array(scenePixels[background..<background + 4]) == [29, 26, 22, 255],
                     "Opaque Monokai backing prevents old CPU text from bleeding through")
        precondition(scenePixels[(11 * 256 + 141) * 4 + 3] == 0,
                     "Unselected batch remains transparent for the CPU fallback route")
        var redInk = 0
        for y in 10..<110 { for x in 10..<110 {
            let i = (y * 256 + x) * 4
            if scenePixels[i + 2] > 100 && scenePixels[i + 1] < 40 { redInk += 1 }
        } }
        precondition(redInk > 100, "Scaled source glyphs render inside their transformed world parcel")
        do {
            let command = queue.makeCommandBuffer()!
            try scene.encode(into: sceneTarget, commandBuffer: command, scale: 1.01, offset: .zero, tileIDs: [7])
            fatalError("Per-tile scale must determine actual mask density")
        } catch AtlasMetalGlyphRenderer.Fallback.undersampled { }
        let tallRect = CGRect(x: 0, y: 0, width: 256, height: 10_000)
        let excessiveFonts = (0..<65).map { index -> AtlasMetalGlyphRenderer.Line in
            let (text, _) = prepared("M", font: .monospacedSystemFont(ofSize: Double(8 + index), weight: .regular), color: .white)
            return .init(text: text, origin: CGPoint(x: 4, y: Double(100 + index * 100)), clip: tallRect)
        }
        let recovered = try AtlasMetalGlyphRenderer(device: device, tiles: [
            .init(id: 1, rect: tallRect, sourceScale: 1, lines: excessiveFonts),
            .init(id: 2, rect: secondRect, sourceScale: 1,
                  lines: [.init(text: sceneGreen, origin: CGPoint(x: 4, y: 24), clip: secondRect)])
        ], pixelsPerPoint: 1)
        precondition(recovered.fallbackTiles[1] == .resourceLimit && recovered.batches[2] != nil,
                     "A rejected tile must return font and atlas capacity to later supported siblings")
        precondition(recovered.distinctGlyphCount == 1 && recovered.instanceCount == 11,
                     "No masks, backgrounds or glyphs from the failed tile survive")
        // Strong minification must integrate thin strokes, not point-sample
        // a large mask. Compare total coverage to an independent supersampled
        // CoreText image, using separated glyphs so alpha does not overlap.
        let (tinyText, tinyReference) = prepared("F", font: .monospacedSystemFont(ofSize: 13, weight: .regular), color: .white)
        var tinyOrigins: [CGPoint] = []
        for index in 0..<16 {
            let column = index % 4, rowIndex = index / 4
            tinyOrigins.append(CGPoint(x: Double(40 + column * 40), y: Double(40 + rowIndex * 40)))
        }
        let tinyLines = tinyOrigins.map { AtlasMetalGlyphRenderer.Line(text: tinyText, origin: $0,
            clip: CGRect(x: 0, y: 0, width: 256, height: 256)) }
        let tinyRenderer = try AtlasMetalGlyphRenderer(device: device, lines: tinyLines, pixelsPerPoint: 8)
        var highResolution = [UInt8](repeating: 0, count: 2048 * 2048)
        highResolution.withUnsafeMutableBytes { raw in
            let bitmap = CGContext(data: raw.baseAddress, width: 2048, height: 2048, bitsPerComponent: 8,
                bytesPerRow: 2048, space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGImageAlphaInfo.none.rawValue)!
            bitmap.scaleBy(x: 8, y: 8)
            for origin in tinyOrigins {
                bitmap.textPosition = CGPoint(x: origin.x, y: 256 - origin.y)
                CTLineDraw(tinyReference, bitmap)
            }
        }
        let expectedCoverage = Double(highResolution.reduce(0) { $0 + Int($1) }) / (64 * 64)
        precondition(expectedCoverage > 0)
        let tinyDescriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .bgra8Unorm,
            width: 32, height: 32, mipmapped: false)
        tinyDescriptor.storageMode = .shared; tinyDescriptor.usage = .renderTarget
        let tinyTarget = device.makeTexture(descriptor: tinyDescriptor)!
        for phase in [0.0, 0.25, 0.5, 0.75] {
            let command = queue.makeCommandBuffer()!
            try tinyRenderer.encode(into: tinyTarget, commandBuffer: command, scale: 0.125,
                                    offset: CGPoint(x: phase, y: phase))
            command.commit(); command.waitUntilCompleted()
            precondition(command.status == .completed && command.error == nil)
            var pixels = [UInt8](repeating: 0, count: 32 * 32 * 4)
            pixels.withUnsafeMutableBytes { tinyTarget.getBytes($0.baseAddress!, bytesPerRow: 32 * 4,
                from: MTLRegionMake2D(0, 0, 32, 32), mipmapLevel: 0) }
            let coverage = Double(stride(from: 3, to: pixels.count, by: 4).reduce(0) { $0 + Int(pixels[$1]) })
            let relativeError = abs(coverage - expectedCoverage) / expectedCoverage
            FileHandle.standardError.write(Data("METAL_MINIFIED_COVERAGE phase=\(phase) actual=\(coverage) expected=\(expectedCoverage) relative_error=\(relativeError)\n".utf8))
            precondition(relativeError <= 0.25, "Minified glyphs must preserve integrated CoreText coverage across pixel phases")
        }
        // Saturated Monokai color under the same opaque-background composition
        // used by the connected route; compare actual GPU RGB to CoreText.
        let keyword = NSColor(srgbRed: 249.0 / 255, green: 38.0 / 255, blue: 114.0 / 255, alpha: 1)
        let (colored, coloredReference) = prepared("F", font: .monospacedSystemFont(ofSize: 24, weight: .regular), color: keyword)
        let colorRect = CGRect(x: 0, y: 0, width: 64, height: 64)
        let colorLine = AtlasMetalGlyphRenderer.Line(text: colored, origin: CGPoint(x: 20, y: 40), clip: colorRect)
        let colorScene = try AtlasMetalGlyphRenderer(device: device,
            tiles: [.init(id: 0, rect: colorRect, sourceScale: 1, lines: [colorLine])], pixelsPerPoint: 2)
        let colorDescriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .bgra8Unorm, width: 128, height: 128, mipmapped: false)
        colorDescriptor.storageMode = .shared; colorDescriptor.usage = .renderTarget
        let colorTarget = device.makeTexture(descriptor: colorDescriptor)!
        let colorCommand = queue.makeCommandBuffer()!
        try colorScene.encode(into: colorTarget, commandBuffer: colorCommand, scale: 2, offset: .zero)
        colorCommand.commit(); colorCommand.waitUntilCompleted()
        precondition(colorCommand.status == .completed && colorCommand.error == nil)
        var colorPixels = [UInt8](repeating: 0, count: 128 * 128 * 4)
        colorPixels.withUnsafeMutableBytes { colorTarget.getBytes($0.baseAddress!, bytesPerRow: 128 * 4,
            from: MTLRegionMake2D(0, 0, 128, 128), mipmapLevel: 0) }
        var colorExpected = [UInt8](repeating: 0, count: colorPixels.count)
        colorExpected.withUnsafeMutableBytes { raw in
            let bitmap = CGContext(data: raw.baseAddress, width: 128, height: 128, bitsPerComponent: 8,
                bytesPerRow: 128 * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
            bitmap.setFillColor(CGColor(srgbRed: 22.0 / 255, green: 26.0 / 255, blue: 29.0 / 255, alpha: 1))
            bitmap.fill(CGRect(x: 0, y: 0, width: 128, height: 128))
            bitmap.translateBy(x: 40, y: 48); bitmap.scaleBy(x: 2, y: 2)
            CTLineDraw(coloredReference, bitmap)
        }
        var colorError = 0, colorEnergy = 0
        for pixel in 0..<(128 * 128) { for channel in 0..<3 {
            let actual = Int(colorPixels[pixel * 4 + 2 - channel]), expected = Int(colorExpected[pixel * 4 + channel])
            colorError += abs(actual - expected)
            colorEnergy += abs(expected - [22, 26, 29][channel])
        } }
        FileHandle.standardError.write(Data("METAL_MONOKAI_RGB relative_error=\(Double(colorError) / Double(max(1, colorEnergy)))\n".utf8))
        precondition(colorEnergy > 0 && Double(colorError) / Double(colorEnergy) <= 0.02,
                     "Saturated Monokai color agrees with the independent DeviceRGB CoreText reference")
        let overlapScene = try AtlasMetalGlyphRenderer(device: device,
            tiles: [.init(id: 0, rect: colorRect, sourceScale: 1, lines: [colorLine, colorLine])], pixelsPerPoint: 2)
        precondition(overlapScene.fallbackTiles[0] == .overlappingGlyphs && overlapScene.batches.isEmpty,
                     "Genuinely overlapping source ink requires CPU union composition")
        let (dark, _) = prepared("F", font: .monospacedSystemFont(ofSize: 24, weight: .regular), color: .black)
        let darkScene = try AtlasMetalGlyphRenderer(device: device, tiles: [
            .init(id: 0, rect: colorRect, sourceScale: 1,
                  lines: [.init(text: dark, origin: CGPoint(x: 20, y: 40), clip: colorRect)]),
            .init(id: 1, rect: colorRect, sourceScale: 1, lines: [colorLine])
        ], pixelsPerPoint: 2)
        precondition(darkScene.fallbackTiles[0] == .unsupportedColor && darkScene.batches[1] != nil,
                     "Dark glyphs require CPU blending without preventing supported Monokai rendering")
        do {
            let diagnosticMode = ProcessInfo.processInfo.environment["FCB_METAL_GLYPH_DIRECT_DIAGNOSTIC"] == "1"
            if diagnosticMode {
            let (_, anchorReference) = prepared("F", font: .monospacedSystemFont(ofSize: 13, weight: .regular), color: .white)
            for anchorScale in [0.4, 1.0, 2.0, 4.0, 8.0] {
            for flags in [-1, 3] {
                for phaseStep in 0..<8 {
                    let phase = Double(phaseStep) / 8
                    var pixels = [UInt8](repeating: 0, count: 128 * 128 * 4)
                    pixels.withUnsafeMutableBytes { raw in
                        let context = CGContext(data: raw.baseAddress, width: 128, height: 128, bitsPerComponent: 8,
                            bytesPerRow: 512, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
                        if flags == 3 {
                            context.setAllowsFontSubpixelPositioning(true); context.setShouldSubpixelPositionFonts(true)
                            context.setAllowsFontSubpixelQuantization(false); context.setShouldSubpixelQuantizeFonts(false)
                        }
                        context.translateBy(x: 16 + phase, y: 16 - phase)
                        context.scaleBy(x: anchorScale, y: anchorScale); context.textPosition = .zero
                        CTLineDraw(anchorReference, context)
                    }
                    var mass = 0.0, xMoment = 0.0, yMoment = 0.0
                    for y in 0..<128 { for x in 0..<128 {
                        let alpha = Double(pixels[(y * 128 + x) * 4 + 3])
                        mass += alpha; xMoment += Double(x) * alpha; yMoment += Double(y) * alpha
                    } }
                    FileHandle.standardError.write(Data("METAL_ANCHOR scale=\(anchorScale) flags=\(flags) phase=\(phase) x=\(xMoment / mass) y=\(yMoment / mass) mass=\(mass)\n".utf8))
                }
            }
            }
            }
            let (sample, reference) = prepared("FgjRFgjRFgjRFgjR", font: .monospacedSystemFont(ofSize: 13, weight: .regular), color: keyword)
            let worldX = diagnosticMode ? Double(ProcessInfo.processInfo.environment["FCB_METAL_DIAGNOSTIC_WORLD_X"] ?? "0")! : 0
            precondition(worldX.isFinite && abs(worldX) <= 1_000_000_000)
            let localSourceRect = CGRect(x: -10, y: -30, width: 600, height: 60)
            let sourceRect = localSourceRect.offsetBy(dx: worldX, dy: 0)
            let preparedReference = !diagnosticMode || ProcessInfo.processInfo.environment["FCB_METAL_GLYPH_PREPARED_REFERENCE"] == "1"
            let fontFlags = diagnosticMode ? Int(ProcessInfo.processInfo.environment["FCB_METAL_GLYPH_DIAGNOSTIC_FONT_FLAGS"] ?? "") : nil
            let density = diagnosticMode ? (Double(ProcessInfo.processInfo.environment["FCB_METAL_GLYPH_DIAGNOSTIC_DENSITY"] ?? "8") ?? 8) : 8
            let rgbPhases = diagnosticMode && ProcessInfo.processInfo.environment["FCB_METAL_GLYPH_RGB_PHASES"] == "1"
            let sampleScene = try AtlasMetalGlyphRenderer(device: device,
                tiles: [.init(id: 0, rect: sourceRect, sourceScale: 1,
                    lines: [.init(text: sample, origin: CGPoint(x: 10, y: 30), clip: sourceRect)])], pixelsPerPoint: density,
                minimumPixelsPerPoint: diagnosticMode ? 0 : 4, maskRepresentation: rgbPhases ? .opaqueRGBPhases : .grayscale)
            precondition(sampleScene.batches[0] != nil)
            let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .bgra8Unorm, width: 512, height: 256, mipmapped: false)
            descriptor.storageMode = .shared; descriptor.usage = .renderTarget
            let target = device.makeTexture(descriptor: descriptor)!
            var scales: [Double] = diagnosticMode
                ? [0.2, 0.4, 0.6, 0.8, 1, 1.5, 1.75, 2, 4, 6, 7, 7.5, 8].filter { $0 <= density }
                : [4, 4.5, 5, 5.5, 6, 6.5, 7, 7.5, 8]
            if diagnosticMode, let raw = ProcessInfo.processInfo.environment["FCB_METAL_DIAGNOSTIC_SCALES"] {
                scales = raw.split(separator: ",").map { Double($0)! }
                precondition(!scales.isEmpty && scales.count <= 16 && scales.allSatisfy { $0.isFinite && $0 > 0 && $0 <= density })
            }
            if !diagnosticMode {
                let batch = sampleScene.batches[0]!
                precondition(batch.supportsDisplayDensity(4) && batch.supportsDisplayDensity(8))
                precondition(!batch.supportsDisplayDensity(3.99) && !batch.supportsDisplayDensity(8.01)
                             && !batch.supportsDisplayDensity(.nan))
                let rejected = queue.makeCommandBuffer()!
                do {
                    try sampleScene.encode(into: target, commandBuffer: rejected, scale: 3.99, offset: .zero)
                    fatalError("Production admission must refuse unqualified small text before drawing")
                } catch AtlasMetalGlyphRenderer.Fallback.unqualifiedDensity { }
            }
            let nativeArtifacts = diagnosticMode && ProcessInfo.processInfo.environment["FCB_METAL_NATIVE_ARTIFACTS"] == "1"
            var phases = nativeArtifacts ? [0.0, 0.25, 1.0 / 3, 0.5, 2.0 / 3, 0.75] : [0.0, 0.25, 0.5, 0.75]
            if diagnosticMode && ProcessInfo.processInfo.environment["FCB_METAL_PHASE_BOUNDARIES"] == "1" {
                phases = phases.flatMap { phase in [(16 + phase).nextDown - 16, phase, (16 + phase).nextUp - 16] }
            }
            for phase in phases {
            for scale in scales {
                let cameraX = 16 + phase - worldX * scale
                let command = queue.makeCommandBuffer()!
                try sampleScene.encode(into: target, commandBuffer: command, scale: scale, offset: CGPoint(x: cameraX, y: 128 + phase))
                command.commit(); command.waitUntilCompleted()
                precondition(command.status == .completed && command.error == nil)
                var actual = [UInt8](repeating: 0, count: 512 * 256 * 4)
                actual.withUnsafeMutableBytes { target.getBytes($0.baseAddress!, bytesPerRow: 512 * 4,
                    from: MTLRegionMake2D(0, 0, 512, 256), mipmapLevel: 0) }
                var expected = [UInt8](repeating: 0, count: actual.count)
                expected.withUnsafeMutableBytes { raw in
                    let bitmap = CGContext(data: raw.baseAddress, width: 512, height: 256, bitsPerComponent: 8,
                        bytesPerRow: 512 * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
                    if let flags = fontFlags {
                        bitmap.setAllowsFontSubpixelPositioning(flags & 1 != 0)
                        bitmap.setShouldSubpixelPositionFonts(flags & 2 != 0)
                        bitmap.setAllowsFontSubpixelQuantization(flags & 4 != 0)
                        bitmap.setShouldSubpixelQuantizeFonts(flags & 8 != 0)
                    }
                    bitmap.translateBy(x: cameraX, y: 128 - phase); bitmap.scaleBy(x: scale, y: scale)
                    bitmap.translateBy(x: worldX, y: 0)
                    bitmap.setFillColor(CGColor(srgbRed: 22.0 / 255, green: 26.0 / 255, blue: 29.0 / 255, alpha: 1))
                    bitmap.fill(localSourceRect); bitmap.clip(to: localSourceRect)
                    bitmap.textPosition = .zero
                    if preparedReference {
                        bitmap.scaleBy(x: 1, y: -1)
                        sample.draw(in: bitmap, origin: .zero)
                    } else { CTLineDraw(reference, bitmap) }
                }
                var error = 0, energy = 0, actualEnergy = 0, compared = 0
                for pixel in 0..<(512 * 256) where actual[pixel * 4 + 3] == 255 && expected[pixel * 4 + 3] == 255 {
                    compared += 1
                    for channel in 0..<3 {
                        let a = Int(actual[pixel * 4 + 2 - channel]), e = Int(expected[pixel * 4 + channel])
                        error += abs(a - e); energy += abs(e - [22, 26, 29][channel]); actualEnergy += abs(a - [22, 26, 29][channel])
                    }
                }
                if nativeArtifacts && scale == density {
                    let (white, _) = prepared("FgjRFgjRFgjRFgjR", font: .monospacedSystemFont(ofSize: 13, weight: .regular), color: .white)
                    var whitePixels = [UInt8](repeating: 0, count: actual.count)
                    whitePixels.withUnsafeMutableBytes { raw in
                        let bitmap = CGContext(data: raw.baseAddress, width: 512, height: 256, bitsPerComponent: 8,
                            bytesPerRow: 512 * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
                        bitmap.translateBy(x: cameraX, y: 128 - phase); bitmap.scaleBy(x: scale, y: scale)
                        bitmap.translateBy(x: worldX, y: 0)
                        bitmap.clip(to: localSourceRect); bitmap.scaleBy(x: 1, y: -1)
                        white.draw(in: bitmap, origin: .zero)
                    }
                    var composed = expected, compositionError = 0, compositionEnergy = 0
                    for pixel in 0..<(512 * 256) where expected[pixel * 4 + 3] == 255 {
                        let alpha = Double(whitePixels[pixel * 4 + 3]) / 255
                        for channel in 0..<3 {
                            let background = [22, 26, 29][channel], foreground = [249, 38, 114][channel]
                            let value = Int((Double(background) + Double(foreground - background) * alpha).rounded())
                            composed[pixel * 4 + channel] = UInt8(value)
                            compositionError += abs(value - Int(expected[pixel * 4 + channel]))
                            compositionEnergy += abs(value - background)
                        }
                    }
                    let directory = FileManager.default.temporaryDirectory.appendingPathComponent("fcb-metal-native-\(UUID())", isDirectory: true)
                    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
                    try Data(actual).write(to: directory.appendingPathComponent("gpu.bgra"))
                    try Data(expected).write(to: directory.appendingPathComponent("coretext.rgba"))
                    try Data(whitePixels).write(to: directory.appendingPathComponent("white-alpha.rgba"))
                    try Data(composed).write(to: directory.appendingPathComponent("white-composited.rgba"))
                    let positions = (0...16).map { cameraX + worldX * scale + Double(CTLineGetOffsetForStringIndex(reference, $0, nil)) * scale }
                    let metadata: [String: Any] = ["width": 512, "height": 256, "density": density, "phase": phase, "world_x": worldX, "camera_x": cameraX,
                        "text": "FgjRFgjRFgjRFgjR", "glyph_cell_x": positions,
                        "white_composition_rgb_error": Double(compositionError) / Double(max(1, energy)),
                        "white_composition_ink_ratio": Double(compositionEnergy) / Double(max(1, energy))]
                    try JSONSerialization.data(withJSONObject: metadata, options: [.sortedKeys]).write(to: directory.appendingPathComponent("metadata.json"))
                    FileHandle.standardError.write(Data("METAL_NATIVE_ARTIFACT path=\(directory.path) density=\(density) phase=\(phase) white_rgb_error=\(Double(compositionError) / Double(max(1, energy))) white_ink_ratio=\(Double(compositionEnergy) / Double(max(1, energy)))\n".utf8))
                }
                if !diagnosticMode {
                    precondition(energy > 0 && compared > 0 && Double(error) / Double(energy) <= 0.12,
                                 "Qualified display densities match actual prepared CoreText drawing at every camera phase")
                }
                let cells = (0..<16).map { glyph -> Double in
                    let lo = cameraX + worldX * scale + Double(CTLineGetOffsetForStringIndex(reference, glyph, nil)) * scale
                    let hi = cameraX + worldX * scale + Double(CTLineGetOffsetForStringIndex(reference, glyph + 1, nil)) * scale
                    var cellError = 0, cellEnergy = 0
                    for y in 0..<256 { for x in max(0, min(512, Int(floor(lo))))..<max(0, min(512, Int(ceil(hi)))) {
                        let pixel = (y * 512 + x) * 4
                        if actual[pixel + 3] != 255 || expected[pixel + 3] != 255 { continue }
                        for channel in 0..<3 {
                            cellError += abs(Int(actual[pixel + 2 - channel]) - Int(expected[pixel + channel]))
                            cellEnergy += abs(Int(expected[pixel + channel]) - [22, 26, 29][channel])
                        }
                    } }
                    return Double(cellError) / Double(max(1, cellEnergy))
                }
                FileHandle.standardError.write(Data("METAL_DIRECT_SCALE world_x=\(worldX) camera_x=\(cameraX) max_cell_rgb_error=\(cells.max()!) rgb_phases=\(rgbPhases) prepared_reference=\(preparedReference) diagnostic_font_flags=\(fontFlags.map(String.init) ?? "default") prepared_density=\(density) phase=\(phase) physical_scale=\(scale) font_pixels=\(13 * scale) rgb_error=\(Double(error) / Double(max(1, energy))) ink_ratio=\(Double(actualEnergy) / Double(max(1, energy))) compared_pixels=\(compared)\n".utf8))
            }
            }
        }
        if ProcessInfo.processInfo.environment["FCB_METAL_FIXED_CAPTURE"] == "1" {
            // Independent retained-image contract: CoreText draws the entire source
            // once. A scalar pixel-area integrator transforms that bitmap; it does
            // not consult glyph positions, phase banks, or renderer prefix sums.
            let density = 4.0, sourceWidth = 800, sourceHeight = 240
            let width = sourceWidth * 4, height = sourceHeight * 4
            let rect = CGRect(x: -17.25, y: 9.5, width: Double(sourceWidth), height: Double(sourceHeight))
            let colors = [NSColor(srgbRed: 249.0 / 255, green: 38.0 / 255, blue: 114.0 / 255, alpha: 1),
                          NSColor(srgbRed: 166.0 / 255, green: 226.0 / 255, blue: 46.0 / 255, alpha: 1)]
            var lines: [AtlasMetalGlyphRenderer.Line] = []
            for row in 0..<12 {
                let (text, _) = prepared(String(repeating: "FgjR", count: 24),
                    font: .monospacedSystemFont(ofSize: 13, weight: .regular), color: colors[row % 2])
                lines.append(.init(text: text, origin: CGPoint(x: 2.375, y: Double(19 + row * 18)),
                    clip: CGRect(x: rect.minX + 4, y: rect.minY + 3, width: 770, height: 224)))
            }
            var capture = [UInt8](repeating: 0, count: width * height * 4)
            capture.withUnsafeMutableBytes { raw in
                let context = CGContext(data: raw.baseAddress, width: width, height: height, bitsPerComponent: 8,
                    bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
                context.setFillColor(CGColor(srgbRed: 22.0 / 255, green: 26.0 / 255, blue: 29.0 / 255, alpha: 1))
                context.fill(CGRect(x: 0, y: 0, width: width, height: height))
                context.translateBy(x: 0, y: CGFloat(height)); context.scaleBy(x: density, y: -density)
                for line in lines {
                    context.saveGState()
                    context.clip(to: line.clip.offsetBy(dx: -rect.minX, dy: -rect.minY))
                    line.text.draw(in: context, origin: line.origin)
                    context.restoreGState()
                }
            }
            let scalarCapture = ProcessInfo.processInfo.environment["FCB_CAPTURE_SCALAR"] == "1"
            let scene = try AtlasMetalGlyphRenderer(device: device,
                tiles: [.init(id: 0, rect: rect, sourceScale: 1, lines: lines)], pixelsPerPoint: density,
                minimumPixelsPerPoint: scalarCapture ? AtlasMetalGlyphRenderer.tileMemoryMinimumDensity : 0,
                maskRepresentation: scalarCapture ? .grayscale : .opaqueRGBPhases, maskPlacement: .captureGrid,
                tileMemoryAccumulation: scalarCapture)
            precondition(scene.batches[0] != nil && scene.fallbackTiles.isEmpty,
                         "Dense fixed-source fixture must exercise real GPU glyphs")
            if scalarCapture {
                precondition(scene.batches[0]!.supportsDisplayDensity(0.04))
                precondition(!scene.batches[0]!.supportsDisplayDensity(0.039),
                    "Do not admit unqualified smaller text")
            }
            let retainedIdentity = scene.retainedInstanceIdentity
            let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .bgra8Unorm,
                width: 512, height: 256, mipmapped: false)
            descriptor.storageMode = .shared; descriptor.usage = .renderTarget
            let target = device.makeTexture(descriptor: descriptor)!
            var failures = 0
            var flightGoldens: [Double: [UInt8]] = [:]
            for scale in (scalarCapture ? [4.0, 2, 1, 0.5, 0.2, 0.1, 0.08, 0.06, 0.04] : [4.0, 2, 1, 0.5, 0.2, 0.1]) {
            for phase in [0.0, 0.25, 0.5, 0.75] {
                let offset = CGPoint(x: 12 + phase - rect.minX * scale, y: 8 + phase - rect.minY * scale)
                let command = queue.makeCommandBuffer()!
                try scene.encode(into: target, commandBuffer: command, scale: scale, offset: offset)
                command.commit(); command.waitUntilCompleted()
                precondition(command.status == .completed && command.error == nil)
                precondition(scene.retainedInstanceIdentity == retainedIdentity)
                var actual = [UInt8](repeating: 0, count: 512 * 256 * 4)
                actual.withUnsafeMutableBytes { target.getBytes($0.baseAddress!, bytesPerRow: 512 * 4,
                    from: MTLRegionMake2D(0, 0, 512, 256), mipmapLevel: 0) }
                if scalarCapture && phase == 0.25 && (scale == 0.04 || scale == 0.08) {
                    flightGoldens[scale] = actual
                }
                var expected = [UInt8](repeating: 0, count: actual.count)
                var error = 0.0, energy = 0.0, actualEnergy = 0.0
                var rowErrors = [Double](repeating: 0, count: 12), rowEnergies = rowErrors
                for y in 0..<256 { for x in 0..<512 {
                    let x0 = (Double(x) - (12 + phase)) * density / scale
                    let y0 = (Double(y) - (8 + phase)) * density / scale
                    let x1 = x0 + density / scale, y1 = y0 + density / scale
                    guard x0 >= 0, y0 >= 0, x1 <= Double(width), y1 <= Double(height) else { continue }
                    let pixel = (y * 512 + x) * 4
                    precondition(actual[pixel + 3] == 255, "Every interior source pixel remains opaque")
                    var sums = [Double](repeating: 0, count: 3)
                    for sy in Int(floor(y0))..<Int(ceil(y1)) {
                        let wy = min(y1, Double(sy + 1)) - max(y0, Double(sy))
                        for sx in Int(floor(x0))..<Int(ceil(x1)) {
                            let area = wy * (min(x1, Double(sx + 1)) - max(x0, Double(sx)))
                            for channel in 0..<3 { sums[channel] += Double(capture[(sy * width + sx) * 4 + channel]) * area }
                        }
                    }
                    let row = max(0, min(11, Int(((y0 + y1) / (2 * density) - 1) / 18)))
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
                let perRow = (0..<12).filter { rowEnergies[$0] > 100 }.map { rowErrors[$0] / rowEnergies[$0] }
                let maximum = perRow.max()!
                FileHandle.standardError.write(Data("METAL_FIXED_CAPTURE scalar=\(scalarCapture) density=4 scale=\(scale) phase=\(phase) rgb_error=\(error / energy) ink_ratio=\(actualEnergy / energy) max_row_error=\(maximum) instances=\(scene.instanceCount) bytes=\(scene.managedBytes)\n".utf8))
                if error / energy > 0.12 || maximum > 0.12 {
                    failures += 1
                    let directory = FileManager.default.temporaryDirectory.appendingPathComponent("fcb-metal-fixed-\(UUID())")
                    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
                    try Data(actual).write(to: directory.appendingPathComponent("gpu.bgra"))
                    try Data(expected).write(to: directory.appendingPathComponent("box-reference.rgba"))
                    try Data(capture).write(to: directory.appendingPathComponent("capture-3200x960.rgba"))
                    FileHandle.standardError.write(Data("METAL_FIXED_FAILURE scale=\(scale) phase=\(phase) path=\(directory.path)\n".utf8))
                }
            }
            }
            precondition(failures == 0, "Fixed-source GPU must preserve complete captured CoreText under every tested camera transform")
            if scalarCapture {
                // Encode both frames before either executes. Separate queues
                // exercise attachment lifetimes without serializing on the CPU.
                var queued: [(Double, MTLTexture, MTLCommandBuffer)] = []
                for scale in [0.04, 0.08] {
                    let texture = device.makeTexture(descriptor: descriptor)!
                    let independentQueue = device.makeCommandQueue()!
                    let command = independentQueue.makeCommandBuffer()!
                    let offset = CGPoint(x: 12.25 - rect.minX * scale, y: 8.25 - rect.minY * scale)
                    try scene.encode(into: texture, commandBuffer: command, scale: scale, offset: offset)
                    queued.append((scale, texture, command))
                }
                for (_, _, command) in queued { command.commit() }
                for (scale, texture, command) in queued {
                    command.waitUntilCompleted()
                    precondition(command.status == .completed && command.error == nil)
                    var pixels = [UInt8](repeating: 0, count: 512 * 256 * 4)
                    pixels.withUnsafeMutableBytes { texture.getBytes($0.baseAddress!, bytesPerRow: 512 * 4,
                        from: MTLRegionMake2D(0, 0, 512, 256), mipmapLevel: 0) }
                    precondition(pixels == flightGoldens[scale],
                        "Concurrent small-text frames retain independent tile-memory attachments")
                }
            }
        }
        if ProcessInfo.processInfo.environment["FCB_METAL_GLYPH_PROFILE"] == "1" {
            // A fully visible source grid, not a huge offscreen line: every
            // instance participates in the raster workload at both camera extremes.
            let rowText = String(String(repeating: "FgjR", count: 63).prefix(250))
            let (row, _) = prepared(rowText, font: .monospacedSystemFont(ofSize: 8, weight: .regular), color: .white)
            let sceneClip = CGRect(x: 0, y: 0, width: 1280, height: 2048)
            let rows = (0..<200).map { index in
                AtlasMetalGlyphRenderer.Line(text: row, origin: CGPoint(x: 20, y: 20 + index * 10), clip: sceneClip)
            }
            let start = ProcessInfo.processInfo.systemUptime
            let grid = try AtlasMetalGlyphRenderer(device: device, lines: rows, pixelsPerPoint: 1)
            let preparationMS = (ProcessInfo.processInfo.systemUptime - start) * 1000
            precondition(grid.instanceCount == 50_000)
            let retained = grid.retainedInstanceIdentity
            let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .bgra8Unorm,
                width: 1280, height: 2048, mipmapped: false)
            descriptor.storageMode = .shared; descriptor.usage = .renderTarget
            let target = device.makeTexture(descriptor: descriptor)!
            var cpu: [Double] = [], gpu: [Double] = [], visiblePixels: [Int] = []
            var firstGPUStart = 0.0, lastGPUEnd = 0.0
            for frame in 0..<120 {
                let scale = 0.92 + Double(frame) / 119 * 0.08
                let offset = CGPoint(x: 8 + sin(Double(frame) * 0.1) * 4, y: 8)
                let command = queue.makeCommandBuffer()!
                let before = ProcessInfo.processInfo.systemUptime
                try grid.encode(into: target, commandBuffer: command, scale: scale, offset: offset)
                cpu.append((ProcessInfo.processInfo.systemUptime - before) * 1000)
                command.commit(); command.waitUntilCompleted() // Test-only serial qualification.
                precondition(command.status == .completed && command.error == nil)
                precondition(command.gpuStartTime > 0 && command.gpuEndTime > command.gpuStartTime,
                             "Actual GPU timestamps, not CPU submission estimates")
                if frame == 0 { firstGPUStart = command.gpuStartTime }
                lastGPUEnd = command.gpuEndTime
                gpu.append((command.gpuEndTime - command.gpuStartTime) * 1000)
                precondition(grid.retainedInstanceIdentity == retained, "All 120 frames reuse identical GPU instances")
                if frame == 0 || frame == 119 {
                    var pixels = [UInt8](repeating: 0, count: 1280 * 2048 * 4)
                    pixels.withUnsafeMutableBytes { target.getBytes($0.baseAddress!, bytesPerRow: 1280 * 4,
                        from: MTLRegionMake2D(0, 0, 1280, 2048), mipmapLevel: 0) }
                    let count = stride(from: 3, to: pixels.count, by: 4).reduce(0) { $0 + (pixels[$1] > 0 ? 1 : 0) }
                    precondition(count > 100_000, "Large glyph grid must actually cover the output")
                    // Each source row must survive clipping and camera projection.
                    for rowIndex in 0..<200 {
                        let baseline = (Double(20 + rowIndex * 10) * scale + offset.y)
                        let lower = max(0, Int(baseline - 8)), upper = min(2048, Int(baseline + 3))
                        let ink = (lower..<upper).contains { y in
                            (0..<1280).contains { x in pixels[(y * 1280 + x) * 4 + 3] > 0 }
                        }
                        precondition(ink, "Every prepared source row is visible")
                    }
                    visiblePixels.append(count)
                }
            }
            func p95(_ values: [Double]) -> Double { values.sorted()[113] }
            let report: [String: Any] = [
                "phase": "offscreen-metal-glyph-profile", "device": device.name,
                "registry_id": String(device.registryID), "unified_memory": device.hasUnifiedMemory,
                "glyph_instances": grid.instanceCount, "distinct_masks": grid.distinctGlyphCount,
                "rows": 200, "columns": 250, "viewport": [1280, 2048], "frames": 120,
                "prepared_density": 1, "camera_scale_range": [0.92, 1.0],
                "prepare_cpu_ms": preparationMS, "encode_cpu_p95_ms": p95(cpu),
                "encode_cpu_max_ms": cpu.max()!, "gpu_p95_ms": p95(gpu), "gpu_max_ms": gpu.max()!,
                "first_gpu_start": firstGPUStart, "last_gpu_end": lastGPUEnd,
                "managed_gpu_bytes": grid.managedBytes, "first_last_covered_pixels": visiblePixels,
                "instance_buffer_reused": true, "serial_test_waits": true
            ]
            let data = try JSONSerialization.data(withJSONObject: report, options: [.sortedKeys])
            FileHandle.standardOutput.write(data + Data("\n".utf8))
        }
        print("AtlasMetalGlyphTests: real Metal masks, fonts, colors, clipping, camera reuse and explicit fallbacks passed")
    }
}
