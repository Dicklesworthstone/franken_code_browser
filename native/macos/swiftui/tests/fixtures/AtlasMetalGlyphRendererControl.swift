// Frozen six-vertex renderer control from local native history commit 7589f2b.
import AppKit
import CoreText
import Metal

/// Retained glyph scene. The caller owns presentation and command completion.
/// No source parsing, shaping, GPU wait or readback occurs in encode().
@MainActor final class AtlasMetalGlyphRendererControl {
    struct Line {
        let text: AtlasPreparedLine
        let origin: CGPoint
        let clip: CGRect
    }
    struct SceneTile {
        let id: Int
        let rect: CGRect
        let sourceScale: Double
        let lines: [Line]
    }
    struct Batch {
        let rect: CGRect
        let instanceRange: Range<Int>
        let pixelsPerWorldPoint: Double
        let minimumPixelsPerWorldPoint: Double
        func supportsDisplayDensity(_ density: Double) -> Bool {
            density.isFinite && density > 0 && density >= minimumPixelsPerWorldPoint && density <= pixelsPerWorldPoint
        }
    }
    let batches: [Int: Batch]
    let fallbackTiles: [Int: Fallback]
    enum Fallback: Error, Equatable {
        case unsupportedFontOrTransform, unsupportedColor, overlappingGlyphs, invalidGeometry, resourceLimit
        case unavailableDevice, allocation, incompatibleTarget, undersampled, unqualifiedDensity
        case pipeline(String)
    }
    enum MaskRepresentation { case grayscale, opaqueRGBPhases }
    enum MaskPlacement { case displayGrid, captureGrid }
    private struct GlyphKey: Hashable { let font: Int; let glyph: CGGlyph; let style: Int }
    private struct Instance {
        var rect: SIMD4<Float>
        var uv: SIMD4<Float>
        var color: SIMD4<Float>
        var clip: SIMD4<Float>
        var baseline: SIMD4<Float>
    }
    private struct Uniforms { var camera: SIMD4<Float>; var viewport: SIMD4<Float> }
    private struct Mask { let rect: CGRect; let inkRect: CGRect; let uv: SIMD4<Float> }
    static let maximumInstances = 8_000_000
    static let atlasSide = 128
    static let maximumGlyphMasks = 2048
    static let maskBytes = 128 * 128 * MemoryLayout<Float>.stride // Inclusive coverage prefix sums
    static let maximumManagedBytes = 2 * 1024 * 1024 * 1024
    let instanceCount: Int
    let distinctGlyphCount: Int
    let managedBytes: Int
    let admittedMaskCapacity: Int
    let pixelsPerPoint: Double
    private let device: MTLDevice
    private let atlas: MTLTexture
    private let instances: MTLBuffer
    private let phaseLookup: MTLBuffer
    var retainedInstanceIdentity: ObjectIdentifier { ObjectIdentifier(instances as AnyObject) }
    private let pipeline: MTLRenderPipelineState
    private let additivePipeline: MTLRenderPipelineState
    private let opaqueBackgrounds: Bool

    convenience init(device: MTLDevice, lines: [Line], pixelsPerPoint: Double) throws {
        try self.init(device: device, tiles: [SceneTile(id: 0, rect: .zero, sourceScale: 1, lines: lines)],
                      pixelsPerPoint: pixelsPerPoint, previousManagedBytes: 0, opaqueBackgrounds: false)
        if let failure = fallbackTiles[0] { throw failure }
    }

    init(device: MTLDevice, tiles: [SceneTile], pixelsPerPoint: Double,
         previousManagedBytes: Int = 0, opaqueBackgrounds: Bool = true, minimumPixelsPerPoint: Double = 0,
         maskRepresentation: MaskRepresentation = .grayscale,
         maskPlacement: MaskPlacement = .displayGrid) throws {
        let rgbPhases = maskRepresentation == .opaqueRGBPhases
        let captureGrid = maskPlacement == .captureGrid
        guard !rgbPhases || opaqueBackgrounds else { throw Fallback.unsupportedColor }
        guard device.hasUnifiedMemory else { throw Fallback.unavailableDevice }
        guard pixelsPerPoint.isFinite, pixelsPerPoint >= 1, pixelsPerPoint <= 32,
              minimumPixelsPerPoint.isFinite, minimumPixelsPerPoint >= 0, minimumPixelsPerPoint <= pixelsPerPoint else {
            throw Fallback.invalidGeometry
        }
        guard tiles.count <= 100_000, previousManagedBytes >= 0,
              previousManagedBytes <= Self.maximumManagedBytes else { throw Fallback.resourceLimit }
        var planned = 0
        for tile in tiles {
            planned = min(Self.maximumInstances, planned + 1)
            for line in tile.lines { for run in line.text.runs {
                planned += min(Self.maximumInstances - planned, run.glyphs.count)
            } }
        }
        let channels = rgbPhases ? 4 : 1
        let phaseTableLimit = rgbPhases ? Self.maximumGlyphMasks * 24 : 1
        let fixedReservation = 2 * (phaseTableLimit * MemoryLayout<UInt32>.stride
            + max(1, planned) * MemoryLayout<Instance>.stride)
            + previousManagedBytes + 64 * 1024 * 1024
        // Reserve instance uploads, phase lookup and metadata before allocating masks.
        // RGB banks may use the remaining capacity without reserving every possible slice.
        let maskCapacity: Int
        if rgbPhases {
            guard fixedReservation <= Self.maximumManagedBytes else { throw Fallback.resourceLimit }
            maskCapacity = min(Self.maximumGlyphMasks,
                (Self.maximumManagedBytes - fixedReservation) / (2 * Self.maskBytes * channels))
            guard maskCapacity >= 1 else { throw Fallback.resourceLimit }
        } else {
            guard fixedReservation + 2 * Self.maximumGlyphMasks * Self.maskBytes <= Self.maximumManagedBytes else {
                throw Fallback.resourceLimit
            }
            maskCapacity = Self.maximumGlyphMasks
        }
        admittedMaskCapacity = maskCapacity
        self.device = device; self.pixelsPerPoint = pixelsPerPoint
        self.opaqueBackgrounds = opaqueBackgrounds
        let side = Self.atlasSide
        var layers: [[Float]] = []
        var styles: [CGColor] = []
        var phaseIndices: [UInt32] = []
        var fonts: [CTFont] = [], masks: [GlyphKey: Mask] = [:], packed: [Instance] = []
        packed.reserveCapacity(planned)
        var accepted: [Int: Batch] = [:], refused: [Int: Fallback] = [:]
        var admitted = 0
        let space = CGColorSpace(name: CGColorSpace.sRGB)!
        func valid(_ value: CGFloat) -> Bool { value.isFinite && abs(value) <= 10_000_000 }
        for tile in tiles {
            guard accepted[tile.id] == nil && refused[tile.id] == nil else { throw Fallback.invalidGeometry }
            let firstInstance = packed.count, firstAdmitted = admitted
            let firstFont = fonts.count, firstLayer = layers.count
            let firstStyle = styles.count, firstPhase = phaseIndices.count
            var addedKeys: [GlyphKey] = []
            do {
                guard tile.sourceScale.isFinite, tile.sourceScale > 0,
                      [tile.rect.minX, tile.rect.minY, tile.rect.maxX, tile.rect.maxY].allSatisfy(valid),
                      !opaqueBackgrounds || (tile.rect.width > 0 && tile.rect.height > 0) else { throw Fallback.invalidGeometry }
                if opaqueBackgrounds {
                    guard admitted < Self.maximumInstances else { throw Fallback.resourceLimit }
                    admitted += 1
                    packed.append(Instance(rect: SIMD4(Float(tile.rect.minX), Float(tile.rect.minY), Float(tile.rect.width), Float(tile.rect.height)),
                        uv: SIMD4<Float>(0, captureGrid ? (rgbPhases ? -1 : -2) : 0, -1, 0), color: SIMD4<Float>(22.0 / 255, 26.0 / 255, 29.0 / 255, 1),
                        clip: SIMD4(Float(tile.rect.minX), Float(tile.rect.minY), Float(tile.rect.maxX), Float(tile.rect.maxY)),
                        baseline: captureGrid ? SIMD4<Float>(0, 0,
                            Float(tile.rect.minX - Double(Float(tile.rect.minX))),
                            Float(tile.rect.minY - Double(Float(tile.rect.minY)))) : .zero))
                }
        var previousLineBottom = -Double.infinity
        for line in tile.lines {
            var inkRects: [CGRect] = []
            guard valid(line.origin.x), valid(line.origin.y), !line.clip.isNull,
                  [line.clip.minX, line.clip.minY, line.clip.maxX, line.clip.maxY].allSatisfy(valid),
                  line.clip.width > 0, line.clip.height > 0 else { throw Fallback.invalidGeometry }
            guard line.text.retainedColorLine == nil else { throw Fallback.unsupportedFontOrTransform }
            for run in line.text.runs {
                guard run.matrix.isIdentity, CTFontGetMatrix(run.font).isIdentity,
                      !CTFontGetSymbolicTraits(run.font).contains(.traitColorGlyphs) else {
                    throw Fallback.unsupportedFontOrTransform
                }
                guard run.glyphs.count == run.positions.count,
                      run.glyphs.count <= Self.maximumInstances - admitted else { throw Fallback.resourceLimit }
                admitted += run.glyphs.count
                guard let color = run.color.converted(to: space, intent: .defaultIntent, options: nil),
                      let c = color.components, c.count == 4, c.allSatisfy({ $0.isFinite && $0 >= 0 && $0 <= 1 }) else {
                    throw Fallback.unsupportedFontOrTransform
                }
                // UNorm blending cannot retain a negative color contribution.
                // Darker-than-background colors use the normal CPU compositor.
                if opaqueBackgrounds && (c[0] < 22.0 / 255 || c[1] < 26.0 / 255 || c[2] < 29.0 / 255) {
                    throw Fallback.unsupportedColor
                }
                let fontIndex: Int
                if let existing = fonts.firstIndex(where: { CFEqual($0, run.font) }) { fontIndex = existing }
                else {
                    guard fonts.count < 64 else { throw Fallback.resourceLimit }
                    fontIndex = fonts.count; fonts.append(run.font)
                }
                let styleIndex: Int
                if !rgbPhases { styleIndex = 0 }
                else if let existing = styles.firstIndex(where: { CFEqual($0, run.color) }) { styleIndex = existing }
                else {
                    guard styles.count < Self.maximumGlyphMasks else { throw Fallback.resourceLimit }
                    styleIndex = styles.count; styles.append(run.color)
                }
                for (glyph, position) in zip(run.glyphs, run.positions) {
                    guard valid(position.x), valid(position.y) else { throw Fallback.invalidGeometry }
                    let key = GlyphKey(font: fontIndex, glyph: glyph, style: styleIndex)
                    let mask: Mask
                    if let cached = masks[key] { mask = cached }
                    else {
                        var g = glyph
                        let bounds = CTFontGetBoundingRectsForGlyphs(run.font, .default, &g, nil, 1)
                        guard !bounds.isNull, [bounds.minX, bounds.minY, bounds.maxX, bounds.maxY].allSatisfy(valid) else {
                            throw Fallback.invalidGeometry
                        }
                        if bounds.width == 0 || bounds.height == 0 {
                            masks[key] = Mask(rect: .zero, inkRect: .zero, uv: .zero); addedKeys.append(key); continue
                        }
                        let minX = floor(bounds.minX * pixelsPerPoint) - 1
                        let minY = floor(bounds.minY * pixelsPerPoint) - 1
                        let width = Int(ceil(bounds.maxX * pixelsPerPoint) - minX + (rgbPhases ? 2 : 1))
                        let height = Int(ceil(bounds.maxY * pixelsPerPoint) - minY + 1)
                        guard width > 0, height > 0, width <= side - 2, height <= side - 2,
                              layers.count < maskCapacity else { throw Fallback.resourceLimit }
                        let phaseStart = phaseIndices.count
                        guard !rgbPhases || phaseStart <= phaseTableLimit - 24 else { throw Fallback.resourceLimit }
                        var phaseImages: [Data: UInt32] = [:]
                        for phase in 0..<(rgbPhases ? 24 : 1) {
                            var bytes = [UInt8](repeating: 0, count: width * height * 4)
                            var backgroundBytes: [UInt8] = [0, 0, 0]
                            try bytes.withUnsafeMutableBytes { raw in
                                guard let bitmap = CGContext(data: raw.baseAddress, width: width, height: height,
                                    bitsPerComponent: 8, bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
                                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { throw Fallback.allocation }
                                if rgbPhases {
                                    bitmap.setFillColor(CGColor(srgbRed: 22.0 / 255, green: 26.0 / 255, blue: 29.0 / 255, alpha: 1))
                                    bitmap.fill(CGRect(x: 0, y: 0, width: width, height: height))
                                    backgroundBytes = [raw[0], raw[1], raw[2]]
                                }
                                // The phase is captured into pixels, not added again to the quad.
                                bitmap.translateBy(x: -minX + Double(phase) / 24, y: -minY)
                                bitmap.scaleBy(x: pixelsPerPoint, y: pixelsPerPoint)
                                bitmap.setFillColor(rgbPhases ? run.color : CGColor(gray: 1, alpha: 1))
                                var origin = CGPoint.zero
                                CTFontDrawGlyphs(run.font, &g, &origin, 1, bitmap)
                            }
                            let imageKey = rgbPhases ? Data(bytes) : Data()
                            if rgbPhases, let existing = phaseImages[imageKey] {
                                phaseIndices.append(existing); continue
                            }
                            guard layers.count < maskCapacity else { throw Fallback.resourceLimit }
                            var base = [Float](repeating: 0, count: side * side * channels)
                            for row in 0..<height { for column in 0..<width {
                                let input = (row * width + column) * 4
                                let output = ((row + 1) * side + 1 + column) * channels
                                if rgbPhases {
                                    // Preserve the actual opaque RGB result, including color-dependent antialiasing.
                                    // A zero background channel in the original bitmap is not assumed.
                                    for channel in 0..<3 {
                                        base[output + channel] = Float(Int(bytes[input + channel]) - Int(backgroundBytes[channel]))
                                    }
                                } else { base[output] = Float(bytes[input + 3]) }
                            } }
                            var prefix = [Float](repeating: 0, count: base.count)
                            for row in 0..<side { for channel in 0..<channels {
                                var rowSum: Float = 0
                                for column in 0..<side {
                                    let index = (row * side + column) * channels + channel
                                    rowSum += base[index]
                                    prefix[index] = rowSum + (row == 0 ? 0 : prefix[index - side * channels])
                                }
                            } }
                            if rgbPhases {
                                let slice = UInt32(layers.count)
                                phaseImages[imageKey] = slice; phaseIndices.append(slice)
                            }
                            layers.append(prefix)
                        }
                        mask = Mask(rect: CGRect(x: minX / pixelsPerPoint,
                            y: -(minY + Double(height)) / pixelsPerPoint,
                            width: Double(width) / pixelsPerPoint, height: Double(height) / pixelsPerPoint),
                            inkRect: CGRect(x: bounds.minX, y: -bounds.maxY, width: bounds.width, height: bounds.height),
                            uv: SIMD4(Float(rgbPhases ? phaseIndices[phaseStart] : UInt32(layers.count - 1)),
                                      rgbPhases ? Float(phaseStart + 1) : 0,
                                      Float(width) / Float(side), Float(height) / Float(side)))
                        masks[key] = mask; addedKeys.append(key)
                    }
                    if mask.rect.isEmpty { continue }
                    var local = mask.rect.offsetBy(dx: line.origin.x + position.x, dy: line.origin.y - position.y)
                    var instanceUV = mask.uv
                    if captureGrid {
                        // Capture is anchored at the tile source origin, before world placement.
                        // Choose the sampled phase once; subsequent cameras only transform pixels.
                        // Sample k represents horizontal phase k/24, with floor selection.
                        let sourceX = (line.origin.x + position.x) * pixelsPerPoint
                        let sourceY = (line.origin.y - position.y) * pixelsPerPoint
                        if rgbPhases {
                            let phase = min(23, max(0, Int(floor((sourceX - floor(sourceX)) * 24))))
                            let lookup = Int(mask.uv.y) - 1 + phase
                            instanceUV.x = Float(phaseIndices[lookup])
                            instanceUV.y = -1
                        } else {
                            // A fixed grayscale capture has no phase bank. Native-density
                            // color/phase fidelity must be qualified separately (not assumed at 1x).
                            instanceUV.y = -2
                        }
                        local = local.offsetBy(dx: (floor(sourceX) - sourceX) / pixelsPerPoint,
                                               dy: (ceil(sourceY) - sourceY) / pixelsPerPoint)
                    }
                    let rect = CGRect(x: tile.rect.minX + local.minX * tile.sourceScale,
                        y: tile.rect.minY + local.minY * tile.sourceScale,
                        width: local.width * tile.sourceScale, height: local.height * tile.sourceScale)
                    guard [rect.minX, rect.minY, rect.maxX, rect.maxY].allSatisfy(valid) else { throw Fallback.invalidGeometry }
                    if opaqueBackgrounds {
                        let ink = mask.inkRect.offsetBy(dx: line.origin.x + position.x, dy: line.origin.y - position.y)
                        let worldInk = CGRect(x: tile.rect.minX + ink.minX * tile.sourceScale,
                            y: tile.rect.minY + ink.minY * tile.sourceScale,
                            width: ink.width * tile.sourceScale, height: ink.height * tile.sourceScale).intersection(line.clip)
                        if !worldInk.isNull && !worldInk.isEmpty { inkRects.append(worldInk) }
                    }
                    let baselineX = tile.rect.minX + (line.origin.x + position.x) * tile.sourceScale
                    let baselineY = tile.rect.minY + (line.origin.y - position.y) * tile.sourceScale
                    packed.append(Instance(rect: SIMD4(Float(rect.minX), Float(rect.minY), Float(rect.width), Float(rect.height)),
                        uv: instanceUV, color: SIMD4(Float(c[0]), Float(c[1]), Float(c[2]), Float(c[3])),
                        clip: SIMD4(Float(line.clip.minX), Float(line.clip.minY), Float(line.clip.maxX), Float(line.clip.maxY)),
                        baseline: SIMD4(Float(baselineX), Float(baselineY),
                            Float(captureGrid ? rect.minX - Double(Float(rect.minX)) : baselineX - Double(Float(baselineX))),
                            Float(captureGrid ? rect.minY - Double(Float(rect.minY)) : baselineY - Double(Float(baselineY))))))
                }
            }
            if opaqueBackgrounds && !inkRects.isEmpty {
                // Sum area-filtered ink only when the original glyph bounds are
                // disjoint. Combining marks/overhangs require the CPU union path.
                inkRects.sort { $0.minX < $1.minX }
                var right = -Double.infinity
                var lineTop = Double.infinity, lineBottom = -Double.infinity
                for ink in inkRects {
                    guard ink.minX >= right - 1e-8 else { throw Fallback.overlappingGlyphs }
                    right = ink.maxX; lineTop = min(lineTop, ink.minY); lineBottom = max(lineBottom, ink.maxY)
                }
                guard lineTop >= previousLineBottom - 1e-8 else { throw Fallback.overlappingGlyphs }
                previousLineBottom = lineBottom
            }
        }
                accepted[tile.id] = Batch(rect: tile.rect, instanceRange: firstInstance..<packed.count,
                                         pixelsPerWorldPoint: pixelsPerPoint / tile.sourceScale,
                                         minimumPixelsPerWorldPoint: minimumPixelsPerPoint / tile.sourceScale)
            } catch let failure as Fallback {
                packed.removeSubrange(firstInstance..<packed.count)
                admitted = firstAdmitted
                // Reclaim all per-tile coverage slices.
                for key in addedKeys { masks.removeValue(forKey: key) }
                layers.removeSubrange(firstLayer..<layers.count)
                fonts.removeSubrange(firstFont..<fonts.count)
                styles.removeSubrange(firstStyle..<styles.count)
                phaseIndices.removeSubrange(firstPhase..<phaseIndices.count)
                refused[tile.id] = failure
            }
        }
        batches = accepted; fallbackTiles = refused
        instanceCount = packed.count; distinctGlyphCount = masks.count
        let bufferBytes = max(1, packed.count) * MemoryLayout<Instance>.stride
        let phaseBytes = max(1, phaseIndices.count) * MemoryLayout<UInt32>.stride
        managedBytes = max(1, layers.count) * Self.maskBytes * channels + bufferBytes + phaseBytes
        // Include transient CPU atlas, packed data and largest temporary mask.
        guard managedBytes * 2 + previousManagedBytes + 64 * 1024 * 1024 <= Self.maximumManagedBytes else { throw Fallback.resourceLimit }
        let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: rgbPhases ? .rgba32Float : .r32Float, width: side, height: side, mipmapped: false)
        descriptor.textureType = .type2DArray
        descriptor.arrayLength = max(1, layers.count)
        descriptor.storageMode = .shared; descriptor.usage = .shaderRead
        guard let atlas = device.makeTexture(descriptor: descriptor),
              let instances = device.makeBuffer(length: bufferBytes, options: .storageModeShared),
              let phaseLookup = device.makeBuffer(length: phaseBytes, options: .storageModeShared) else { throw Fallback.allocation }
        if layers.isEmpty { layers = [[Float](repeating: 0, count: side * side * channels)] }
        for (slice, prefix) in layers.enumerated() {
            prefix.withUnsafeBytes { atlas.replace(region: MTLRegionMake2D(0, 0, side, side),
                mipmapLevel: 0, slice: slice, withBytes: $0.baseAddress!, bytesPerRow: side * channels * 4,
                bytesPerImage: side * side * channels * 4) }
        }
        packed.withUnsafeBytes { if let pointer = $0.baseAddress, !$0.isEmpty {
            instances.contents().copyMemory(from: pointer, byteCount: $0.count)
        } }
        phaseIndices.withUnsafeBytes { if let pointer = $0.baseAddress, !$0.isEmpty {
            phaseLookup.contents().copyMemory(from: pointer, byteCount: $0.count)
        } }
        self.atlas = atlas; self.instances = instances; self.phaseLookup = phaseLookup
        do {
            // Compensated phase arithmetic depends on explicit rounding boundaries.
            // Preserve the existing compiler settings for grayscale rendering.
            let options: MTLCompileOptions?
            if rgbPhases || captureGrid {
                let preciseOptions = MTLCompileOptions()
                preciseOptions.fastMathEnabled = false
                options = preciseOptions
            } else { options = nil }
            let library = try device.makeLibrary(source: Self.shader, options: options)
            let desc = MTLRenderPipelineDescriptor()
            desc.vertexFunction = library.makeFunction(name: "glyphVertex")
            desc.fragmentFunction = library.makeFunction(name: "glyphFragment")
            let attachment = desc.colorAttachments[0]!
            attachment.pixelFormat = .bgra8Unorm
            attachment.isBlendingEnabled = true
            attachment.sourceRGBBlendFactor = .one; attachment.destinationRGBBlendFactor = .oneMinusSourceAlpha
            attachment.sourceAlphaBlendFactor = .one; attachment.destinationAlphaBlendFactor = .oneMinusSourceAlpha
            pipeline = try device.makeRenderPipelineState(descriptor: desc)
            desc.fragmentFunction = library.makeFunction(name: "glyphDeltaFragment")
            attachment.destinationRGBBlendFactor = .one
            attachment.writeMask = [.red, .green, .blue]
            additivePipeline = try device.makeRenderPipelineState(descriptor: desc)
        } catch { throw Fallback.pipeline(String(describing: error)) }
    }

    /// Scale and offset are physical pixels per world point and physical pixels.
    /// Returns an explicit fallback instead of magnifying an inadequate mask.
    func encode(into target: MTLTexture, commandBuffer: MTLCommandBuffer,
                scale: Double, offset: CGPoint, tileIDs: [Int]? = nil) throws {
        guard scale.isFinite, scale > 0, offset.x.isFinite, offset.y.isFinite,
              abs(offset.x) <= 10_000_000, abs(offset.y) <= 10_000_000 else { throw Fallback.invalidGeometry }
        let selected = tileIDs ?? batches.keys.sorted()
        guard selected.count <= batches.count, Set(selected).count == selected.count else { throw Fallback.invalidGeometry }
        for id in selected {
            guard let batch = batches[id] else { throw Fallback.invalidGeometry }
            guard scale <= batch.pixelsPerWorldPoint else { throw Fallback.undersampled }
            guard batch.supportsDisplayDensity(scale) else { throw Fallback.unqualifiedDensity }
        }
        guard target.device.registryID == device.registryID, commandBuffer.device.registryID == device.registryID,
              target.pixelFormat == .bgra8Unorm, target.sampleCount == 1,
              target.textureType == .type2D, target.usage.contains(.renderTarget) else { throw Fallback.incompatibleTarget }
        var uniforms = Uniforms(camera: SIMD4(Float(scale), Float(offset.x), Float(offset.y), Float(scale - Double(Float(scale)))),
            viewport: SIMD4(Float(target.width), Float(target.height),
                Float(offset.x - Double(Float(offset.x))), Float(offset.y - Double(Float(offset.y)))))
        let pass = MTLRenderPassDescriptor()
        pass.colorAttachments[0].texture = target
        pass.colorAttachments[0].loadAction = .clear; pass.colorAttachments[0].storeAction = .store
        pass.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 0)
        guard let encoder = commandBuffer.makeRenderCommandEncoder(descriptor: pass) else { throw Fallback.allocation }
        encoder.setRenderPipelineState(pipeline)
        encoder.setVertexBuffer(instances, offset: 0, index: 0)
        encoder.setVertexBuffer(phaseLookup, offset: 0, index: 2)
        encoder.setVertexBytes(&uniforms, length: MemoryLayout<Uniforms>.stride, index: 1)
        encoder.setFragmentBytes(&uniforms, length: MemoryLayout<Uniforms>.stride, index: 1)
        encoder.setFragmentTexture(atlas, index: 0)
        for id in selected {
            let range = batches[id]!.instanceRange
            if !range.isEmpty {
                encoder.setRenderPipelineState(pipeline)
                if opaqueBackgrounds {
                    encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 6,
                        instanceCount: 1, baseInstance: range.lowerBound)
                    if range.count > 1 {
                        encoder.setRenderPipelineState(additivePipeline)
                        encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 6,
                            instanceCount: range.count - 1, baseInstance: range.lowerBound + 1)
                    }
                } else {
                    encoder.drawPrimitives(type: .triangle, vertexStart: 0, vertexCount: 6,
                        instanceCount: range.count, baseInstance: range.lowerBound)
                }
            }
        }
        encoder.endEncoding()
    }

    private static let shader = """
    #include <metal_stdlib>
    using namespace metal;
    struct Instance { float4 rect; float4 uv; float4 color; float4 clip; float4 baseline; };
    struct Uniforms { float4 camera; float4 viewport; };
    struct Out { float4 position [[position]]; float2 uv; float2 world; float4 color; float4 clip; float solid [[flat]]; uint layer [[flat]]; float4 rect [[flat]]; float4 maskUV [[flat]]; };
    // Double-float operations retain residuals across cancellation and bin boundaries.
    float2 phaseTwoSum(float a, float b) {
        float high = a + b;
        float back = high - a;
        return float2(high, (a - (high - back)) + (b - back));
    }
    float2 phaseAdd(float2 a, float2 b) {
        float2 sum = phaseTwoSum(a.x, b.x);
        float2 tail = phaseTwoSum(a.y, b.y);
        float2 middle = phaseTwoSum(sum.y, tail.x);
        float2 head = phaseTwoSum(sum.x, middle.x);
        return phaseTwoSum(head.x, head.y + middle.y + tail.y);
    }
    float2 phaseProduct(float a, float b) {
        float high = a * b;
        return float2(high, fma(a, b, -high));
    }
    float2 phaseAnchor(float baseHigh, float baseLow, float scaleHigh, float scaleLow,
                       float offsetHigh, float offsetLow) {
        float2 value = phaseAdd(phaseProduct(baseHigh, scaleHigh), float2(offsetHigh, offsetLow));
        value = phaseAdd(value, phaseProduct(baseLow, scaleHigh));
        value = phaseAdd(value, phaseProduct(baseHigh, scaleLow));
        return phaseAdd(value, phaseProduct(baseLow, scaleLow));
    }
    float phaseFloor(float2 value) {
        float whole = floor(value.x);
        float fraction = value.x - whole;
        if (value.y < -fraction) return whole - 1;
        if (value.y >= 1 - fraction) return whole + 1;
        return whole;
    }
    vertex Out glyphVertex(uint vertexIndex [[vertex_id]], uint instance [[instance_id]],
        const device Instance *items [[buffer(0)]], constant Uniforms &u [[buffer(1)]], const device uint *phases [[buffer(2)]]) {
        const float2 corners[6] = {float2(0,0),float2(1,0),float2(0,1),float2(1,0),float2(1,1),float2(0,1)};
        Instance g = items[instance]; float2 corner = corners[vertexIndex]; Out o;
        bool solid = g.uv.z < 0;
        // The qualified grayscale path snaps the actual glyph baseline, not
        // padded mask bounds. RGB banks capture horizontal subpixel phase
        // themselves; floor X locates their integer base without adding phase twice.
        // Top-left Y retains the qualified ceil convention.
        if (!solid && g.uv.y >= 0) {
            // Preserve subpixel residuals before discontinuous floor/ceil:
            // large world coordinates and camera offsets otherwise cancel
            // after Float rounding and spuriously shift near-integer glyphs.
            if (g.uv.y > 0) {
                float2 x = phaseAnchor(g.baseline.x, g.baseline.z, u.camera.x, u.camera.w,
                                       u.camera.y, u.viewport.z);
                float2 y = phaseAnchor(g.baseline.y, g.baseline.w, u.camera.x, u.camera.w,
                                       u.camera.z, u.viewport.w);
                float wholeX = phaseFloor(x);
                float wholeY = -phaseFloor(-y);
                float2 fraction = phaseAdd(x, float2(-wholeX, 0));
                float2 bins = phaseAdd(phaseProduct(fraction.x, 24), phaseProduct(fraction.y, 24));
                uint phase = uint(clamp(phaseFloor(bins), 0.0, 23.0));
                g.uv.x = float(phases[uint(g.uv.y) - 1 + phase]);
                float2 adjustment = float2((wholeX - x.x) - x.y, (wholeY - y.x) - y.y);
                g.rect.xy += adjustment / u.camera.x;
            } else {
            float2 anchor = fma(g.baseline.xy, float2(u.camera.x), u.camera.yz)
                + g.baseline.zw * u.camera.x + g.baseline.xy * u.camera.w + u.viewport.zw;
                float2 snapped = float2(floor(anchor.x), ceil(anchor.y));
                g.rect.xy += (snapped - anchor) / u.camera.x;
            }
        }
        // Exact pixel-box integration reaches half a physical pixel beyond
        // the original mask; clipping remains in authoritative world space.
        float padding = solid ? 0 : 0.5 / u.camera.x;
        o.world = g.rect.xy - padding + corner * (g.rect.zw + 2 * padding);
        float2 pixel = o.world * u.camera.x + u.camera.yz;
        float4 screenRect = float4(0);
        if (g.uv.y < 0) {
            // Stable capture stores origin residuals in otherwise-unused baseline.zw.
            // Cancel world/camera translations before rounding into screen coordinates.
            float2 x = phaseAnchor(g.rect.x, g.baseline.z, u.camera.x, u.camera.w,
                                   u.camera.y, u.viewport.z);
            float2 y = phaseAnchor(g.rect.y, g.baseline.w, u.camera.x, u.camera.w,
                                   u.camera.z, u.viewport.w);
            screenRect = float4(x.x + x.y, y.x + y.y,
                                g.rect.zw * u.camera.x + g.rect.zw * u.camera.w);
            float screenPadding = solid ? 0 : 0.5;
            pixel = screenRect.xy - screenPadding + corner * (screenRect.zw + 2 * screenPadding);
        }
        o.position = float4(pixel.x / u.viewport.x * 2 - 1, 1 - pixel.y / u.viewport.y * 2, 0, 1);
        float2 uvPadding = solid ? float2(0) : padding * g.uv.zw / g.rect.zw;
        o.uv = float2(1.0 / 128.0) - uvPadding + corner * (g.uv.zw + 2 * uvPadding);
        o.rect = g.uv.y < 0 ? screenRect : g.rect; o.maskUV = g.uv; o.layer = uint(max(0.0, g.uv.x)); o.color = g.color; o.clip = g.clip; o.solid = solid ? 1 : 0; return o;
    }
    float prefixRead(texture2d_array<float> atlas, uint2 point, uint layer) {
        // The omitted zero row/column are implicit, preserving a power-of-two
        // 128² allocation while representing all 129² integration boundaries.
        return any(point == 0) ? 0 : atlas.read(point - 1, layer).r;
    }
    float prefixAt(texture2d_array<float> atlas, float2 p, uint layer) {
        p = clamp(p, float2(0), float2(128));
        uint2 lo = uint2(floor(p)); uint2 hi = min(lo + 1, uint2(128));
        float2 f = fract(p);
        // Explicit interpolation avoids requiring float32 texture filtering.
        float a = mix(prefixRead(atlas, lo, layer), prefixRead(atlas, uint2(hi.x, lo.y), layer), f.x);
        float b = mix(prefixRead(atlas, uint2(lo.x, hi.y), layer), prefixRead(atlas, hi, layer), f.x);
        return mix(a, b, f.y);
    }
    float boxCoverage(texture2d_array<float> atlas, Out in, constant Uniforms &u) {
        // Pixel centers minus camera translation are exactly invariant under
        // integer panning. Interpolated UV/derivatives can jitter by an ulp,
        // amplified by subtraction of large prefix sums.
        float2 center, extent;
        if (in.maskUV.y < 0) {
            float2 texelsPerPixel = in.maskUV.zw * 128 / in.rect.zw;
            center = float2(1) + (in.position.xy - in.rect.xy) * texelsPerPixel;
            extent = max(texelsPerPixel, float2(0.000001));
        } else {
            float2 world = (in.position.xy - u.camera.yz) / u.camera.x;
            float2 texelsPerWorld = in.maskUV.zw * 128 / in.rect.zw;
            center = float2(1) + (world - in.rect.xy) * texelsPerWorld;
            extent = max(texelsPerWorld / u.camera.x, float2(0.000001));
        }
        uint layer = in.layer;
        float2 lo = center - extent * 0.5, hi = center + extent * 0.5;
        float sum = prefixAt(atlas, hi, layer) - prefixAt(atlas, float2(lo.x, hi.y), layer)
            - prefixAt(atlas, float2(hi.x, lo.y), layer) + prefixAt(atlas, lo, layer);
        return clamp(sum / (extent.x * extent.y * 255), 0.0, 1.0);
    }
    float3 rgbPrefixRead(texture2d_array<float> atlas, uint2 point, uint layer) {
        return any(point == 0) ? float3(0) : atlas.read(point - 1, layer).rgb;
    }
    float3 rgbPrefixAt(texture2d_array<float> atlas, float2 p, uint layer) {
        p = clamp(p, float2(0), float2(128));
        uint2 lo = uint2(floor(p)); uint2 hi = min(lo + 1, uint2(128));
        float2 f = fract(p);
        float3 a = mix(rgbPrefixRead(atlas, lo, layer), rgbPrefixRead(atlas, uint2(hi.x, lo.y), layer), f.x);
        float3 b = mix(rgbPrefixRead(atlas, uint2(lo.x, hi.y), layer), rgbPrefixRead(atlas, hi, layer), f.x);
        return mix(a, b, f.y);
    }
    float3 rgbBoxDelta(texture2d_array<float> atlas, Out in, constant Uniforms &u) {
        float2 center, extent;
        if (in.maskUV.y < 0) {
            // Avoid subtracting large, nearly equal world coordinates per pixel.
            float2 texelsPerPixel = in.maskUV.zw * 128 / in.rect.zw;
            center = float2(1) + (in.position.xy - in.rect.xy) * texelsPerPixel;
            extent = max(texelsPerPixel, float2(0.000001));
        } else {
            float2 world = (in.position.xy - u.camera.yz) / u.camera.x;
            float2 texelsPerWorld = in.maskUV.zw * 128 / in.rect.zw;
            center = float2(1) + (world - in.rect.xy) * texelsPerWorld;
            extent = max(texelsPerWorld / u.camera.x, float2(0.000001));
        }
        float2 lo = center - extent * 0.5, hi = center + extent * 0.5;
        float3 sum = rgbPrefixAt(atlas, hi, in.layer) - rgbPrefixAt(atlas, float2(lo.x, hi.y), in.layer)
            - rgbPrefixAt(atlas, float2(hi.x, lo.y), in.layer) + rgbPrefixAt(atlas, lo, in.layer);
        return sum / (extent.x * extent.y * 255);
    }
    fragment float4 glyphFragment(Out in [[stage_in]], texture2d_array<float> atlas [[texture(0)]], constant Uniforms &u [[buffer(1)]]) {
        float2 world = (in.position.xy - u.camera.yz) / u.camera.x;
        if (any(world < in.clip.xy) || any(world >= in.clip.zw)) discard_fragment();
        float alpha = (in.solid > 0 ? 1 : boxCoverage(atlas, in, u)) * in.color.a;
        return float4(in.color.rgb * alpha, alpha);
    }
    fragment float4 glyphDeltaFragment(Out in [[stage_in]], texture2d_array<float> atlas [[texture(0)]], constant Uniforms &u [[buffer(1)]]) {
        float2 world = (in.position.xy - u.camera.yz) / u.camera.x;
        if (any(world < in.clip.xy) || any(world >= in.clip.zw)) discard_fragment();
        if (in.maskUV.y > 0 || in.maskUV.y == -1) return float4(rgbBoxDelta(atlas, in, u), 0);
        float coverage = boxCoverage(atlas, in, u) * in.color.a;
        const float3 background = float3(22.0, 26.0, 29.0) / 255.0;
        // Filtering and addition commute for disjoint source ink. Source-over
        // after filtering would spuriously subtract overlapping filter lobes.
        return float4((in.color.rgb - background) * coverage, 0);
    }
    """
}
