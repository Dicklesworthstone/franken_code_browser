import AppKit
import QuartzCore
import Metal

/// Original CALayer filtering is the oracle; texture bytes are never the reference.
@MainActor enum AtlasMetalRasterTests {
    static func output(_ value: [String: Any]) throws {
        let data = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
        FileHandle.standardOutput.write(data + Data([10]))
    }
    static func texture(_ device: MTLDevice, width: Int = 512, height: Int = 256) -> MTLTexture {
        let d = MTLTextureDescriptor.texture2DDescriptor(pixelFormat: .bgra8Unorm, width: width, height: height, mipmapped: false)
        d.storageMode = .shared; d.usage = [.renderTarget, .shaderRead]
        return device.makeTexture(descriptor: d)!
    }
    static func bytes(_ t: MTLTexture, flipped: Bool = false) -> [UInt8] {
        var result = [UInt8](repeating: 0, count: t.width*t.height*4)
        result.withUnsafeMutableBytes { t.getBytes($0.baseAddress!, bytesPerRow: t.width*4,
            from: MTLRegionMake2D(0,0,t.width,t.height), mipmapLevel: 0) }
        if !flipped { return result }
        return (0..<t.height).reversed().flatMap { Array(result[($0*t.width*4)..<(($0+1)*t.width*4)]) }
    }
    static func reference(tile: AtlasTextTile, scale: Double, offset: CGPoint,
                          device: MTLDevice, queue: MTLCommandQueue) -> [UInt8] {
        return reference(tiles: [tile], scale: scale, offset: offset, device: device, queue: queue)
    }
    static func reference(tiles: [AtlasTextTile], scale: Double, offset: CGPoint,
                          device: MTLDevice, queue: MTLCommandQueue) -> [UInt8] {
        let target = texture(device)
        CATransaction.begin(); CATransaction.setDisableActions(true)
        let root = CALayer(); root.frame = CGRect(x:0,y:0,width:512,height:256); root.isGeometryFlipped = true
        let world = CALayer(); world.anchorPoint = .zero
        world.setAffineTransform(CGAffineTransform(a:scale,b:0,c:0,d:scale,tx:offset.x,ty:offset.y)); root.addSublayer(world)
        for tile in tiles {
        let image = CALayer(); image.anchorPoint = .zero; image.frame = tile.rect; image.contents = tile.raster
        image.contentsGravity = .resize; image.minificationFilter = .linear; image.magnificationFilter = .linear
        world.addSublayer(image)
        }
        let renderer = CARenderer(mtlTexture:target, options:[kCARendererMetalCommandQueue:queue,
            kCARendererColorSpace:CGColorSpace(name:CGColorSpace.sRGB)!])
        renderer.layer = root; renderer.bounds = root.bounds
        CATransaction.commit(); CATransaction.flush()
        renderer.beginFrame(atTime:CACurrentMediaTime(),timeStamp:nil); renderer.addUpdate(root.bounds)
        renderer.render(); renderer.endFrame()
        let fence = queue.makeCommandBuffer()!; fence.commit(); fence.waitUntilCompleted()
        precondition(fence.status == .completed)
        // CARenderer targets are bottom-up; the production Metal view is top-down.
        return bytes(target, flipped:true)
    }
    static func compare(_ actual: [UInt8], _ expected: [UInt8]) -> (max: Int, error: Double, energy: Int) {
        var maximum=0, error=0.0, energy=0.0
        for i in actual.indices { maximum=max(maximum,abs(Int(actual[i])-Int(expected[i]))) }
        // Measure visible ink over Monokai, excluding both transparent space and
        // the opaque cached background. Background pixels cannot dilute this gate.
        for i in stride(from:0,to:actual.count,by:4) { for c in 0..<3 {
            let background=Double([29,26,22][c])
            let a=Double(actual[i+c])+background*(1-Double(actual[i+3])/255)
            let e=Double(expected[i+c])+background*(1-Double(expected[i+3])/255)
            error += abs(a-e); energy += abs(e-background)
        }}
        return (maximum,error/max(1,energy),Int(energy))
    }
    static func verifyAccepted(_ surface: AtlasRetainedSurface) throws {
        guard let metal=surface.metal, let frame=metal.acceptedFrame, let raster=metal.rasterScene,
              let first=surface.gpuRasterTiles.sorted().first else { return }
        precondition(surface.gpuGlyphTiles.isDisjoint(with:surface.gpuRasterTiles))
        precondition(surface.gpuCoveredTiles == surface.gpuGlyphTiles.union(surface.gpuRasterTiles))
        let scale=frame.scale*frame.backing
        let tile=frame.tiles[first]
        let projected=CGPoint(x:tile.rect.midX*scale+frame.offset.x*frame.backing,
                              y:tile.rect.midY*scale+frame.offset.y*frame.backing)
        let offset=CGPoint(x:frame.offset.x*frame.backing-floor(projected.x-256),
                           y:frame.offset.y*frame.backing-floor(projected.y-128))
        let ids=surface.gpuRasterTiles.sorted(), glyphIDs=surface.gpuGlyphTiles.sorted()
        for id in ids {
            precondition(raster.tiles[id]?.image === frame.tiles[id].raster)
            precondition(raster.tiles[id]!.supports(scale))
        }
        for image in surface.retainedOverviewImages where surface.gpuRasterTiles.contains(image.tile) {
            precondition(!image.visible,"Accepted Metal source must hide duplicate CPU image")
        }
        let device=metal.device,queue=device.makeCommandQueue()!
        let target=texture(device),glyphTarget=texture(device)
        let command=queue.makeCommandBuffer()!,glyphCommand=queue.makeCommandBuffer()!
        if let glyph=metal.scene {
            try glyph.encode(into:target,commandBuffer:command,scale:scale,offset:offset,tileIDs:glyphIDs,raster:raster,rasterIDs:ids)
            try glyph.encode(into:glyphTarget,commandBuffer:glyphCommand,scale:scale,offset:offset,tileIDs:glyphIDs)
        } else {
            precondition(glyphIDs.isEmpty)
            try raster.encode(into:target,commandBuffer:command,scale:scale,offset:offset,tileIDs:ids)
            try raster.encode(into:glyphTarget,commandBuffer:glyphCommand,scale:scale,offset:offset,tileIDs:[])
        }
        command.commit();glyphCommand.commit();command.waitUntilCompleted();glyphCommand.waitUntilCompleted()
        precondition(command.status == .completed && glyphCommand.status == .completed)
        let ca=reference(tiles:ids.map{frame.tiles[$0]},scale:scale,offset:offset,device:device,queue:queue)
        var expected=bytes(glyphTarget)
        for i in stride(from:0,to:expected.count,by:4) {
            let alpha=Int(ca[i+3])
            for c in 0..<4 {expected[i+c]=UInt8(min(255,Int(ca[i+c])+(Int(expected[i+c])*(255-alpha)+127)/255))}
        }
        let result=compare(bytes(target),expected)
        try output(["phase":"raster-connected","raster_tiles":ids.count,"glyph_tiles":glyphIDs.count,
            "scale":scale,"max_byte_error":result.max,"normalized_ink_error":result.error,"reference_energy":result.energy])
        precondition(result.energy>100 && result.max<=1 && result.error<=0.02,"Accepted camera and coverage preserve CA source pixels")
    }
    static func mixed() throws {
        let device=MTLCreateSystemDefaultDevice()!, queue=device.makeCommandQueue()!
        let text=CTLineCreateWithAttributedString(NSAttributedString(string:String(repeating:"fn main() { let golden = 0xF92672; } ",count:5), attributes:[
            .font:NSFont.monospacedSystemFont(ofSize:13,weight:.regular),.foregroundColor:Monokai.color("keyword")]))
        for worldX in [0.0, 1_000_000.125] { for gap in [0.0,152.0] {
        let a=AtlasTextTile(path:"left.rs",lines:Array(repeating:text,count:12),sourceRange:NSRange(location:0,length:1),part:0)
        a.rect=CGRect(x:worldX,y:0,width:648,height:a.height); a.prepareRaster(pixelBudget:648*256)
        let b=AtlasTextTile(path:"right.rs",lines:Array(repeating:text,count:12),sourceRange:NSRange(location:0,length:1),part:0)
        b.rect=CGRect(x:worldX+648+gap,y:0,width:648,height:b.height)
        let raster=try AtlasMetalRasterRenderer(device:device,tiles:[a,b],previousManagedBytes:0)
        let rows=[AtlasMetalGlyphRenderer.Line(text:AtlasPreparedLine(b.header!)!,origin:CGPoint(x:4,y:12),clip:b.rect)] +
            b.lines.enumerated().map { AtlasMetalGlyphRenderer.Line(text:AtlasPreparedLine($0.element)!,
                origin:CGPoint(x:4,y:34+Double($0.offset)*16),clip:b.rect) }
        let glyph=try AtlasMetalGlyphRenderer(device:device,tiles:[.init(id:1,rect:b.rect,sourceScale:1,lines:rows)],
            pixelsPerPoint:4,minimumPixelsPerPoint:0.04,maskPlacement:.captureGrid,tileMemoryAccumulation:true)
        precondition(glyph.batches[1] != nil)
        for scale in [0.05,0.2] { for phase in [0.0,0.25,0.75] {
            let offset=CGPoint(x:12+phase-worldX*scale,y:8+phase)
            let combined=texture(device), glyphOnly=texture(device)
            let first=queue.makeCommandBuffer()!,second=queue.makeCommandBuffer()!
            try glyph.encode(into:combined,commandBuffer:first,scale:scale,offset:offset,tileIDs:[1],raster:raster,rasterIDs:[0])
            try glyph.encode(into:glyphOnly,commandBuffer:second,scale:scale,offset:offset,tileIDs:[1])
            first.commit();second.commit();first.waitUntilCompleted();second.waitUntilCompleted()
            precondition(first.status == .completed && second.status == .completed)
            let actual=bytes(combined), only=bytes(glyphOnly)
            let ca=reference(tile:a,scale:scale,offset:offset,device:device,queue:queue)
            var expected=only
            for i in stride(from:0,to:expected.count,by:4) {
                let alpha=Int(ca[i+3])
                for c in 0..<4 {expected[i+c]=UInt8(min(255,Int(ca[i+c])+(Int(expected[i+c])*(255-alpha)+127)/255))}
            }
            let result=compare(actual,expected)
            try output(["phase":"mixed-diagnostic","world_x":worldX,"gap":gap,"scale":scale,"camera_phase":phase,"max":result.max,"error":result.error,"energy":result.energy,"ca_energy":ca.reduce(0){$0+Int($1)}])
            if result.max>1 {
                let artifactPath=ProcessInfo.processInfo.environment["FCB_RASTER_ARTIFACTS"]
                    ?? (NSTemporaryDirectory() as NSString).appendingPathComponent("fcb-raster-artifacts")
                let dir=URL(fileURLWithPath:artifactPath,isDirectory:true)
                try FileManager.default.createDirectory(at:dir,withIntermediateDirectories:true)
                try Data(actual).write(to:dir.appendingPathComponent("mixed-gpu.bgra"))
                try Data(expected).write(to:dir.appendingPathComponent("mixed-expected.bgra"))
                try Data(ca).write(to:dir.appendingPathComponent("mixed-ca.bgra"))
            }
            precondition(result.energy>100 && result.max<=1 && result.error<=0.02,"Mixed pass preserves raster and glyph pixels")
            precondition(compare(only,expected).max>1,"Clearing raster before glyphs must fail")
            try output(["phase":"raster-mixed","tile_memory":scale<0.1,"scale":scale,
                "camera_phase":phase,"max_byte_error":result.max,"normalized_ink_error":result.error,
                "two_outstanding_commands":true,"overlap_pixels":stride(from:3,to:ca.count,by:4).filter{ca[$0]>0 && only[$0]>0}.count])
        }}
        }}
    }
    static func optimization() throws {
        let device = MTLCreateSystemDefaultDevice()!, queue = device.makeCommandQueue()!
        let line = CTLineCreateWithAttributedString(NSAttributedString(string: "fn golden() { 42 }", attributes: [
            .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular),
            .foregroundColor: Monokai.color("keyword")]))
        let tiles = (0..<12).map { index in
            let tile = AtlasTextTile(path: "optimized-\(index).rs", lines: [line],
                sourceRange: NSRange(location: 0, length: 1), part: 0)
            tile.rect = CGRect(x: Double(index % 4) * 648, y: Double(index / 4) * tile.height,
                width: 648, height: tile.height)
            tile.prepareRaster(pixelBudget: 648 * 128)
            return tile
        }
        let control = try AtlasMetalRasterRenderer(device: device, tiles: tiles,
            previousManagedBytes: 0, optimizeForGPU: false)
        let optimized = try AtlasMetalRasterRenderer(device: device, tiles: tiles,
            previousManagedBytes: control.managedBytes, optimizeForGPU: true)
        precondition(!control.gpuOptimizationApplied && optimized.gpuOptimizationApplied)
        precondition(Set(control.tiles.keys) == Set(optimized.tiles.keys) && control.tiles.count == tiles.count)
        let largest = control.tiles.values.map { max($0.texture.allocatedSize, $0.image.width * $0.image.height * 4) }.max()!
        let limit = 1024 * 1024 + control.managedBytes + 3 * largest
        let limited = try AtlasMetalRasterRenderer(device: device, tiles: tiles,
            previousManagedBytes: 0, maximumManagedBytes: limit, optimizeForGPU: true)
        precondition(!limited.gpuOptimizationApplied && Set(limited.tiles.keys) == Set(control.tiles.keys),
            "Insufficient optimization reserve must preserve original coverage")
        precondition(limited.managedBytes <= limit && optimized.managedBytes <= AtlasMetalGlyphRenderer.maximumManagedBytes)
        let ids = tiles.indices.map { $0 }
        for phase in [0.0, 0.25, 0.75] {
            let offset = CGPoint(x: 2 + phase, y: 3 + phase)
            let a = texture(device), b = texture(device), c = texture(device)
            let first = queue.makeCommandBuffer()!, second = queue.makeCommandBuffer()!, third = queue.makeCommandBuffer()!
            try control.encode(into: a, commandBuffer: first, scale: 0.15, offset: offset, tileIDs: ids)
            try optimized.encode(into: b, commandBuffer: second, scale: 0.15, offset: offset, tileIDs: ids)
            try limited.encode(into: c, commandBuffer: third, scale: 0.15, offset: offset, tileIDs: ids)
            first.commit(); second.commit(); third.commit()
            first.waitUntilCompleted(); second.waitUntilCompleted(); third.waitUntilCompleted()
            precondition([first, second, third].allSatisfy { $0.status == .completed })
            precondition(bytes(a) == bytes(b) && bytes(a) == bytes(c), "Texture optimization preserves every output byte")
            precondition(bytes(a).contains { $0 > 0 }, "Pixel proof must contain visible text")
        }
        try output(["phase": "raster-optimization", "cases": 3, "tiles": tiles.count,
            "budget_fallback_preserves_coverage": true, "optimized_bytes": optimized.managedBytes])
    }

    static func lifecycle() throws {
        try optimization()
        let app=NSApplication.shared
        app.setActivationPolicy(.regular); app.finishLaunching()
        let window=NSWindow(contentRect:CGRect(x:0,y:0,width:512,height:256),styleMask:[.titled],backing:.buffered,defer:false)
        let surface=AtlasRetainedSurface(); surface.frame=CGRect(x:0,y:0,width:512,height:256)
        window.contentView=surface; window.makeKeyAndOrderFront(nil)
        let backing=Double(window.backingScaleFactor); surface.setBackingScale(backing)
        let text=CTLineCreateWithAttributedString(NSAttributedString(string:"let lifetime = immutable_source;",attributes:[
            .font:NSFont.monospacedSystemFont(ofSize:13,weight:.regular),.foregroundColor:Monokai.color("keyword")]))
        let tile=AtlasTextTile(path:"lifetime.rs",lines:Array(repeating:text,count:12),sourceRange:NSRange(location:0,length:1),part:0)
        tile.rect=CGRect(x:0,y:0,width:648,height:tile.height);tile.prepareRaster(pixelBudget:648*256)
        let metal=surface.metal!,revision=UUID(),scale=0.25/backing
        var coherentGPUFrames = 0
        surface.onPresentedFrame = { [weak surface] revision, scale, offset in
            guard let surface, !surface.gpuCoveredTiles.isEmpty else { return }
            guard let accepted = metal.acceptedFrame else { preconditionFailure("GPU camera receipt required") }
            precondition(accepted.revision == revision && accepted.scale == scale && accepted.offset == offset,
                "CPU overlay callback and Metal accept use the same camera")
            precondition(CATransaction.disableActions(), "Metal and CPU overlays share disabled-actions transaction")
            let world = surface.layer!.sublayers!.first { $0.zPosition == 1 }!
            precondition(world.affineTransform() == CGAffineTransform(a:scale,b:0,c:0,d:scale,tx:offset.x,ty:offset.y),
                "CPU world transform equals accepted Metal camera")
            coherentGPUFrames += 1
        }
        metal.rasterEnabled=false
        metal.prepare(tiles:[tile],revision:UUID())
        precondition(metal.rasterScene==nil,"Disabled overview must not prepare or allocate raster resources")
        metal.rasterEnabled=true
        func wait(_ label:String,_ condition:()->Bool) {
            let deadline=ProcessInfo.processInfo.systemUptime+30
            while !condition() && ProcessInfo.processInfo.systemUptime<deadline {
                surface.advanceMetalFrame()
                while let event=app.nextEvent(matching:.any,until:Date(timeIntervalSinceNow:0.002),inMode:.default,dequeue:true) {app.sendEvent(event)}
                _=RunLoop.main.run(mode:.default,before:Date(timeIntervalSinceNow:0.002))
            }
            precondition(condition(),label)
        }
        surface.update(tiles:[tile],revision:revision,scale:scale,offset:CGPoint(x:12,y:8),selectedPath:nil,hitPaths:[])
        wait("Raster-only frame must be accepted") { surface.gpuRasterTiles == [0] }
        precondition(surface.gpuGlyphTiles.isEmpty)
        try verifyAccepted(surface)
        weak var priorRaster=metal.rasterScene
        let submitted=metal.submissions
        surface.update(tiles:[tile],revision:revision,scale:scale,offset:CGPoint(x:12.25,y:8.75),selectedPath:nil,hitPaths:[])
        wait("Real command submitted before replacement") {metal.submissions>submitted}
        let replacement=UUID()
        surface.update(tiles:[tile],revision:replacement,scale:scale,offset:CGPoint(x:13,y:9),selectedPath:nil,hitPaths:[])
        wait("Replacement scene must retire old resources") {metal.acceptedFrame?.revision==replacement && !metal.hasWork}
        precondition(priorRaster==nil,"No retired revision texture leak")
        let image=tile.raster!
        let bitmap=CGContext(data:nil,width:image.width,height:image.height,bitsPerComponent:8,bytesPerRow:image.width*4,
            space:CGColorSpaceCreateDeviceRGB(),bitmapInfo:CGImageAlphaInfo.premultipliedLast.rawValue)!
        bitmap.draw(image,in:CGRect(x:0,y:0,width:image.width,height:image.height));tile.raster=bitmap.makeImage()!
        precondition(tile.raster !== image)
        surface.update(tiles:[tile],revision:replacement,scale:scale,offset:CGPoint(x:14,y:9),selectedPath:nil,hitPaths:[])
        precondition(surface.gpuRasterTiles.isEmpty,"Same-revision changed image must retain CPU coverage")
        let final=UUID()
        surface.update(tiles:[tile],revision:final,scale:scale,offset:CGPoint(x:14,y:9),selectedPath:nil,hitPaths:[])
        wait("New image revision admitted") {metal.acceptedFrame?.revision==final && surface.gpuRasterTiles == [0]}
        window.setContentSize(CGSize(width:576,height:288));surface.frame.size=CGSize(width:576,height:288)
        surface.setBackingScale(1)
        wait("Resized drawable and backing accepted") {metal.acceptedFrame?.size==CGSize(width:576,height:288) && metal.acceptedFrame?.backing==1 && !metal.hasWork}
        try verifyAccepted(surface)
        precondition(metal.acceptedFrame?.scale == scale && metal.acceptedFrame?.offset == CGPoint(x:14,y:9),
            "Last requested camera must settle exactly")
        precondition(coherentGPUFrames > 0)
        let acceptedBeforeDetach = coherentGPUFrames
        window.contentView=nil
        wait("Detached commands drain") {!metal.hasWork}
        precondition(surface.gpuCoveredTiles.isEmpty && metal.coveredTiles.isEmpty && metal.maximumInFlight<=2)
        precondition(coherentGPUFrames == acceptedBeforeDetach, "Detach cannot accept a stale GPU receipt")
        surface.onPresentedFrame = nil
        window.close()
        try output(["coherent_gpu_frames":coherentGPUFrames,"final_camera_exact":true,"phase":"raster-lifecycle-complete","revision_retired":true,"image_identity_fallback":true,
            "resize_backing_detach":true,"maximum_in_flight":metal.maximumInFlight,"failures":metal.failures])
    }
    static func run(tiles: [AtlasTextTile]) throws {
        _ = NSApplication.shared
        try optimization()
        try mixed()
        let device=MTLCreateSystemDefaultDevice()!, queue=device.makeCommandQueue()!
        var checked=0, controls=0
        let candidates=tiles.filter { $0.raster != nil && $0.rect.width>0 && $0.rect.height>0 }
        let count=min(12,candidates.count)
        for index in 0..<count {
            let original=candidates[index*max(0,candidates.count-1)/max(1,count-1)]
            guard let image=original.raster else { continue }
            let tile=AtlasTextTile(path:original.path,lines:[],sourceRange:NSRange(location:0,length:0),part:0)
            tile.rect=original.rect; tile.raster=image
            let scene=try AtlasMetalRasterRenderer(device:device,tiles:[tile],previousManagedBytes:0)
            precondition(scene.tiles.count==1)
            let sourceDensity=min(Double(image.width)/tile.rect.width,Double(image.height)/tile.rect.height)
            for fraction in [0.1,0.25,0.5,0.8] { for phase in [0.0,0.25,0.5,0.75] {
                let scale=fraction*sourceDensity
                let offset=CGPoint(x:12+phase-tile.rect.minX*scale,y:8+phase-tile.rect.minY*scale)
                let expected=reference(tile:tile,scale:scale,offset:offset,device:device,queue:queue)
                let repeatReference=reference(tile:tile,scale:scale,offset:offset,device:device,queue:queue)
                precondition(expected==repeatReference,"Original CA oracle must be deterministic")
                let target=texture(device), command=queue.makeCommandBuffer()!
                try scene.encode(into:target,commandBuffer:command,scale:scale,offset:offset,tileIDs:[0])
                command.commit(); command.waitUntilCompleted(); precondition(command.status == .completed)
                let actual=bytes(target), result=compare(actual,expected)
                try output(["phase":"raster-ca-parity","path":original.path,"scale":scale,"camera_phase":phase,
                    "max_byte_error":result.max,"normalized_ink_error":result.error,"reference_energy":result.energy])
                // Both pipelines quantize to 8-bit UNORM. At most one final rounding
                // code value is admitted; no tolerance proportional to blank area.
                precondition(result.energy>100 && result.max<=1 && result.error<=0.02,"Original CA colors, alpha and filtering")
                var opaque=actual
                for i in stride(from:3,to:opaque.count,by:4) { opaque[i]=255 }
                let flipped=(0..<256).reversed().flatMap { Array(actual[($0*2048)..<(($0+1)*2048)]) }
                let blank=[UInt8](repeating:0,count:actual.count)
                let shifted=Array(actual.dropFirst(4))+[UInt8](repeating:0,count:4)
                for wrong in [opaque,flipped,blank,shifted] {
                    precondition(compare(wrong,expected).max>1,"Negative control must fail the same pixel gate")
                    controls += 1
                }
                checked += 1
            }}
            let refused=try AtlasMetalRasterRenderer(device:device,tiles:[tile],previousManagedBytes:0,maximumManagedBytes:1)
            precondition(refused.tiles.isEmpty && refused.managedBytes==0)
            do { try scene.validate(scale:sourceDensity,tileIDs:[0]); preconditionFailure("Inadequate source must refuse") }
            catch AtlasMetalGlyphRenderer.Fallback.invalidGeometry {}
        }
        precondition(checked>0)
        try output(["phase":"raster-ca-complete","cases":checked,"negative_controls":controls])
    }
}
