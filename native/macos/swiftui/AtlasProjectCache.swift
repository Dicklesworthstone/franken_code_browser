import Foundation
import AppKit
import CoreText
import CryptoKit
import ImageIO

@_silgen_name("fcb_source_cache_open") private func cacheOpen(_ root: UnsafePointer<CChar>) -> UInt64
@_silgen_name("fcb_source_cache_close") private func cacheClose(_ handle: UInt64) -> Bool
@_silgen_name("fcb_source_document_cached") private func cachedSource(_ handle: UInt64,
    _ path: UnsafePointer<CChar>, _ key: UnsafeMutablePointer<UInt8>) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_source_cache_get") private func cacheGet(_ handle: UInt64,
    _ key: UnsafePointer<CChar>, _ length: UnsafeMutablePointer<UInt64>) -> UnsafeMutablePointer<UInt8>?
@_silgen_name("fcb_source_cache_put") private func cachePut(_ handle: UInt64,
    _ key: UnsafePointer<CChar>, _ bytes: UnsafePointer<UInt8>, _ length: UInt64) -> Bool
@_silgen_name("fcb_source_cache_repair") private func cacheRepair(_ handle: UInt64,
    _ key: UnsafePointer<CChar>, _ bytes: UnsafePointer<UInt8>, _ length: UInt64) -> Bool
@_silgen_name("fcb_source_cache_free") private func cacheFree(_ bytes: UnsafeMutablePointer<UInt8>, _ length: UInt64)
@_silgen_name("fcb_source_cache_stats") private func cacheStats(_ handle: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_free_string") private func sourceFree(_ text: UnsafeMutablePointer<CChar>)

/// One explicit project owner. Disk authority/quota/publication live in Rust;
/// this object only serializes Apple text/bitmap resources and retains hot ones.
final class AtlasProjectCache {
    static let layoutVersion = "fcb-native-prepared/6;cgfont-tables1;width648;wrap640;rows80;font13;line16;tabs62.64;monokai1"
    let root: String
    private let handle: UInt64
    private var fonts = AtlasPreparedFonts()
    private var hot: [String: AtlasDocument] = [:]
    private var keys: [String: String] = [:]
    private var persisted = Set<String>()
    private var rejected = Set<String>()
    private var overviewKeys: [String: String] = [:]
    private var overviewOwners: [String: ObjectIdentifier] = [:]
    private(set) var overviewDiskHits = 0, overviewRAMHits = 0, overviewRasters = 0, overviewSaved = 0
    private var environment = ""
    private var fontObservers: [(NotificationCenter, NSObjectProtocol)] = []
    private var remainingPixels = 16 * 1024 * 1024
    private var remainingGlyphs = 16 * 1024 * 1024
    private var remainingRuns = 1024 * 1024
    private var remainingArtifactBytes = 512 * 1024 * 1024
    private(set) var ramHits = 0
    private(set) var diskHits = 0
    private(set) var rebuilt = 0
    private(set) var saved = 0
    private(set) var reshapedColorLines = 0
    private(set) var rejectionReasons: [String: Int] = [:]

    var engineStatsJSON: String? {
        guard let pointer = cacheStats(handle) else { return nil }
        defer { sourceFree(pointer) }
        return String(cString: pointer)
    }

    init(root: String, cacheDirectory: String) {
        self.root = root
        handle = cacheDirectory.withCString { cacheOpen($0) }
        environment = Self.fontEnvironment()
        let name = Notification.Name(kCTFontManagerRegisteredFontsChangedNotification as String)
        for center in [NotificationCenter.default, DistributedNotificationCenter.default()] {
            let token = center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                self?.invalidateFonts()
            }
            fontObservers.append((center, token))
        }
    }
    deinit {
        for (center, token) in fontObservers { center.removeObserver(token) }
        if handle != 0 { _ = cacheClose(handle) }
    }

    private static func fontEnvironment() -> String {
        var hash = SHA256()
        let names = (CTFontManagerCopyAvailablePostScriptNames() as? [String] ?? []).sorted()
        hash.update(data: Data((names.joined(separator: "\u{0}") + Locale.preferredLanguages.joined(separator: "\u{0}")).utf8))
        // Registry/file metadata invalidates usual font installation changes;
        // actual used font tables are independently checked by artifact decode.
        let urls = (CTFontManagerCopyAvailableFontURLs() as? [URL] ?? []).sorted { $0.path < $1.path }
        for url in urls {
            let attrs = try? FileManager.default.attributesOfItem(atPath: url.path)
            let stamp = "\(url.path)\u{0}\(attrs?[.size] ?? "")\u{0}\(attrs?[.modificationDate] ?? "")\u{0}\(attrs?[.systemFileNumber] ?? "")"
            hash.update(data: Data(stamp.utf8))
        }
        return hash.finalize().map { String(format: "%02x", $0) }.joined()
    }

    func invalidateFonts() {
        hot = [:]; keys = [:]; persisted = []; rejected = []
        overviewKeys = [:]; overviewOwners = [:]
        fonts = AtlasPreparedFonts()
        environment = Self.fontEnvironment()
    }

    static func defaultDirectory(root: String) -> String? {
        guard let base = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask).first else { return nil }
        let parent = base.appendingPathComponent("dev.frankencode.browser/prepared-v1", isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: parent, withIntermediateDirectories: true,
                attributes: [.posixPermissions: 0o700])
        } catch { return nil }
        let id = SHA256.hash(data: Data(root.utf8)).map { String(format: "%02x", $0) }.joined()
        return parent.resolvingSymlinksInPath().appendingPathComponent(id, isDirectory: true).path
    }

    func beginRefresh(glyphLimit: Int = 16 * 1024 * 1024, runLimit: Int = 1024 * 1024,
                      pixelLimit: Int = 16 * 1024 * 1024) {
        let current = Self.fontEnvironment()
        if current != environment { invalidateFonts() }
        ramHits = 0; diskHits = 0; rebuilt = 0; saved = 0; reshapedColorLines = 0; rejectionReasons = [:]
        remainingPixels = max(0, min(16 * 1024 * 1024, pixelLimit))
        remainingGlyphs = max(0, min(16 * 1024 * 1024, glyphLimit))
        remainingRuns = max(0, min(1024 * 1024, runLimit)); remainingArtifactBytes = 512 * 1024 * 1024
    }

    /// The handle is borrowed only while this owner is retained by the project
    /// load and its worker lease. Project source/cache reads happen off-main.
    var sourceHandle: UInt64 { handle }

    func preparedKey(path: String, sourceKey: [UInt8]?) -> String? {
        guard handle != 0, let sourceKey, sourceKey.count == 32 else { return nil }
        var hash = SHA256()
        hash.update(data: Data(sourceKey))
        hash.update(data: Data((Self.layoutVersion + "\u{0}" + environment + "\u{0}" + path + "\u{0}"
            + ProcessInfo.processInfo.operatingSystemVersionString + "\u{0}"
            + (CTFontCopyPostScriptName(NSFont.monospacedSystemFont(ofSize: 13, weight: .regular) as CTFont) as String)).utf8))
        return hash.finalize().map { String(format: "%02x", $0) }.joined()
    }

    /// Consumes immutable bytes fetched by AtlasProjectIO. Font shaping and
    /// bitmap ownership stay on the main actor, exactly as in the legacy path.
    func document(path: String, packet: AtlasProjectSourcePacket, artifact: Data?) -> AtlasDocument? {
        if handle == 0 {
            guard let capture = try? JSONDecoder().decode(AtlasHighlightCapture.self, from: packet.json),
                  let document = AtlasDocument(path: path, capture: capture,
                    glyphLimit: remainingGlyphs, runLimit: remainingRuns), admit(document) else { return nil }
            rebuilt += 1
            return document
        }
        guard let key = preparedKey(path: path, sourceKey: packet.key) else { return nil }
        if keys[path] == key, let document = hot[path] {
            guard admit(document) else { return nil }
            ramHits += 1
            return document
        }
        if let artifact {
            guard artifact.count <= min(AtlasBinaryWriter.limit, remainingArtifactBytes) else { return nil }
            do {
                let document = try AtlasDocumentArchive.decode(artifact, path: path, key: key,
                    fonts: fonts, glyphLimit: remainingGlyphs, pixelLimit: remainingPixels,
                    runLimit: remainingRuns, onColorLineRestore: { self.reshapedColorLines += 1 })
                remainingArtifactBytes -= artifact.count
                guard admit(document) else { return nil }
                diskHits += 1
                persisted.insert(key)
                hot[path] = document; keys[path] = key
                return document
            } catch AtlasPreparationError.limit {
                return nil
            } catch {
                rejected.insert(key)
                let reason = String(describing: error)
                if rejectionReasons[reason] != nil || rejectionReasons.count < 16 {
                    rejectionReasons[reason, default: 0] += 1
                } else { rejectionReasons["other", default: 0] += 1 }
            }
        }
        guard let capture = try? JSONDecoder().decode(AtlasHighlightCapture.self, from: packet.json),
              let document = AtlasDocument(path: path, capture: capture,
                glyphLimit: remainingGlyphs, runLimit: remainingRuns), admit(document) else { return nil }
        rebuilt += 1
        hot[path] = document; keys[path] = key
        return document
    }

    func document(path: String, fallback: () -> AtlasHighlightCapture?) -> AtlasDocument? {
        guard handle != 0 else {
            guard let capture = fallback(), let document = AtlasDocument(path: path, capture: capture,
                glyphLimit: remainingGlyphs, runLimit: remainingRuns), admit(document) else { return nil }
            rebuilt += 1; return document
        }
        var sourceKey = [UInt8](repeating: 0, count: 32)
        let full = (root as NSString).appendingPathComponent(path)
        guard let response = full.withCString({ name in
            sourceKey.withUnsafeMutableBufferPointer { cachedSource(handle, name, $0.baseAddress!) }
        }) else { return nil }
        defer { sourceFree(response) }
        var hash = SHA256()
        hash.update(data: Data(sourceKey))
        hash.update(data: Data((Self.layoutVersion + "\u{0}" + environment + "\u{0}" + path + "\u{0}"
            + ProcessInfo.processInfo.operatingSystemVersionString + "\u{0}"
            + (CTFontCopyPostScriptName(NSFont.monospacedSystemFont(ofSize: 13, weight: .regular) as CTFont) as String)).utf8))
        let key = hash.finalize().map { String(format: "%02x", $0) }.joined()
        if keys[path] == key, let document = hot[path] {
            guard admit(document) else { return nil }
            ramHits += 1; return document
        }
        var length: UInt64 = 0
        if let bytes = key.withCString({ cacheGet(handle, $0, &length) }) {
            defer { cacheFree(bytes, length) }
            guard length <= UInt64(min(AtlasBinaryWriter.limit, remainingArtifactBytes)) else { return nil }
            do {
                let document = try AtlasDocumentArchive.decode(Data(bytes: bytes, count: Int(length)),
                    path: path, key: key, fonts: fonts, glyphLimit: remainingGlyphs,
                    pixelLimit: remainingPixels, runLimit: remainingRuns,
                    onColorLineRestore: { self.reshapedColorLines += 1 })
                remainingArtifactBytes -= Int(length)
                guard admit(document) else { return nil }
                diskHits += 1
                persisted.insert(key)
                hot[path] = document; keys[path] = key
                return document
                } catch AtlasPreparationError.limit {
                    // Resource refusal is not corruption and cannot repair a valid entry.
                    return nil
                } catch {
                    rejected.insert(key)
                    let reason = String(describing: error)
                    if rejectionReasons[reason] != nil || rejectionReasons.count < 16 {
                        rejectionReasons[reason, default: 0] += 1
                    } else { rejectionReasons["other", default: 0] += 1 }
                }
        }
        // JSON conversion, lexed runs, and native shaping are cold-path work.
        let data = Data(bytes: response, count: strlen(response))
        guard let capture = try? JSONDecoder().decode(AtlasHighlightCapture.self, from: data),
              let document = AtlasDocument(path: path, capture: capture,
                glyphLimit: remainingGlyphs, runLimit: remainingRuns), admit(document) else { return nil }
        rebuilt += 1
        hot[path] = document; keys[path] = key
        return document
    }

    private func admit(_ document: AtlasDocument) -> Bool {
        let counts = document.preparationCounts
        let addonOwned = document.displayTiles == nil && overviewOwners[document.source.path] == ObjectIdentifier(document)
        let pixels = addonOwned ? 0 : document.tiles.reduce(0) { $0 + ($1.raster.map { $0.width * $0.height } ?? 0) }
        guard counts.glyphs <= remainingGlyphs, counts.runs <= remainingRuns, pixels <= remainingPixels else { return false }
        remainingGlyphs -= counts.glyphs; remainingRuns -= counts.runs; remainingPixels -= pixels
        return true
    }

    /// Called after raster admission, never from a camera or draw callback.
    func finishRefresh(documents: [String: AtlasDocument]) {
        for (path, document) in documents {
            // A prior write may have failed before the higher-density tier was
            // installed. Never retry the base key with addon pixels: a later
            // fresh load must reconstruct its correctly budgeted base preview.
            guard overviewOwners[path] != ObjectIdentifier(document),
                  let key = keys[path], !persisted.contains(key),
                  let data = try? AtlasDocumentArchive.encode(document, key: key, fonts: fonts) else { continue }
            let success = data.withUnsafeBytes { buffer in
                key.withCString {
                    let bytes = buffer.bindMemory(to: UInt8.self).baseAddress!
                    return rejected.contains(key) ? cacheRepair(handle, $0, bytes, UInt64(data.count))
                        : cachePut(handle, $0, bytes, UInt64(data.count))
                }
            }
            if success { saved += 1; persisted.insert(key) }
        }
        // Retain only the admitted current project, not an ever-growing history.
        hot = documents
        keys = keys.filter { documents[$0.key] != nil }
        persisted.formIntersection(Set(keys.values))
    }

    /// Prepare once after geometry and base archives, before atlas publication.
    /// The original source archive stays independent of this replaceable PNG tier.
    func prepareOverview(documents: [String: AtlasDocument], pixelLimit: Int = 64 * 1024 * 1024) {
        overviewDiskHits = 0; overviewRAMHits = 0; overviewRasters = 0; overviewSaved = 0
        let paths = documents.keys.sorted()
        let all = paths.flatMap { documents[$0]!.renderTiles }
        guard !all.isEmpty, all.count <= 65536,
              all.allSatisfy({ $0.rect.width.isFinite && $0.rect.height.isFinite && $0.rect.width > 0 && $0.rect.height > 0 }) else { return }
        let limit = max(all.count, min(64 * 1024 * 1024, max(0, pixelLimit)))
        let area = all.reduce(0.0) { $0 + $1.rect.width * $1.rect.height }
        guard area.isFinite, area > 0 else { return }
        let density = sqrt(Double(limit - all.count) / area)
        var remaining = limit, remainingTiles = all.count
        for path in paths {
            guard let document = documents[path] else { continue }
            var sizes: [CGSize] = []
            for tile in document.renderTiles {
                let scale = min(1, max(0, density * tile.contentScale))
                let width = max(1, min(remaining - remainingTiles + 1, Int(floor(AtlasTextTile.width * scale))))
                let height = max(1, min((remaining - remainingTiles + 1) / width, Int(floor(tile.height * scale))))
                sizes.append(CGSize(width: width, height: height))
                remaining -= width * height; remainingTiles -= 1
            }
            guard let sourceKey = keys[path], let key = try? AtlasOverviewArchive.key(sourceKey: sourceKey, tiles: document.renderTiles, sizes: sizes) else {
                for (tile, size) in zip(document.renderTiles, sizes) { tile.raster = AtlasOverviewArchive.render(tile, size: size); overviewRasters += 1 }
                continue
            }
            if overviewKeys[path] == key, overviewOwners[path] == ObjectIdentifier(document),
               zip(document.renderTiles, sizes).allSatisfy({ $0.0.raster?.width == Int($0.1.width) && $0.0.raster?.height == Int($0.1.height) }) {
                overviewRAMHits += 1; continue
            }
            var length: UInt64 = 0
            var restored: [CGImage]?
            if handle != 0, let bytes = key.withCString({ cacheGet(handle, $0, &length) }) {
                if length <= UInt64(AtlasBinaryWriter.limit) {
                    restored = try? AtlasOverviewArchive.decode(Data(bytes: bytes, count: Int(length)), sizes: sizes)
                }
                cacheFree(bytes, length)
            }
            if let restored {
                for (tile, image) in zip(document.renderTiles, restored) { tile.raster = image }
                overviewDiskHits += 1
            } else {
                for (tile, size) in zip(document.renderTiles, sizes) { tile.raster = AtlasOverviewArchive.render(tile, size: size); overviewRasters += 1 }
                if handle != 0, let data = try? AtlasOverviewArchive.encode(document.renderTiles) {
                    let saved = data.withUnsafeBytes { buffer in
                        key.withCString { cachePut(handle, $0, buffer.bindMemory(to: UInt8.self).baseAddress!, UInt64(data.count)) }
                    }
                    if saved { overviewSaved += 1 }
                }
            }
            overviewKeys[path] = key; overviewOwners[path] = ObjectIdentifier(document)
        }
        overviewKeys = overviewKeys.filter { documents[$0.key] != nil }
        overviewOwners = overviewOwners.filter { documents[$0.key] != nil }
    }
}

enum AtlasOverviewArchive {
    static func key(sourceKey: String, tiles: [AtlasTextTile], sizes: [CGSize]) throws -> String {
        guard tiles.count == sizes.count else { throw AtlasPreparationError.invalid }
        var writer = AtlasBinaryWriter()
        try writer.string("fcb-overview-png/2;balanced-source-rows;opaque-monokai")
        try writer.string(sourceKey)
        for (tile, size) in zip(tiles, sizes) {
            try writer.integer(UInt64(tile.sourceRange.location))
            try writer.integer(UInt64(tile.sourceRange.length))
            try writer.integer(UInt64(tile.lineCount))
            for value in [tile.rect.width, tile.rect.height, size.width, size.height] { try writer.number(value) }
        }
        return SHA256.hash(data: writer.data).map { String(format: "%02x", $0) }.joined()
    }

    static func render(_ tile: AtlasTextTile, size: CGSize) -> CGImage? {
        let width = Int(size.width), height = Int(size.height)
        guard width > 0, height > 0, width <= 64 * 1024 * 1024 / height,
              let bitmap = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
                bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
        // Opaque PNG avoids unpremultiplication rounding of translucent glyph ink.
        bitmap.setFillColor(Monokai.background.cgColor)
        bitmap.fill(CGRect(x: 0, y: 0, width: width, height: height))
        bitmap.translateBy(x: 0, y: Double(height))
        bitmap.scaleBy(x: Double(width) / AtlasTextTile.width, y: -Double(height) / tile.height)
        tile.draw(in: bitmap)
        return bitmap.makeImage()
    }

    static func encode(_ tiles: [AtlasTextTile]) throws -> Data {
        var writer = AtlasBinaryWriter()
        try writer.string("FCBOV1"); try writer.integer(UInt64(tiles.count))
        for tile in tiles {
            guard let image = tile.raster else { throw AtlasPreparationError.invalid }
            let data = NSMutableData()
            guard let destination = CGImageDestinationCreateWithData(data, "public.png" as CFString, 1, nil) else { throw AtlasPreparationError.invalid }
            CGImageDestinationAddImage(destination, image, nil)
            guard CGImageDestinationFinalize(destination) else { throw AtlasPreparationError.invalid }
            try writer.integer(UInt64(data.length)); try writer.bytes(data as Data)
        }
        return writer.data
    }

    static func decode(_ data: Data, sizes: [CGSize]) throws -> [CGImage] {
        guard data.count <= AtlasBinaryWriter.limit else { throw AtlasPreparationError.limit }
        var reader = AtlasBinaryReader(data: data)
        guard try reader.string(16) == "FCBOV1", try reader.count(65536) == sizes.count else { throw AtlasPreparationError.invalid }
        var result: [CGImage] = [], remaining = 64 * 1024 * 1024
        for size in sizes {
            let count = try reader.count(AtlasBinaryWriter.limit)
            let png = try reader.bytes(count)
            guard let source = CGImageSourceCreateWithData(png as CFData, [kCGImageSourceShouldCache: false] as CFDictionary),
                  CGImageSourceGetCount(source) == 1,
                  let props = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
                  let width = props[kCGImagePropertyPixelWidth] as? Int,
                  let height = props[kCGImagePropertyPixelHeight] as? Int,
                  let depth = props[kCGImagePropertyDepth] as? Int, depth == 8,
                  width > 0, height > 0, width == Int(size.width), height == Int(size.height),
                  width <= remaining / height,
                  let image = CGImageSourceCreateImageAtIndex(source, 0, [kCGImageSourceShouldCacheImmediately: true] as CFDictionary),
                  let bitmap = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
                    bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { throw AtlasPreparationError.invalid }
            remaining -= width * height
            bitmap.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
            guard let decoded = bitmap.makeImage() else { throw AtlasPreparationError.invalid }
            result.append(decoded)
        }
        guard reader.offset == data.count else { throw AtlasPreparationError.invalid }
        return result
    }
}

enum AtlasDocumentArchive {
    static func encode(_ document: AtlasDocument, key: String, fonts: AtlasPreparedFonts) throws -> Data {
        var writer = AtlasBinaryWriter()
        try writer.string("FCBPT4")
        try writer.string(key)
        try writer.string(document.source.path)
        try writer.string(document.capture.text)
        try writer.integer(UInt64(document.capture.runs.count))
        for run in document.capture.runs {
            guard let start = UInt64(run.start), let length = UInt64(run.length) else { throw AtlasPreparationError.invalid }
            try writer.integer(start); try writer.integer(length); try writer.string(run.role)
        }
        try writer.integer(UInt64(document.tiles.count))
        var styles: [Data: UInt32] = [:]
        for (part, tile) in document.tiles.enumerated() {
            try writer.integer(UInt64(tile.sourceRange.location))
            try writer.integer(UInt64(tile.sourceRange.length))
            let title = (document.source.path as NSString).lastPathComponent + (part > 0 ? " · \(part + 1)" : "")
            let header = tile.preparedHeader ?? tile.header.flatMap { AtlasPreparedLine($0, source: title) }
            guard let header else { throw AtlasPreparationError.fontUnavailable("header run extraction") }
            try line(header, into: &writer, fonts: fonts, styles: &styles)
            try writer.integer(UInt64(tile.lineCount))
            for index in 0..<tile.lineCount {
                guard let prepared = tile.preparedLines?[index] ?? AtlasPreparedLine(tile.lines[index], source: document.capture.text) else {
                    throw AtlasPreparationError.fontUnavailable("body run extraction index: \(index)")
                }
                try line(prepared, into: &writer, fonts: fonts, styles: &styles)
            }
            guard let image = tile.raster, let pixels = image.dataProvider?.data else { throw AtlasPreparationError.invalid }
            guard image.bitsPerComponent == 8, image.bitsPerPixel == 32,
                  image.bytesPerRow == image.width * 4 else { throw AtlasPreparationError.invalid }
            try writer.integer(UInt64(image.width)); try writer.integer(UInt64(image.height))
            try writer.integer(image.bitmapInfo.rawValue)
            try writer.integer(UInt64(CFDataGetLength(pixels)))
            try writer.bytes(pixels as Data)
        }
        return writer.data
    }

    private static func line(_ line: AtlasPreparedLine, into writer: inout AtlasBinaryWriter,
                             fonts: AtlasPreparedFonts, styles: inout [Data: UInt32]) throws {
        guard line.sourceRange.location >= 0, line.sourceRange.length >= 0 else { throw AtlasPreparationError.invalid }
        try writer.integer(UInt64(line.sourceRange.location)); try writer.integer(UInt64(line.sourceRange.length))
        try writer.integer(UInt64(line.runs.count))
        for run in line.runs {
            let font = try fonts.encode(run.font, fallbackText: run.fallbackText)
            var style = AtlasBinaryWriter()
            try style.integer(UInt64(font.count)); try style.bytes(font)
            guard let color = run.color.converted(to: CGColorSpace(name: CGColorSpace.sRGB)!, intent: .defaultIntent, options: nil),
                  let components = color.components, components.count == 4 else { throw AtlasPreparationError.invalid }
            for value in components { try style.number(value) }
            try AtlasPreparedFonts.matrix(run.matrix, into: &style)
            if let index = styles[style.data] { try writer.integer(index) }
            else {
                guard styles.count < 4096 else { throw AtlasPreparationError.limit }
                let index = UInt32(styles.count)
                styles[style.data] = index
                try writer.integer(index)
                try writer.bytes(style.data)
            }
            guard run.glyphs.count == run.positions.count else { throw AtlasPreparationError.invalid }
            try writer.integer(UInt64(run.glyphs.count))
            for (glyph, point) in zip(run.glyphs, run.positions) {
                try writer.integer(glyph); try writer.number(point.x); try writer.number(point.y)
            }
        }
    }

    static func decode(_ data: Data, path: String, key: String, fonts: AtlasPreparedFonts,
                       glyphLimit: Int = 4 * 1024 * 1024, pixelLimit: Int = 16 * 1024 * 1024,
                       runLimit: Int = 262144, onColorLineRestore: () -> Void = {}) throws -> AtlasDocument {
        guard data.count <= AtlasBinaryWriter.limit else { throw AtlasPreparationError.limit }
        var reader = AtlasBinaryReader(data: data)
        guard try reader.string(16) == "FCBPT4", try reader.string(64) == key,
              try reader.string(16384) == path else { throw AtlasPreparationError.invalid }
        let text = try reader.string(4 * 1024 * 1024)
        let runCount = try reader.count(4 * 1024 * 1024, stride: 24)
        var runs: [AtlasHighlightCapture.Run] = []
        for _ in 0..<runCount {
            let start: UInt64 = try reader.integer(), length: UInt64 = try reader.integer()
            runs.append(.init(start: String(start), length: String(length), role: try reader.string(32)))
        }
        let capture = AtlasHighlightCapture(schema: "fcb.source-document/1", text: text, runs: runs)
        guard capture.validatedRuns() != nil else { throw AtlasPreparationError.invalid }
        let textLength = text.utf16.count
        let tileCount = try reader.count(65536, stride: 32)
        var tiles: [AtlasTextTile] = []
        var cursor = 0
        var glyphsRemaining = glyphLimit
        var pixelsRemaining = pixelLimit
        var runsRemaining = runLimit
        var typesetter: CTTypesetter?
        var restoredColorLineCount = 0
        var styles: [AtlasPreparedRun] = []
        for _ in 0..<tileCount {
            let start: UInt64 = try reader.integer(), length: UInt64 = try reader.integer()
            guard start == UInt64(cursor), length > 0, length <= UInt64(textLength - cursor) else {
                throw AtlasPreparationError.invalid
            }
            var header = try line(from: &reader, fonts: fonts, remaining: &glyphsRemaining, runsRemaining: &runsRemaining, styles: &styles)
            let title = (path as NSString).lastPathComponent + (tiles.count > 0 ? " · \(tiles.count + 1)" : "")
            guard header.sourceRange == NSRange(location: 0, length: title.utf16.count) else { throw AtlasPreparationError.invalid }
            if header.hasColorGlyphs {
                let chargedGlyphs = header.runs.reduce(0) { $0 + $1.glyphs.count }
                guard chargedGlyphs <= glyphsRemaining, header.runs.count <= runsRemaining else { throw AtlasPreparationError.limit }
                let restored = AtlasTextTile.makeHeader(title)
                onColorLineRestore()
                let actualGlyphs = CTLineGetGlyphCount(restored), actualRuns = CFArrayGetCount(CTLineGetGlyphRuns(restored))
                guard actualGlyphs <= chargedGlyphs, actualRuns <= header.runs.count else { throw AtlasPreparationError.invalid }
                glyphsRemaining -= actualGlyphs; runsRemaining -= actualRuns
                header.retainedColorLine = restored
                restoredColorLineCount += 1
            }
            let count = try reader.count(80, stride: 8)
            guard count > 0 else { throw AtlasPreparationError.invalid }
            var lines: [AtlasPreparedLine] = []
            var lineCursor = cursor
            for _ in 0..<count {
                var prepared = try line(from: &reader, fonts: fonts, remaining: &glyphsRemaining, runsRemaining: &runsRemaining, styles: &styles)
                guard prepared.sourceRange.location == lineCursor, prepared.sourceRange.length > 0,
                      prepared.sourceRange.length <= cursor + Int(length) - lineCursor else { throw AtlasPreparationError.invalid }
                lineCursor += prepared.sourceRange.length
                if prepared.hasColorGlyphs {
                    let chargedGlyphs = prepared.runs.reduce(0) { $0 + $1.glyphs.count }
                    guard chargedGlyphs <= glyphsRemaining, prepared.runs.count <= runsRemaining else { throw AtlasPreparationError.limit }
                    if typesetter == nil { typesetter = CTTypesetterCreateWithAttributedString(AtlasDocument.style(capture)) }
                    let restored = CTTypesetterCreateLine(typesetter!, CFRange(location: prepared.sourceRange.location, length: prepared.sourceRange.length))
                    onColorLineRestore()
                    let actualGlyphs = CTLineGetGlyphCount(restored), actualRuns = CFArrayGetCount(CTLineGetGlyphRuns(restored))
                    guard actualGlyphs <= chargedGlyphs, actualRuns <= prepared.runs.count else { throw AtlasPreparationError.invalid }
                    glyphsRemaining -= actualGlyphs; runsRemaining -= actualRuns
                    prepared.retainedColorLine = restored
                    restoredColorLineCount += 1
                }
                lines.append(prepared)
            }
            guard lineCursor == cursor + Int(length) else { throw AtlasPreparationError.invalid }
            let tile = AtlasTextTile(path: path, preparedLines: lines, header: header,
                sourceRange: NSRange(location: cursor, length: Int(length)))
            cursor += Int(length)
            let width: UInt64 = try reader.integer(), height: UInt64 = try reader.integer()
            let bitmap: UInt32 = try reader.integer()
            guard width > 0, width <= 648, height > 0, height <= 1302,
                  bitmap == CGImageAlphaInfo.premultipliedLast.rawValue else {
                throw AtlasPreparationError.invalid
            }
            guard width * height <= UInt64(pixelsRemaining) else { throw AtlasPreparationError.limit }
            let bytes = try reader.count(pixelsRemaining * 4)
            guard bytes == Int(width * height * 4) else { throw AtlasPreparationError.invalid }
            pixelsRemaining -= Int(width * height)
            let pixels = try reader.bytes(bytes)
            guard let provider = CGDataProvider(data: pixels as CFData), let image = CGImage(width: Int(width), height: Int(height),
                bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: Int(width) * 4,
                space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGBitmapInfo(rawValue: bitmap),
                provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent) else {
                throw AtlasPreparationError.invalid
            }
            tile.raster = image
            tiles.append(tile)
        }
        guard cursor == textLength, reader.offset == data.count else { throw AtlasPreparationError.invalid }
        return AtlasDocument(source: AtlasSource(path: path, text: text), capture: capture, tiles: tiles, restoredColorLineCount: restoredColorLineCount)
    }

    private static func line(from reader: inout AtlasBinaryReader, fonts: AtlasPreparedFonts,
                             remaining: inout Int, runsRemaining: inout Int,
                             styles: inout [AtlasPreparedRun]) throws -> AtlasPreparedLine {
        let start: UInt64 = try reader.integer(), length: UInt64 = try reader.integer()
        guard start <= 4 * 1024 * 1024, length <= 4 * 1024 * 1024 else { throw AtlasPreparationError.invalid }
        let count = try reader.count(min(16384, runsRemaining), stride: 12)
        runsRemaining -= count
        var runs: [AtlasPreparedRun] = []
        for _ in 0..<count {
            let index: UInt32 = try reader.integer()
            guard Int(index) <= styles.count, index < 4096 else { throw AtlasPreparationError.invalid }
            if Int(index) == styles.count {
            let fontBytes = try reader.count(8192)
            let encodedFont = try reader.bytes(fontBytes)
            let restoredFont = try fonts.decode(encodedFont)
            let rgba = try (0..<4).map { _ in try reader.number() }
            guard rgba.allSatisfy({ (0...1).contains($0) }),
                  let color = CGColor(colorSpace: CGColorSpace(name: CGColorSpace.sRGB)!, components: rgba.map { CGFloat($0) }) else {
                throw AtlasPreparationError.invalid
            }
            let matrix = try AtlasPreparedFonts.matrix(from: &reader)
            styles.append(AtlasPreparedRun(font: restoredFont.font, color: color, matrix: matrix, glyphs: [], positions: [],
                fallbackText: restoredFont.fallbackText))
            }
            let style = styles[Int(index)]
            let count = try reader.count(remaining, stride: 18)
            remaining -= count
            var glyphs: [CGGlyph] = []; var positions: [CGPoint] = []
            glyphs.reserveCapacity(count); positions.reserveCapacity(count)
            for _ in 0..<count {
                let glyph: UInt16 = try reader.integer()
                let x = try reader.number(), y = try reader.number()
                guard abs(x) <= 1_000_000, abs(y) <= 1_000_000,
                      Int(glyph) < CTFontGetGlyphCount(style.font) else { throw AtlasPreparationError.invalid }
                glyphs.append(glyph); positions.append(CGPoint(x: x, y: y))
            }
            runs.append(AtlasPreparedRun(font: style.font, color: style.color, matrix: style.matrix, glyphs: glyphs, positions: positions,
                fallbackText: style.fallbackText))
        }
        var line = AtlasPreparedLine(runs: runs)
        line.sourceRange = NSRange(location: Int(start), length: Int(length))
        return line
    }
}
