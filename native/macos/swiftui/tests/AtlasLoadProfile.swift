import Foundation
import CryptoKit
import CoreGraphics
import CoreText

@_silgen_name("fcb_atlas_layout")
func profileAtlas(_ root: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_source_document")
func profileSource(_ path: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_text_tile_positions")
func profilePositions(_ heights: UnsafePointer<Double>?, _ count: UInt64, _ columns: UInt64,
                      _ width: Double, _ gap: Double) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_free_string")
func profileFree(_ pointer: UnsafeMutablePointer<CChar>?)

@main struct AtlasLoadProfile {
    static func reply(_ pointer: UnsafeMutablePointer<CChar>?) -> Data? {
        guard let pointer else { return nil }
        defer { profileFree(pointer) }
        return Data(String(cString: pointer).utf8)
    }
    static func require(_ condition: @autoclosure () -> Bool, _ message: String = "profile invariant failed") {
        guard condition() else {
            FileHandle.standardError.write(Data(("PROFILE ASSERTION: " + message + "\n").utf8))
            preconditionFailure(message)
        }
    }
    static func describeFont(_ font: CTFont, label: String) throws {
        let digest = try AtlasPreparedFonts().fingerprint(font).map { String(format: "%02x", $0) }.joined()
        let graphics = CTFontCopyGraphicsFont(font, nil)
        let graphicsTags = graphics.tableTags!
        let gtags = (0..<CFArrayGetCount(graphicsTags)).map { UInt32(truncatingIfNeeded: UInt(bitPattern: CFArrayGetValueAtIndex(graphicsTags, $0))) }.sorted()
        require(gtags.count < 128, "bounded graphics font table list")
        var graphicsHash = SHA256()
        for tag in gtags {
            guard let bytes = graphics.table(for: tag) else { continue }
            var id = tag.littleEndian
            withUnsafeBytes(of: &id) { graphicsHash.update(data: Data($0)) }
            graphicsHash.update(data: bytes as Data)
        }
        FileHandle.standardError.write(Data("GRAPHICS_FONT_TABLES label=\(label) digest=\(self.digest(graphicsHash)) tags=\(gtags.map { String($0, radix: 16) })\n".utf8))
        let available = CTFontCopyAvailableTables(font, CTFontTableOptions(rawValue: 0))!
        let tags = (0..<CFArrayGetCount(available)).map { UInt32(truncatingIfNeeded: UInt(bitPattern: CFArrayGetValueAtIndex(available, $0))) }.sorted()
        require(tags.count < 128, "bounded diagnostic table list")
        FileHandle.standardError.write(Data("FONT_TABLES label=\(label) digest=\(digest) tags=\(tags.map { String($0, radix: 16) })\n".utf8))
        for tag in tags {
            let data = CTFontCopyTable(font, tag, CTFontTableOptions(rawValue: 0)) as Data?
            let hash = data.map { SHA256.hash(data: $0).map { String(format: "%02x", $0) }.joined() } ?? "missing"
            FileHandle.standardError.write(Data("FONT_TABLE label=\(label) tag=\(String(tag, radix: 16)) bytes=\(data?.count ?? -1) hash=\(hash)\n".utf8))
        }
    }
    static func stamp() -> Double { ProcessInfo.processInfo.systemUptime }
    static func digest(_ hash: SHA256) -> String { hash.finalize().map { String(format: "%02x", $0) }.joined() }
    static func run(root: String, pass: Int, oracle: String, cache: AtlasProjectCache?) throws {
        let engineBefore = cache?.engineStatsJSON
        require(cache == nil || engineBefore != nil, "cache profiling requires the real Rust cache owner")
        let begin = stamp()
        cache?.beginRefresh()
        var clock = stamp()
        guard let atlas = reply(profileAtlas(root)),
              let object = try JSONSerialization.jsonObject(with: atlas) as? [String: Any],
              let files = object["files"] as? [[String: Any]], !files.isEmpty else {
            preconditionFailure("real corpus discovery must succeed")
        }
        let discovery = stamp() - clock
        var captureTime = 0.0, decodeTime = 0.0, shapeTime = 0.0
        var cacheTime = 0.0
        var remaining = 64 * 1024 * 1024
        var documents: [AtlasDocument] = []
        var byPath: [String: AtlasDocument] = [:]
        var tiles: [AtlasTextTile] = []
        var sourceHash = SHA256(), captureHash = SHA256(), geometryHash = SHA256(), rasterHash = SHA256()
        var admittedBytes = 0, unavailable = 0
        for file in files {
            guard let path = file["path"] as? String else { preconditionFailure("invalid atlas path") }
            guard ((file["bytes"] as? NSNumber)?.intValue ?? 0) <= remaining else { unavailable += 1; continue }
            let prepared: AtlasDocument?
            var directCapture: Data?
            if let cache {
                clock = stamp()
                prepared = cache.document(path: path) {
                    guard let data = reply(profileSource((root as NSString).appendingPathComponent(path))) else { return nil }
                    return try? JSONDecoder().decode(AtlasHighlightCapture.self, from: data)
                }
                cacheTime += stamp() - clock
            } else {
                clock = stamp()
                let data = reply(profileSource((root as NSString).appendingPathComponent(path)))
                captureTime += stamp() - clock
                guard let data else { unavailable += 1; continue }
                clock = stamp()
                let capture = try JSONDecoder().decode(AtlasHighlightCapture.self, from: data)
                decodeTime += stamp() - clock
                guard capture.text.utf8.count <= remaining else { unavailable += 1; continue }
                clock = stamp()
                prepared = AtlasDocument(path: path, capture: capture)
                shapeTime += stamp() - clock
                directCapture = data
            }
            guard let document = prepared, document.source.text.utf8.count <= remaining,
                  document.tiles.count <= 65536 - tiles.count else { unavailable += 1; continue }
            let capture = document.capture
            if let data = directCapture {
                captureHash.update(data: Data(path.utf8)); captureHash.update(data: data)
            }
            require(document.source.text == capture.text, "shaping must preserve exact source")
            var end = 0
            for tile in document.tiles {
                require(tile.sourceRange.location == end, "no wrapped-source gap at \(path), expected \(end), actual \(tile.sourceRange)")
                end += tile.sourceRange.length
            }
            require(end == capture.text.utf16.count, "no source suffix omitted at \(path), end \(end), source \(capture.text.utf16.count)")
            remaining -= capture.text.utf8.count
            admittedBytes += capture.text.utf8.count
            sourceHash.update(data: Data(path.utf8)); sourceHash.update(data: Data([0]))
            sourceHash.update(data: Data(capture.text.utf8)); sourceHash.update(data: Data([0]))
            documents.append(document)
            byPath[path] = document
            tiles.append(contentsOf: document.tiles)
        }
        require(!tiles.isEmpty, "real corpus must yield shaped tiles")
        clock = stamp()
        let heights = tiles.map(\.height)
        let columns = max(1, min(256, Int(sqrt(heights.reduce(0, +) * 1.9 / AtlasTextTile.width).rounded())))
        let data = heights.withUnsafeBufferPointer {
            reply(profilePositions($0.baseAddress, UInt64($0.count), UInt64(columns), AtlasTextTile.width, 6))
        }!
        let positions = try JSONDecoder().decode([[Double]].self, from: data)
        require(positions.count == tiles.count, "packing must preserve every tile")
        geometryHash.update(data: data)
        for (tile, rect) in zip(tiles, positions) {
            require(rect.count == 4 && rect.allSatisfy { $0.isFinite })
            tile.rect = CGRect(x: rect[0], y: rect[1], width: rect[2], height: rect[3])
        }
        let packingTime = stamp() - clock
        clock = stamp()
        let budget = max(1, 16 * 1024 * 1024 / tiles.count)
        var remainingPixels = 16 * 1024 * 1024
        var rasterized = 0
        for (index, tile) in tiles.enumerated() {
            let available = max(1, remainingPixels - (tiles.count - index - 1))
            if tile.raster.map({ $0.width * $0.height > available }) ?? true {
                tile.prepareRaster(pixelBudget: min(budget, available))
                rasterized += 1
            }
            remainingPixels -= tile.raster.map { $0.width * $0.height } ?? 0
        }
        let rasterTime = stamp() - clock
        clock = stamp()
        cache?.finishRefresh(documents: byPath)
        let persistTime = stamp() - clock
        let loadTime = stamp() - begin
        if ProcessInfo.processInfo.environment["FCB_PROFILE_FONT_DIAGNOSTIC"] == "1" {
            var seen: Set<String> = []
            var inspectedFonts: [CTFont] = []
            for document in documents {
                for tile in document.tiles where tile.preparedLines == nil {
                    for line in tile.lines {
                        guard let prepared = AtlasPreparedLine(line, source: document.capture.text) else { continue }
                        for run in prepared.runs where CTFontCopyPostScriptName(run.font) as String == "CourierNewPSMT" {
                            if inspectedFonts.contains(where: { CFEqual($0, run.font) }) { continue }
                            require(inspectedFonts.count < 128, "bounded diagnostic font identities")
                            inspectedFonts.append(run.font)
                            let resource = (CTFontCopyAttribute(run.font, kCTFontURLAttribute) as? URL)?.lastPathComponent ?? "none"
                            let digest = try AtlasPreparedFonts().fingerprint(run.font).map { String(format: "%02x", $0) }.joined()
                            let identity = "resource=\(resource) digest=\(digest)"
                            if seen.insert(identity).inserted {
                                FileHandle.standardError.write(Data(("ORIGINAL_FONT " + identity + "\n").utf8))
                                var matrix = CTFontGetMatrix(run.font)
                                let variation = CTFontCopyVariation(run.font) as? [NSNumber: NSNumber] ?? [:]
                                let descriptor = CTFontDescriptorCreateWithAttributes([
                                    kCTFontNameAttribute: "CourierNewPSMT", kCTFontVariationAttribute: variation
                                ] as CFDictionary)
                                let candidate = CTFontCreateWithFontDescriptor(descriptor, CTFontGetSize(run.font), &matrix)
                                FileHandle.standardError.write(Data("FONT_DESCRIPTORS original=\(CTFontDescriptorCopyAttributes(CTFontCopyFontDescriptor(run.font))) candidate=\(CTFontDescriptorCopyAttributes(CTFontCopyFontDescriptor(candidate)))\n".utf8))
                                try describeFont(run.font, label: "original")
                                try describeFont(candidate, label: "named-after-shaping")
                                let originalURL = CTFontCopyAttribute(run.font, kCTFontURLAttribute)
                                let candidateURL = CTFontCopyAttribute(candidate, kCTFontURLAttribute)
                                FileHandle.standardError.write(Data("FONT_RESOURCES original=\(String(describing: originalURL)) candidate=\(String(describing: candidateURL))\n".utf8))
                                var tags: Set<UInt32> = []
                                for font in [run.font, candidate] {
                                    if let array = CTFontCopyAvailableTables(font, CTFontTableOptions(rawValue: 0)) {
                                        for index in 0..<CFArrayGetCount(array) {
                                            tags.insert(UInt32(truncatingIfNeeded: UInt(bitPattern: CFArrayGetValueAtIndex(array, index))))
                                        }
                                    }
                                }
                                for tag in tags.sorted() {
                                    let original = CTFontCopyTable(run.font, tag, CTFontTableOptions(rawValue: 0)) as Data?
                                    let named = CTFontCopyTable(candidate, tag, CTFontTableOptions(rawValue: 0)) as Data?
                                    if original != named {
                                        let summary: (Data?) -> String = { data in
                                            guard let data else { return "missing" }
                                            return "\(data.count):" + SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
                                        }
                                        FileHandle.standardError.write(Data("FONT_TABLE_DIFFERENCE tag=\(String(tag, radix: 16)) original=\(summary(original)) candidate=\(summary(named))\n".utf8))
                                    }
                                }

                            }
                        }
                    }
                }
            }
        }
        clock = stamp()
        if cache != nil {
            // Independent fresh-source oracle is OUTSIDE loading timing. The
            // whole-process hyperfine measurement includes this verification.
            for document in documents {
                let path = document.source.path
                guard let data = reply(profileSource((root as NSString).appendingPathComponent(path))) else {
                    preconditionFailure("golden source observation unavailable")
                }
                let expected = try JSONDecoder().decode(AtlasHighlightCapture.self, from: data)
                let actual = document.capture
                require(expected.text == actual.text && expected.runs.count == actual.runs.count,
                             "cache must preserve current source and syntax")
                for (a, b) in zip(expected.runs, actual.runs) {
                    require(a.start == b.start && a.length == b.length && a.role == b.role,
                                 "cache altered syntax role or UTF16 coordinate")
                }
                captureHash.update(data: Data(path.utf8)); captureHash.update(data: data)
            }
        }
        let captureOracleTime = stamp() - clock
        clock = stamp()
        var rasterBytes = 0
        for tile in tiles {
            guard let image = tile.raster, let data = image.dataProvider?.data else {
                preconditionFailure("admitted tile raster must exist")
            }
            rasterBytes += CFDataGetLength(data)
            rasterHash.update(data: data as Data)
        }
        let oracleTime = stamp() - clock
        let signature: [String: String] = ["source": digest(sourceHash), "capture": digest(captureHash),
            "geometry": digest(geometryHash), "raster": digest(rasterHash)]
        if FileManager.default.fileExists(atPath: oracle) {
            let expected = try JSONDecoder().decode([String: String].self, from: Data(contentsOf: URL(fileURLWithPath: oracle)))
            require(signature == expected, "repeat changed source, syntax, layout or pixels: actual=\(signature) expected=\(expected)")
        } else {
            try JSONEncoder().encode(signature).write(to: URL(fileURLWithPath: oracle), options: .atomic)
        }
        let report: [String: Any] = ["pass": pass, "files": files.count, "admitted": documents.count,
            "unavailable": unavailable, "source_bytes": admittedBytes, "tiles": tiles.count,
            "raster_bytes": rasterBytes, "discovery_s": discovery, "capture_highlight_s": captureTime,
            "decode_s": decodeTime, "shape_s": shapeTime, "packing_s": packingTime,
            "raster_s": rasterTime, "load_total_s": loadTime, "raster_oracle_s": oracleTime,
            "mode": cache == nil ? "direct-current-production" : "production-project-cache",
            "document_cache_s": cacheTime, "persist_s": persistTime,
            "capture_oracle_s": captureOracleTime, "rasterized_tiles": rasterized,
            "ram_hits": cache?.ramHits ?? 0, "disk_hits": cache?.diskHits ?? 0,
            "rebuilt": cache?.rebuilt ?? documents.count, "saved": cache?.saved ?? 0,
            "reshaped_color_lines": cache?.reshapedColorLines ?? 0,
            "rejection_reasons": cache?.rejectionReasons ?? [:],
            "engine_stats_before": engineBefore ?? "{}",
            "engine_stats_after": cache?.engineStatsJSON ?? "{}",
            "checksums": signature]
        print(String(data: try JSONSerialization.data(withJSONObject: report, options: [.sortedKeys]), encoding: .utf8)!)
        fflush(stdout)
        withExtendedLifetime(documents) {}
    }
    static func main() throws {
        if ProcessInfo.processInfo.environment["FCB_PROFILE_FONT_DIAGNOSTIC"] == "1" {
            let descriptor = CTFontDescriptorCreateWithAttributes([kCTFontNameAttribute: "CourierNewPSMT", kCTFontVariationAttribute: [:]] as CFDictionary)
            var matrix = CGAffineTransform.identity
            try describeFont(CTFontCreateWithFontDescriptor(descriptor, 13, &matrix), label: "named-before-shaping")
        }
        let args = CommandLine.arguments
        require(args.count == 4 || args.count == 5, "corpus root, oracle path, passes, optional cache directory")
        let cache = args.count == 5 ? AtlasProjectCache(root: args[1], cacheDirectory: args[4]) : nil
        for pass in 0..<Int(args[3])! {
            try autoreleasepool { try run(root: args[1], pass: pass, oracle: args[2], cache: cache) }
        }
    }
}
