import Foundation
import CoreGraphics
import ImageIO
import CoreText

@main struct AtlasPreparedTextTests {
    static func sharedLineStateReplay() {
        let text = "fn café(a\u{301}: &str) { שלום /* fj */ return 17; }"
        let styled = NSMutableAttributedString(string: text, attributes: [
            NSAttributedString.Key(kCTFontAttributeName as String): CTFontCreateWithName("Menlo" as CFString, 13, nil),
            NSAttributedString.Key(kCTForegroundColorAttributeName as String): CGColor(red: 0.8, green: 0.2, blue: 0.5, alpha: 0.8)
        ])
        for index in stride(from: 0, to: styled.length, by: 3) {
            styled.addAttribute(NSAttributedString.Key(kCTForegroundColorAttributeName as String),
                value: CGColor(red: Double(index % 5) / 5, green: 0.8, blue: 0.2, alpha: 0.65),
                range: NSRange(location: index, length: min(2, styled.length - index)))
        }
        let prepared = AtlasPreparedLine(CTLineCreateWithAttributedString(styled), source: text)!
        precondition(prepared.runs.count > 8)
        for transformed in [false, true] {
            let line = transformed ? AtlasPreparedLine(runs: prepared.runs.enumerated().map { index, run in
                AtlasPreparedRun(font: run.font, color: run.color,
                    matrix: index.isMultiple(of: 2) ? .identity : CGAffineTransform(a: 1, b: 0.1, c: 0.05, d: 1, tx: 0, ty: 0),
                    glyphs: run.glyphs, positions: run.positions)
            }) : prepared
            for scale in [0.2, 0.75, 1.0, 2.0, 8.0] {
                for x in [-4.25, 0.0, 71.3] {
                    func render(shared: Bool) -> Data {
                        let bitmap = CGContext(data: nil, width: 256, height: 96, bitsPerComponent: 8,
                            bytesPerRow: 1024, space: CGColorSpaceCreateDeviceRGB(),
                            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
                        bitmap.scaleBy(x: scale, y: scale)
                        bitmap.setFillColor(CGColor(gray: 0.4, alpha: 0.7))
                        let origin = CGPoint(x: x, y: 32.25)
                        let originalTransform = bitmap.ctm
                        if shared { line.draw(in: bitmap, origin: origin) }
                        else {
                            // Independent pre-optimization replay: restore all
                            // graphics state separately around every ordered run.
                            for run in line.runs {
                                bitmap.saveGState(); bitmap.setFillColor(run.color)
                                bitmap.translateBy(x: origin.x, y: origin.y); bitmap.scaleBy(x: 1, y: -1)
                                bitmap.textMatrix = run.matrix
                                let range = run.visibleGlyphRange(in: bitmap)
                                if !range.isEmpty {
                                    CTFontDrawGlyphs(run.font, Array(run.glyphs[range]), Array(run.positions[range]), range.count, bitmap)
                                }
                                bitmap.restoreGState()
                            }
                        }
                        precondition(bitmap.ctm == originalTransform)
                        bitmap.fill(CGRect(x: 4, y: 70, width: 20, height: 4))
                        return bitmap.makeImage()!.dataProvider!.data! as Data
                    }
                    precondition(render(shared: true) == render(shared: false),
                        "shared line state must preserve mixed-color, transformed, clipped glyph pixels and caller state")
                }
            }
        }
    }
    static func clippedGlyphReplay() {
        let font = CTFontCreateWithName("TimesNewRomanPS-ItalicMT" as CFString, 24, nil)
        let text = String(repeating: "fj café a\u{301} שלום  ", count: 12)
        let line = CTLineCreateWithAttributedString(NSAttributedString(string: text, attributes: [
            NSAttributedString.Key(kCTFontAttributeName as String): font,
            NSAttributedString.Key(kCTForegroundColorAttributeName as String): CGColor(gray: 1, alpha: 1)
        ]))
        let prepared = AtlasPreparedLine(line, source: text)!
        for scale in [0.2, 1.0, 2.0, 8.0] {
            for x in [-20.0, 0, 41, 140, 400, 2000, 10000] {
                func render(culled: Bool) -> (Data, Int) {
                    let context = CGContext(data: nil, width: 128, height: 128, bitsPerComponent: 8,
                        bytesPerRow: 512, space: CGColorSpaceCreateDeviceRGB(),
                        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
                    context.scaleBy(x: scale, y: scale)
                    context.translateBy(x: -x, y: 40)
                    var submitted = 0
                    for run in prepared.runs {
                        context.saveGState()
                        context.scaleBy(x: 1, y: -1)
                        context.textMatrix = run.matrix
                        context.setFillColor(run.color)
                        let range = culled ? run.visibleGlyphRange(in: context) : 0..<run.glyphs.count
                        submitted += range.count
                        if !range.isEmpty {
                            CTFontDrawGlyphs(run.font, Array(run.glyphs[range]), Array(run.positions[range]), range.count, context)
                        }
                        context.restoreGState()
                    }
                    return (context.makeImage()!.dataProvider!.data! as Data, submitted)
                }
                let full = render(culled: false), clipped = render(culled: true)
                precondition(full.0 == clipped.0, "glyph culling must preserve every clipped pixel including italic bearings and RTL")
                if x == 10000 { precondition(clipped.1 == 0, "fully offscreen runs submit zero glyphs") }
                if x == 400 && scale == 8 { precondition(clipped.1 < full.1, "horizontal detail clipping reduces submitted glyphs") }
            }
        }
    }
    static func capture(_ text: String) -> AtlasHighlightCapture {
        AtlasHighlightCapture(schema: "fcb.source-document/1", text: text,
            runs: text.isEmpty ? [] : [.init(start: "0", length: String(text.utf16.count), role: "keyword")])
    }
    static func pixels(_ tile: AtlasTextTile) -> Data {
        let bitmap = CGContext(data: nil, width: 648, height: Int(tile.height), bitsPerComponent: 8,
            bytesPerRow: 648 * 4, space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        bitmap.translateBy(x: 0, y: tile.height)
        bitmap.scaleBy(x: 1, y: -1)
        tile.draw(in: bitmap)
        return bitmap.makeImage()!.dataProvider!.data! as Data
    }
    static func scaledPixels(_ tile: AtlasTextTile, scale: Double, quality: CGInterpolationQuality? = nil,
                             smoothing: Bool? = nil, subpixel: Bool? = nil, flagMask: Int? = nil) -> Data {
        let width = Int(ceil(648 * scale)), height = Int(ceil(tile.height * scale))
        let bitmap = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
            bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        bitmap.translateBy(x: 0, y: Double(height))
        bitmap.scaleBy(x: scale, y: -scale)
        if let quality { bitmap.interpolationQuality = quality }
        if let smoothing { bitmap.setShouldSmoothFonts(smoothing) }
        if let subpixel { bitmap.setShouldSubpixelPositionFonts(subpixel) }
        if let flagMask {
            bitmap.setAllowsFontSubpixelPositioning(flagMask & 1 != 0)
            bitmap.setShouldSubpixelPositionFonts(flagMask & 2 != 0)
            bitmap.setAllowsFontSubpixelQuantization(flagMask & 4 != 0)
            bitmap.setShouldSubpixelQuantizeFonts(flagMask & 8 != 0)
        }
        tile.draw(in: bitmap)
        return bitmap.makeImage()!.dataProvider!.data! as Data
    }
    static func variantPixels(_ original: AtlasTextTile, _ prepared: AtlasTextTile, scale: Double, matrixFont: Bool) -> Data {
        let width = Int(ceil(648 * scale)), height = Int(ceil(original.height * scale))
        let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
            bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
        context.translateBy(x: 0, y: Double(height)); context.scaleBy(x: scale, y: -scale)
        context.textMatrix = CGAffineTransform(scaleX: 1, y: -1)
        context.textPosition = CGPoint(x: 4, y: 12)
        CTLineDraw(original.header!, context)
        for (index, line) in prepared.preparedLines!.enumerated() {
            let origin = CGPoint(x: 4, y: 34 + Double(index) * AtlasTextTile.lineHeight)
            for run in line.runs {
                context.saveGState(); context.setFillColor(run.color)
                let positions = run.positions.map { CGPoint(x: origin.x + $0.x, y: origin.y - $0.y) }
                if matrixFont {
                    var matrix = CTFontGetMatrix(run.font).concatenating(CGAffineTransform(scaleX: 1, y: -1))
                    let font = CTFontCreateCopyWithAttributes(run.font, CTFontGetSize(run.font), &matrix, nil)
                    CTFontDrawGlyphs(font, run.glyphs, positions, run.glyphs.count, context)
                } else {
                    context.textMatrix = CGAffineTransform(scaleX: 1, y: -1)
                    context.setFont(CTFontCopyGraphicsFont(run.font, nil)); context.setFontSize(CTFontGetSize(run.font))
                    context.showGlyphs(run.glyphs, at: positions)
                }
                context.restoreGState()
            }
        }
        return context.makeImage()!.dataProvider!.data! as Data
    }
    static func diagnose(_ a: AtlasTextTile, _ b: AtlasTextTile, scale: Double, label: String,
                         quality: CGInterpolationQuality? = nil, smoothing: Bool? = nil, subpixel: Bool? = nil,
                         flagMask: Int? = nil, override: Data? = nil) throws {
        let left = scaledPixels(a, scale: scale)
        let right = override ?? scaledPixels(b, scale: scale, quality: quality, smoothing: smoothing, subpixel: subpixel, flagMask: flagMask)
        let width = Int(ceil(648 * scale)), height = Int(ceil(a.height * scale))
        var count = 0, minX = width, minY = height, maxX = -1, maxY = -1, maximumDelta = 0
        for index in 0..<(width * height) {
            let offset = index * 4
            if left[offset..<offset + 4] != right[offset..<offset + 4] {
                count += 1
                let x = index % width, y = index / width
                minX = min(minX, x); minY = min(minY, y); maxX = max(maxX, x); maxY = max(maxY, y)
                for channel in 0..<4 { maximumDelta = max(maximumDelta, abs(Int(left[offset + channel]) - Int(right[offset + channel]))) }
            }
        }
        let folder = URL(fileURLWithPath: "/private/tmp").appendingPathComponent("fcb-glyph-diagnostic-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: false)
        for (name, data) in [("original", left), ("replay", right)] {
            let image = CGImage(width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32,
                bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
                provider: CGDataProvider(data: data as CFData)!, decode: nil, shouldInterpolate: false, intent: .defaultIntent)!
            let output = folder.appendingPathComponent(name + ".png")
            let destination = CGImageDestinationCreateWithURL(output as CFURL, "public.png" as CFString, 1, nil)!
            CGImageDestinationAddImage(destination, image, nil)
            precondition(CGImageDestinationFinalize(destination), "retain actual diagnostic pixels")
        }
        let message = "GLYPH_DIAGNOSTIC label=\(label) scale=\(scale) pixels=\(count) bbox=\(minX),\(minY),\(maxX),\(maxY) max_channel_delta=\(maximumDelta) folder=\(folder.path)\n"
        FileHandle.standardError.write(Data(message.utf8))
    }
    static func realCache() throws {
        let base = URL(fileURLWithPath: "/private/tmp", isDirectory: true).appendingPathComponent("fcb-native-cache-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: false)
        let path = base.appendingPathComponent("file.rs")
        try Data("fn first() {}\n".utf8).write(to: path)
        let disk = base.appendingPathComponent("cache").path
        var first: AtlasProjectCache? = AtlasProjectCache(root: base.path, cacheDirectory: disk)
        let a = first!.document(path: "file.rs", fallback: { preconditionFailure("real cache source route required") })!
        for tile in a.tiles { tile.prepareRaster(pixelBudget: 8192) }
        first!.finishRefresh(documents: ["file.rs": a])
        precondition(first!.rebuilt == 1 && first!.saved == 1)
        first!.beginRefresh()
        let hot = first!.document(path: "file.rs", fallback: { nil })!
        precondition(first!.ramHits == 1 && first!.rebuilt == 0 && hot.tiles[0] === a.tiles[0])
        first = nil
        let second = AtlasProjectCache(root: base.path, cacheDirectory: disk)
        let cold = second.document(path: "file.rs", fallback: { nil })!
        precondition(second.diskHits == 1 && second.rebuilt == 0)
        precondition(second.reshapedColorLines == 0, "ordinary source disk hit does not reshape color lines")
        precondition(pixels(a.tiles[0]) == pixels(cold.tiles[0]))
        second.beginRefresh(glyphLimit: 0)
        precondition(second.document(path: "file.rs", fallback: { nil }) == nil,
                     "RAM hit cannot bypass exhausted project admission")
        second.invalidateFonts()
        second.beginRefresh(glyphLimit: 0)
        precondition(second.document(path: "file.rs", fallback: { nil }) == nil && second.rebuilt == 0,
                     "disk admission failure cannot fall through to unbounded shaping")
        second.beginRefresh()
        _ = second.document(path: "file.rs", fallback: { nil })!
        precondition(second.ramHits == 0, "font environment invalidation discards shaped RAM hits")
        try Data("fn other() {}\n".utf8).write(to: path)
        second.beginRefresh(glyphLimit: 0)
        precondition(second.document(path: "file.rs", fallback: { nil }) == nil && second.rebuilt == 0,
                     "changed-source miss must honor admission before shaping")
        second.beginRefresh()
        let changed = second.document(path: "file.rs", fallback: { nil })!
        precondition(second.rebuilt == 1 && changed.source.text == "fn other() {}\n")
    }
    static func workerPreparedCache() throws {
        let base = URL(fileURLWithPath: "/private/tmp", isDirectory: true)
            .appendingPathComponent("fcb-worker-prepared-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: false)
        let source = base.appendingPathComponent("file.rs")
        try Data("fn worker() {}\n".utf8).write(to: source)
        let disk = base.appendingPathComponent("cache").path
        let token = AtlasSearchCancellation()
        var cache: AtlasProjectCache? = AtlasProjectCache(root: base.path, cacheDirectory: disk)
        precondition(cache!.sourceHandle != 0)
        cache!.beginRefresh()
        let packet = try AtlasProjectWorker.source(path: source.path,
            handle: cache!.sourceHandle, cancellation: token)!
        let key = cache!.preparedKey(path: "file.rs", sourceKey: packet.key)!
        let missing = try AtlasProjectWorker.artifact(handle: cache!.sourceHandle,
            key: key, limit: AtlasBinaryWriter.limit, cancellation: token)
        precondition(missing == nil)
        let first = cache!.document(path: "file.rs", packet: packet, artifact: nil)!
        precondition(first.source.text == "fn worker() {}\n" && cache!.rebuilt == 1)
        for tile in first.tiles { tile.prepareRaster(pixelBudget: 8192) }
        cache!.finishRefresh(documents: ["file.rs": first])
        precondition(cache!.saved == 1)
        cache!.beginRefresh()
        let hot = cache!.document(path: "file.rs", packet: packet, artifact: nil)!
        precondition(cache!.ramHits == 1 && hot.tiles[0] === first.tiles[0])
        cache = nil
        let reopened = AtlasProjectCache(root: base.path, cacheDirectory: disk)
        reopened.beginRefresh()
        let coldPacket = try AtlasProjectWorker.source(path: source.path,
            handle: reopened.sourceHandle, cancellation: token)!
        let coldKey = reopened.preparedKey(path: "file.rs", sourceKey: coldPacket.key)!
        let artifact = try AtlasProjectWorker.artifact(handle: reopened.sourceHandle,
            key: coldKey, limit: AtlasBinaryWriter.limit, cancellation: token)
        let cold = reopened.document(path: "file.rs", packet: coldPacket, artifact: artifact)!
        precondition(reopened.diskHits == 1 && reopened.rebuilt == 0)
        precondition(cold.source.text == first.source.text)
    }
    static func overviewCache() throws {
        let base = URL(fileURLWithPath: "/private/tmp", isDirectory: true).appendingPathComponent("fcb-overview-cache-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: base, withIntermediateDirectories: false)
        try Data("fn highlight() { let café = 123; }\n".utf8).write(to: base.appendingPathComponent("file.rs"))
        let directory = base.appendingPathComponent("cache").path
        var cache: AtlasProjectCache? = AtlasProjectCache(root: base.path, cacheDirectory: directory)
        let original = cache!.document(path: "file.rs", fallback: { nil })!
        let tile = original.tiles[0]
        tile.rect = CGRect(x: 0, y: 0, width: AtlasTextTile.width, height: tile.height)
        tile.prepareRaster(pixelBudget: 128)
        cache!.finishRefresh(documents: ["file.rs": original])
        cache!.prepareOverview(documents: ["file.rs": original])
        precondition(cache!.overviewRasters == 1 && cache!.overviewSaved == 1 && cache!.overviewDiskHits == 0)
        let expected = tile.raster!.dataProvider!.data! as Data
        let dimensions = CGSize(width: tile.raster!.width, height: tile.raster!.height)
        precondition(tile.raster!.width > 128, "higher-density addon replaces tiny base preview")
        let image = tile.raster!
        cache!.beginRefresh()
        let hot = cache!.document(path: "file.rs", fallback: { nil })!
        cache!.finishRefresh(documents: ["file.rs": hot])
        cache!.prepareOverview(documents: ["file.rs": hot])
        precondition(cache!.overviewRAMHits == 1 && cache!.overviewRasters == 0 && hot.tiles[0].raster === image)
        // The Rust cache intentionally permits only one writer. Reopen after
        // releasing the first owner, exactly as a subsequent app launch does.
        cache = nil
        let reopened = AtlasProjectCache(root: base.path, cacheDirectory: directory)
        let restored = reopened.document(path: "file.rs", fallback: { nil })!
        precondition(restored.tiles[0].raster!.width * restored.tiles[0].raster!.height <= 128,
                     "base source archive is unchanged by addon publication")
        restored.tiles[0].rect = tile.rect
        reopened.finishRefresh(documents: ["file.rs": restored])
        reopened.prepareOverview(documents: ["file.rs": restored])
        precondition(reopened.overviewDiskHits == 1 && reopened.overviewRasters == 0 && reopened.rebuilt == 0)
        precondition((restored.tiles[0].raster!.dataProvider!.data! as Data) == expected, "PNG restoration preserves exact opaque RGBA pixels")
        let encoded = try AtlasOverviewArchive.encode(restored.tiles)
        precondition((try? AtlasOverviewArchive.decode(encoded, sizes: [CGSize(width: dimensions.width + 1, height: dimensions.height)])) == nil)
        let oldKey = try AtlasOverviewArchive.key(sourceKey: "captured", tiles: restored.tiles, sizes: [dimensions])
        restored.tiles[0].rect.size.width *= 2
        let changedKey = try AtlasOverviewArchive.key(sourceKey: "captured", tiles: restored.tiles, sizes: [dimensions])
        precondition(oldKey != changedKey, "parcel geometry participates in addon identity")
        reopened.prepareOverview(documents: ["file.rs": restored])
        precondition(reopened.overviewRasters == 1 && reopened.overviewDiskHits == 0, "geometry change cannot reuse stale addon")
        reopened.prepareOverview(documents: ["file.rs": restored], pixelLimit: 128)
        precondition(restored.tiles[0].raster!.width * restored.tiles[0].raster!.height <= 128, "project pixel budget bounds decoded residency")

        // Exercise the production retry seam with no successful base publication:
        // addon exists, but the original key is still absent (as after a failed
        // initial write). finishRefresh must not encode addon pixels under it.
        let retryDirectory = base.appendingPathComponent("retry-cache").path
        var retry: AtlasProjectCache? = AtlasProjectCache(root: base.path, cacheDirectory: retryDirectory)
        let unpublished = retry!.document(path: "file.rs", fallback: { nil })!
        unpublished.tiles[0].rect = tile.rect
        retry!.prepareOverview(documents: ["file.rs": unpublished])
        precondition(retry!.overviewSaved == 1)
        retry!.finishRefresh(documents: ["file.rs": unpublished])
        precondition(retry!.saved == 0, "addon-owned rasters cannot be published as base archive on retry")
        retry = nil
        let afterRetry = AtlasProjectCache(root: base.path, cacheDirectory: retryDirectory)
        let reconstructed = afterRetry.document(path: "file.rs", fallback: { nil })!
        precondition(afterRetry.rebuilt == 1 && afterRetry.diskHits == 0 && reconstructed.tiles[0].raster == nil,
                     "unpublished base stays absent and reconstructs cleanly on a later load")
        let widePixel = Data(repeating: 255, count: 8)
        let wideImage = CGImage(width: 1, height: 1, bitsPerComponent: 16, bitsPerPixel: 64, bytesPerRow: 8,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue).union(.byteOrder16Big),
            provider: CGDataProvider(data: widePixel as CFData)!, decode: nil, shouldInterpolate: false, intent: .defaultIntent)!
        let widePNG = NSMutableData()
        let writer = CGImageDestinationCreateWithData(widePNG, "public.png" as CFString, 1, nil)!
        CGImageDestinationAddImage(writer, wideImage, nil)
        precondition(CGImageDestinationFinalize(writer))
        let wideSource = CGImageSourceCreateWithData(widePNG, nil)!
        let wideProps = CGImageSourceCopyPropertiesAtIndex(wideSource, 0, nil)! as NSDictionary
        precondition(wideProps[kCGImagePropertyDepth] as? Int == 16, "negative fixture really is a 16-bit PNG")
        var wideBundle = AtlasBinaryWriter()
        try wideBundle.string("FCBOV1"); try wideBundle.integer(UInt64(1))
        try wideBundle.integer(UInt64(widePNG.length)); try wideBundle.bytes(widePNG as Data)
        precondition((try? AtlasOverviewArchive.decode(wideBundle.data, sizes: [CGSize(width: 1, height: 1)])) == nil,
                     "wide-component PNG must refuse before image decoding exceeds RGBA8 budget")
    }
    static func fontFingerprintSurvivesShaping() throws {
        let font = CTFontCreateWithName("CourierNewPSMT" as CFString, 13, nil)
        let before = try AtlasPreparedFonts().fingerprint(font)
        let text = NSAttributedString(string: "ffi café a\u{301} עברית", attributes: [
            NSAttributedString.Key(kCTFontAttributeName as String): font
        ])
        let line = CTLineCreateWithAttributedString(text)
        precondition(CTLineGetGlyphCount(line) > 0, "exercise actual CoreText shaping")
        let after = try AtlasPreparedFonts().fingerprint(font)
        let recreated = CTFontCreateWithName("CourierNewPSMT" as CFString, 13, nil)
        let reopened = try AtlasPreparedFonts().fingerprint(recreated)
        precondition(before == after && after == reopened, "font resource identity must survive lazy CoreText shaping")
    }
    static func sharedFontSeedVariants() throws {
        let font = CTFontCreateWithName("Menlo-Regular" as CFString, 13, nil)
        let decoder = AtlasPreparedFonts()
        let expected = try AtlasPreparedFonts().fingerprint(font)
        for index in 0..<160 {
            let seed = "source seed \(index)"
            let encoded = try AtlasPreparedFonts().encode(font, fallbackText: seed)
            let restored = try decoder.decode(encoded)
            precondition(restored.fallbackText == seed, "shared face must retain each run's own source seed")
            precondition(CTFontCopyPostScriptName(restored.font) == CTFontCopyPostScriptName(font))
            let actual = try AtlasPreparedFonts().fingerprint(restored.font)
            precondition(actual == expected)
        }
    }
    static func main() throws {
        sharedLineStateReplay()
        clippedGlyphReplay()
        try overviewCache()
        try fontFingerprintSurvivesShaping()
        try sharedFontSeedVariants()
        try realCache()
        try workerPreparedCache()

        let fonts = AtlasPreparedFonts()
        let key = String(repeating: "a", count: 64)
        for (caseIndex, text) in ["fn main() {\n\tlet value = 123; // source\n}\n", "", "\n\n",
                     "café e\u{301} שלום مرحبا 中文 🙂 👨‍👩‍👧‍👦 ffi\r\n",
                     String(repeating: "long source line; ", count: 1000)].enumerated() {
            let original = AtlasDocument(path: "test.rs", capture: capture(text))!
            for tile in original.tiles { tile.prepareRaster(pixelBudget: 8192) }
            let encoded: Data
            let loaded: AtlasDocument
            do { encoded = try AtlasDocumentArchive.encode(original, key: key, fonts: fonts) }
            catch { fatalError("case \(caseIndex) encode: \(error)") }
            do { loaded = try AtlasDocumentArchive.decode(encoded, path: "test.rs", key: key, fonts: AtlasPreparedFonts()) }
            catch { fatalError("case \(caseIndex) decode: \(error)") }
            precondition(original.source.text == loaded.source.text)
            precondition(original.tiles.count == loaded.tiles.count)
            precondition(loaded.restoredColorLineCount == (caseIndex == 3 ? 1 : 0),
                         "only the actual color-font source line is reconstructed")
            if caseIndex == 3 {
                let counts = original.preparationCounts
                precondition((try? AtlasDocumentArchive.decode(encoded, path: "test.rs", key: key,
                    fonts: AtlasPreparedFonts(), glyphLimit: counts.glyphs)) == nil,
                    "color CTLine retention must charge beyond saved glyph arrays")
                precondition((try? AtlasDocumentArchive.decode(encoded, path: "test.rs", key: key,
                    fonts: AtlasPreparedFonts(), runLimit: counts.runs)) == nil,
                    "color CTLine retention must charge beyond saved runs")
            }
            let reencoded = try AtlasDocumentArchive.encode(loaded, key: key, fonts: AtlasPreparedFonts())
            let reopened = try AtlasDocumentArchive.decode(reencoded, path: "test.rs", key: key,
                                                           fonts: AtlasPreparedFonts())
            precondition(reopened.source.text == original.source.text && reopened.tiles.count == original.tiles.count)
            precondition(reopened.restoredColorLineCount == loaded.restoredColorLineCount)
            for (a, b) in zip(original.tiles, reopened.tiles) {
                precondition(pixels(a) == pixels(b), "second-generation archive preserves exact glyph pixels")
            }
            for (a, b) in zip(original.tiles, loaded.tiles) {
                precondition(a.sourceRange == b.sourceRange && a.lineCount == b.lineCount)
                precondition((a.raster!.dataProvider!.data! as Data) == (b.raster!.dataProvider!.data! as Data),
                             "persisted overview pixels are exact")
                precondition(pixels(a) == pixels(b), "owned glyph replay matches CoreText pixels")
                for scale in [0.2, 0.5, 1.7, 2.0] {
                    if scaledPixels(a, scale: scale) != scaledPixels(b, scale: scale) {
                        try diagnose(a, b, scale: scale, label: "case\(caseIndex)")
                        for (index, sample) in ["café e\u{301}", "שלום", "مرحبا", "中文", "🙂", "👨‍👩‍👧‍👦", "ffi"].enumerated() {
                            let probe = AtlasDocument(path: "probe.rs", capture: capture(sample))!
                            for tile in probe.tiles { tile.prepareRaster(pixelBudget: 8192) }
                            let bytes = try AtlasDocumentArchive.encode(probe, key: key, fonts: AtlasPreparedFonts())
                            let restored = try AtlasDocumentArchive.decode(bytes, path: "probe.rs", key: key, fonts: AtlasPreparedFonts())
                            try diagnose(probe.tiles[0], restored.tiles[0], scale: scale, label: "component\(index)")
                            let originalTile = probe.tiles[0]
                            let direct = AtlasTextTile(path: "probe.rs",
                                preparedLines: originalTile.lines.map { AtlasPreparedLine($0, source: sample)! },
                                header: AtlasPreparedLine(originalTile.header!, source: "probe.rs")!,
                                sourceRange: originalTile.sourceRange)
                            try diagnose(originalTile, direct, scale: scale, label: "direct\(index)")
                            if index == 4 || index == 5 {
                                for mask in 0..<16 {
                                    try diagnose(originalTile, direct, scale: scale, label: "mask\(index)_\(mask)", flagMask: mask)
                                }
                                for matrixFont in [false, true] {
                                    try diagnose(originalTile, direct, scale: scale, label: "api\(index)_\(matrixFont)",
                                        override: variantPixels(originalTile, direct, scale: scale, matrixFont: matrixFont))
                                }
                                for smoothing in [false, true] {
                                    for subpixel in [false, true] {
                                        try diagnose(originalTile, direct, scale: scale,
                                            label: "flags\(index)_\(smoothing)_\(subpixel)", smoothing: smoothing, subpixel: subpixel)
                                    }
                                }
                                for quality: CGInterpolationQuality in [.none, .low, .medium, .high] {
                                    try diagnose(originalTile, direct, scale: scale,
                                                 label: "quality\(index)_\(quality.rawValue)", quality: quality)
                                }
                                for (label, tile) in [("direct", direct), ("restored", restored.tiles[0])] {
                                    for run in tile.preparedLines!.flatMap(\.runs) {
                                        let description = "FONT_DIAGNOSTIC component=\(index) owner=\(label) descriptor=\(CTFontDescriptorCopyAttributes(CTFontCopyFontDescriptor(run.font))) traits=\(CTFontCopyTraits(run.font)) matrix=\(CTFontGetMatrix(run.font))\n"
                                        FileHandle.standardError.write(Data(description.utf8))
                                    }
                                }
                            }
                        }
                    }
                    precondition(scaledPixels(a, scale: scale) == scaledPixels(b, scale: scale),
                                 "case \(caseIndex): owned replay differs at scale \(scale)")
                }
            }
            precondition((try? AtlasDocumentArchive.decode(encoded, path: "other.rs", key: key, fonts: fonts)) == nil)
            precondition((try? AtlasDocumentArchive.decode(encoded, path: "test.rs", key: "wrong", fonts: fonts)) == nil)
            precondition((try? AtlasDocumentArchive.decode(Data(encoded.dropLast()), path: "test.rs", key: key, fonts: fonts)) == nil)
            var trailing = encoded; trailing.append(0)
            precondition((try? AtlasDocumentArchive.decode(trailing, path: "test.rs", key: key, fonts: fonts)) == nil)
            if !original.tiles.isEmpty {
                precondition((try? AtlasDocumentArchive.decode(encoded, path: "test.rs", key: key,
                    fonts: fonts, pixelLimit: 0)) == nil, "global pixel admission precedes image allocation")
                precondition((try? AtlasDocumentArchive.decode(encoded, path: "test.rs", key: key,
                    fonts: fonts, glyphLimit: 0)) == nil, "global glyph admission precedes arrays")
                precondition((try? AtlasDocumentArchive.decode(encoded, path: "test.rs", key: key,
                    fonts: fonts, runLimit: 0)) == nil, "global run admission precedes run allocation")
            }
        }
        print("AtlasPreparedText: exact source, glyph and raster replay; corrupt/key refusal; real RAM/disk/delta cache passed")
    }
}
