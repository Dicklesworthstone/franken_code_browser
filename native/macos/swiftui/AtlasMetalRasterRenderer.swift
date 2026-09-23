import AppKit
import Metal

/// Immutable overview images uploaded once per scene, never during camera delivery.
/// Images are premultiplied sRGB, matching the presentation layer's color space.
@MainActor final class AtlasMetalRasterRenderer {
    struct Tile {
        let image: CGImage
        let rect: CGRect
        let texture: MTLTexture
        fileprivate let quad: Quad
        func supports(_ density: Double) -> Bool {
            density.isFinite && density > 0 && Double(image.width) / rect.width >= density * 1.15 &&
                Double(image.height) / rect.height >= density * 1.15
        }
    }
    let tiles: [Int: Tile]
    let managedBytes: Int
    let gpuOptimizationApplied: Bool
    private let device: MTLDevice
    private let direct: MTLRenderPipelineState
    private let accumulated: MTLRenderPipelineState?
    fileprivate struct Quad { var rect: SIMD4<Float>; var residual: SIMD4<Float> }
    private struct Camera { var high: SIMD4<Float>; var low: SIMD4<Float> }

    init(device: MTLDevice, tiles source: [AtlasTextTile], previousManagedBytes: Int,
         maximumManagedBytes: Int? = nil,
         optimizeForGPU: Bool = ProcessInfo.processInfo.environment["FCB_RASTER_OPTIMIZE"] == "1") throws {
        self.device = device
        guard source.count <= 100_000, previousManagedBytes >= 0 else {
            throw AtlasMetalGlyphRenderer.Fallback.resourceLimit
        }
        let options = MTLCompileOptions(); options.fastMathEnabled = false
        let library = try device.makeLibrary(source: Self.shader, options: options)
        func pipeline(_ tileMemory: Bool) throws -> MTLRenderPipelineState {
            let descriptor = MTLRenderPipelineDescriptor()
            descriptor.vertexFunction = library.makeFunction(name: "rasterVertex")
            descriptor.fragmentFunction = library.makeFunction(name: tileMemory ? "rasterAccumulatedFragment" : "rasterFragment")
            let color = descriptor.colorAttachments[0]!
            color.pixelFormat = tileMemory ? .rgba16Float : .bgra8Unorm
            color.isBlendingEnabled = true
            color.sourceRGBBlendFactor = .one; color.destinationRGBBlendFactor = .oneMinusSourceAlpha
            color.sourceAlphaBlendFactor = .one; color.destinationAlphaBlendFactor = .oneMinusSourceAlpha
            if tileMemory {
                descriptor.colorAttachments[1].pixelFormat = .bgra8Unorm
                descriptor.colorAttachments[1].writeMask = []
            }
            return try device.makeRenderPipelineState(descriptor: descriptor)
        }
        direct = try pipeline(false)
        accumulated = device.supportsFamily(.apple2) ? try pipeline(true) : nil
        var retained: [Int: Tile] = [:]
        var bytes = 0
        let limit = max(0, (maximumManagedBytes ?? AtlasMetalGlyphRenderer.maximumManagedBytes) - max(0, previousManagedBytes) - 1024 * 1024)
        for (index, tile) in source.enumerated() {
            guard let image = tile.raster, tile.rect.width > 0, tile.rect.height > 0,
                  [tile.rect.minX, tile.rect.minY, tile.rect.maxX, tile.rect.maxY].allSatisfy({ $0.isFinite && abs($0) <= 10_000_000 }),
                  image.width > 0, image.height > 0, image.width <= 16_384, image.height <= 16_384 else { continue }
            let cost = image.width * image.height * 4
            // Conversion bytes and a conservative Metal allocation allowance coexist.
            guard cost <= (limit - bytes) / 3 else { continue }
            let descriptor = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .rgba8Unorm,
                width: image.width, height: image.height, mipmapped: false)
            descriptor.storageMode = .shared; descriptor.usage = .shaderRead
            let estimate = device.heapTextureSizeAndAlign(descriptor: descriptor).size
            guard estimate <= limit - bytes - cost else { continue }
            guard let texture = device.makeTexture(descriptor: descriptor),
                  max(cost, texture.allocatedSize) + cost <= limit - bytes,
                  let context = CGContext(data: nil, width: image.width, height: image.height,
                    bitsPerComponent: 8, bytesPerRow: image.width * 4,
                    space: CGColorSpace(name: CGColorSpace.sRGB)!,
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue),
                  let data = context.data else { continue }
            context.setBlendMode(.copy)
            context.interpolationQuality = .none
            context.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
            texture.replace(region: MTLRegionMake2D(0, 0, image.width, image.height), mipmapLevel: 0,
                withBytes: data, bytesPerRow: image.width * 4)
            let allocation = max(cost, texture.allocatedSize)
            guard allocation <= limit - bytes else { continue }
            bytes += allocation
            let values = [tile.rect.minX, tile.rect.minY, tile.rect.maxX, tile.rect.maxY]
            let high = SIMD4<Float>(values.map(Float.init))
            let quad = Quad(rect: high, residual: SIMD4<Float>((0..<4).map { Float(values[$0] - Double(high[$0])) }))
            retained[index] = Tile(image: image, rect: tile.rect, texture: texture, quad: quad)
        }
        var optimized = false
        // Explicit experiment only. Reserve a second complete allocation while
        // the driver reorganizes texture storage; insufficient headroom preserves
        // the original scene and coverage without submitting any optimization.
        if optimizeForGPU && !retained.isEmpty && bytes <= limit - bytes {
            guard let queue = device.makeCommandQueue(),
                  let command = queue.makeCommandBuffer(),
                  let blit = command.makeBlitCommandEncoder() else {
                throw AtlasMetalGlyphRenderer.Fallback.allocation
            }
            for id in retained.keys.sorted() {
                blit.optimizeContentsForGPUAccess(texture: retained[id]!.texture)
            }
            blit.endEncoding()
            command.commit()
            // Once per immutable scene, never during camera delivery. The local
            // retained dictionary owns every texture until this terminal fence.
            command.waitUntilCompleted()
            guard command.status == .completed else {
                throw AtlasMetalGlyphRenderer.Fallback.allocation
            }
            bytes = retained.values.reduce(0) {
                $0 + max($1.image.width * $1.image.height * 4, $1.texture.allocatedSize)
            }
            guard bytes <= limit else { throw AtlasMetalGlyphRenderer.Fallback.resourceLimit }
            optimized = true
        }
        tiles = retained; managedBytes = bytes; gpuOptimizationApplied = optimized
    }

    func validate(scale: Double, tileIDs: [Int], deviceRegistryID: UInt64? = nil) throws {
        guard deviceRegistryID == nil || deviceRegistryID == device.registryID else {
            throw AtlasMetalGlyphRenderer.Fallback.incompatibleTarget
        }
        guard Set(tileIDs).count == tileIDs.count,
              tileIDs.allSatisfy({ tiles[$0]?.supports(scale) == true }) else {
            throw AtlasMetalGlyphRenderer.Fallback.invalidGeometry
        }
    }

    func encode(encoder: MTLRenderCommandEncoder, tileMemory: Bool, width: Int, height: Int,
                scale: Double, offset: CGPoint, tileIDs: [Int]) {
        encoder.setRenderPipelineState(tileMemory ? accumulated! : direct)
        var camera = Camera(high: SIMD4(Float(scale), Float(offset.x), Float(offset.y), 0),
            low: SIMD4(Float(scale - Double(Float(scale))), Float(offset.x - Double(Float(offset.x))),
                Float(offset.y - Double(Float(offset.y))), 0))
        var viewport = SIMD2<Float>(Float(width), Float(height))
        encoder.setVertexBytes(&camera, length: MemoryLayout<Camera>.stride, index: 1)
        encoder.setVertexBytes(&viewport, length: MemoryLayout<SIMD2<Float>>.stride, index: 2)
        for id in tileIDs {
            guard let tile = tiles[id] else { continue }
            var quad = tile.quad
            encoder.setVertexBytes(&quad, length: MemoryLayout<Quad>.stride, index: 0)
            encoder.setFragmentTexture(tile.texture, index: 0)
            encoder.drawPrimitives(type: .triangleStrip, vertexStart: 0, vertexCount: 4)
        }
    }

    func encode(into target: MTLTexture, commandBuffer: MTLCommandBuffer,
                scale: Double, offset: CGPoint, tileIDs: [Int]) throws {
        guard scale.isFinite, scale > 0, offset.x.isFinite, offset.y.isFinite,
              abs(offset.x) <= 10_000_000, abs(offset.y) <= 10_000_000,
              Set(tileIDs).count == tileIDs.count,
              tileIDs.allSatisfy({ tiles[$0]?.supports(scale) == true }) else {
            throw AtlasMetalGlyphRenderer.Fallback.invalidGeometry
        }
        guard target.device.registryID == device.registryID, commandBuffer.device.registryID == device.registryID,
              target.pixelFormat == .bgra8Unorm, target.sampleCount == 1,
              target.textureType == .type2D, target.usage.contains(.renderTarget) else {
            throw AtlasMetalGlyphRenderer.Fallback.incompatibleTarget
        }
        let pass = MTLRenderPassDescriptor()
        pass.colorAttachments[0].texture = target
        pass.colorAttachments[0].loadAction = .clear; pass.colorAttachments[0].storeAction = .store
        pass.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 0)
        guard let encoder = commandBuffer.makeRenderCommandEncoder(descriptor: pass) else {
            throw AtlasMetalGlyphRenderer.Fallback.allocation
        }
        encode(encoder: encoder, tileMemory: false, width: target.width, height: target.height,
            scale: scale, offset: offset, tileIDs: tileIDs)
        encoder.endEncoding()
    }

    private static let shader = """
    #include <metal_stdlib>
    using namespace metal;
    struct Quad { float4 rect; float4 residual; };
    struct Camera { float4 high; float4 low; };
    struct Out { float4 position [[position]]; float4 rect [[flat]]; float2 uv; };
    vertex Out rasterVertex(uint i [[vertex_id]], constant Quad &q [[buffer(0)]],
        constant Camera &c [[buffer(1)]], constant float2 &viewport [[buffer(2)]]) {
        float4 hi = q.rect, lo = q.residual;
        float4 translation = c.high.yzyz;
        float4 product = hi * c.high.x;
        float4 error = fma(hi, c.high.x, -product) + lo*c.high.x + hi*c.low.x + lo*c.low.x;
        float4 sum = product + translation;
        float4 back = sum - product;
        float4 rect = sum + ((product - (sum-back)) + (translation-back) + error + c.low.yzyz);
        // Retain fractional vertex phases for hardware interpolation while
        // admitting edge pixels whose centers fall outside the source rectangle.
        float2 screen = float2(i & 1 ? rect.z+0.5f : rect.x-0.5f, i >> 1 ? rect.w+0.5f : rect.y-0.5f);
        return Out{float4(screen.x/viewport.x*2-1, 1-screen.y/viewport.y*2, 0, 1), rect, (screen-rect.xy)/(rect.zw-rect.xy)};
    }
    float4 rasterSample(Out in, texture2d<float> image) {
        constexpr sampler linearSampler(coord::normalized, address::clamp_to_edge, filter::linear);
        float2 coverage = clamp(min(in.rect.zw,in.position.xy+0.5f)-max(in.rect.xy,in.position.xy-0.5f),0.0f,1.0f);
        float2 uv = in.uv;
        return image.sample(linearSampler, uv) * coverage.x * coverage.y;
    }
    fragment float4 rasterFragment(Out in [[stage_in]], texture2d<float> image [[texture(0)]]) {
        return rasterSample(in,image);
    }
    fragment float4 rasterAccumulatedFragment(Out in [[stage_in]], texture2d<float> image [[texture(0)]]) {
        // Match the direct UNORM target before half storage to avoid double rounding.
        return floor(rasterSample(in,image)*255.0f+0.5f)/255.0f;
    }
    """
}
