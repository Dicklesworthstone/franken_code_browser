import AppKit
import CoreText
import Metal
import CryptoKit
@main struct QuadStripComparison {
    @MainActor static func main() throws {
        let device = MTLCreateSystemDefaultDevice()!, queue = device.makeCommandQueue()!
        let source = String(String(repeating: "FgjR", count: 63).prefix(250))
        let line = CTLineCreateWithAttributedString(NSAttributedString(string: source, attributes: [.font: NSFont.monospacedSystemFont(ofSize: 8, weight: .regular), .foregroundColor: NSColor(srgbRed: 166.0/255, green: 226.0/255, blue: 46.0/255, alpha: 1)]))
        let row = AtlasPreparedLine(line, source: source)!
        let desc = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .bgra8Unorm, width: 1476, height: 1488, mipmapped: false)
        desc.storageMode = .shared; desc.usage = .renderTarget
        let targets = [device.makeTexture(descriptor: desc)!, device.makeTexture(descriptor: desc)!]
        for count in [1, 16] {
            var aTiles: [AtlasMetalGlyphRendererControl.SceneTile] = []
            var bTiles: [AtlasMetalGlyphRenderer.SceneTile] = []
            for id in 0..<count {
                let rect = CGRect(x: (id % 4) * 1290, y: (id / 4) * 2058, width: 1280, height: 2048)
                let aRows = (0..<200).map { i in AtlasMetalGlyphRendererControl.Line(text: row, origin: CGPoint(x: 20, y: 20 + i * 10), clip: rect) }
                let bRows = (0..<200).map { i in AtlasMetalGlyphRenderer.Line(text: row, origin: CGPoint(x: 20, y: 20 + i * 10), clip: rect) }
                aTiles.append(.init(id: id, rect: rect, sourceScale: 1, lines: aRows))
                bTiles.append(.init(id: id, rect: rect, sourceScale: 1, lines: bRows))
            }
            let a = try AtlasMetalGlyphRendererControl(device: device, tiles: aTiles, pixelsPerPoint: 4, minimumPixelsPerPoint: 0.1, maskPlacement: .captureGrid)
            let b = try AtlasMetalGlyphRenderer(device: device, tiles: bTiles, pixelsPerPoint: 4, minimumPixelsPerPoint: 0.1, maskPlacement: .captureGrid)
            precondition(a.instanceCount == b.instanceCount && a.fallbackTiles.isEmpty && b.fallbackTiles.isEmpty)
            func render(_ which: Int, _ scale: Double, _ phase: Double) throws -> Double {
                let command = queue.makeCommandBuffer()!
                let offset = CGPoint(x: 4 + phase, y: 4 + phase)
                if which == 0 { try a.encode(into: targets[0], commandBuffer: command, scale: scale, offset: offset) }
                else { try b.encode(into: targets[1], commandBuffer: command, scale: scale, offset: offset) }
                command.commit(); command.waitUntilCompleted()
                precondition(command.status == .completed && command.error == nil)
                precondition(command.gpuEndTime > command.gpuStartTime && command.gpuStartTime > 0)
                return (command.gpuEndTime - command.gpuStartTime) * 1000
            }
            func pixels(_ which: Int) -> [UInt8] {
                var data = [UInt8](repeating: 0, count: 1476 * 1488 * 4)
                data.withUnsafeMutableBytes { targets[which].getBytes($0.baseAddress!, bytesPerRow: 1476 * 4, from: MTLRegionMake2D(0,0,1476,1488), mipmapLevel: 0) }
                return data
            }
            for scale in [0.1, 0.15, 0.25, 0.5, 1.0, 2.0, 4.0] {
                for phase in [0.0, 0.25, 0.5, 0.75] {
                    _ = try render(0, scale, phase); _ = try render(1, scale, phase)
                    let control = pixels(0), strip = pixels(1)
                    precondition(control == strip, "Triangle strip must preserve every rendered pixel")
                    precondition(control.contains { $0 != 0 })
                }
            }
            print("PIXEL_PARITY instances=\(a.instanceCount) transforms=28 bytes_per_transform=\(1476*1488*4)")
            for run in -3..<20 {
                var values = [[Double](),[Double]()]
                for frame in 0..<8 {
                    let scale = (count == 1 ? 0.5 : 0.15) + Double(frame) * 0.0001
                    for which in (frame % 2 == 0 ? [0,1,1,0] : [1,0,0,1]) {
                        values[which].append(try render(which, scale, Double(frame % 4) * 0.25))
                    }
                }
                let result: [String: Any] = ["phase":"quad-strip","run":run,"instances":a.instanceCount,"control_gpu_ms":values[0],"strip_gpu_ms":values[1]]
                print(String(data: try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys]), encoding: .utf8)!)
                fflush(stdout)
            }
        }
    }
}
