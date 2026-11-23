import Foundation
import CoreGraphics
import CoreText
import Dispatch
import AppKit
import CryptoKit

/// Resident preview limits are separate from the renderer's GPU/image limits
/// and from an independently opened reader. At most one bounded preparation
/// overlaps this working set. Source text is never truncated to fit a preview.
struct AtlasViewportLimits: Sendable {
    var documents = 24
    var sourceBytes = 32 * 1024 * 1024
    var glyphs = 4 * 1024 * 1024
    var runs = 128 * 1024
    var tiles = 4096
    var pixels = 4 * 1024 * 1024
    var estimatedBytes = 256 * 1024 * 1024
    var maximumDemand = 32

    static let sourceFileBytes = 4 * 1024 * 1024
    static let responseBytes = 8 * 1024 * 1024
    static let captureRuns = 65_536
    static let fileRasterPixels = 512 * 1024
    static let preparationBytes = 512 * 1024 * 1024

    var valid: Bool {
        (1...256).contains(documents) && (1...(64 * 1024 * 1024)).contains(sourceBytes)
            && (1...(8 * 1024 * 1024)).contains(glyphs) && (1...(1024 * 1024)).contains(runs)
            && (1...16_384).contains(tiles) && (1...(16 * 1024 * 1024)).contains(pixels)
            && (1...(512 * 1024 * 1024)).contains(estimatedBytes) && (1...64).contains(maximumDemand)
    }
}

struct AtlasViewportCost: Equatable, Sendable {
    var sourceBytes = 0, glyphs = 0, runs = 0, tiles = 0, pixels = 0, estimatedBytes = 0

    static func + (left: Self, right: Self) -> Self {
        Self(sourceBytes: left.sourceBytes + right.sourceBytes, glyphs: left.glyphs + right.glyphs,
            runs: left.runs + right.runs, tiles: left.tiles + right.tiles, pixels: left.pixels + right.pixels,
            estimatedBytes: left.estimatedBytes + right.estimatedBytes)
    }
    func fits(_ limits: AtlasViewportLimits) -> Bool {
        sourceBytes <= limits.sourceBytes && glyphs <= limits.glyphs && runs <= limits.runs
            && tiles <= limits.tiles && pixels <= limits.pixels && estimatedBytes <= limits.estimatedBytes
    }
}

enum AtlasViewportPreviewState: String, Sendable {
    case cold, queued, loading, ready, markdown, sourceFallback, empty, readerOnly, unavailable, capacityLimited
    var label: String {
        switch self {
        case .cold: return "Zoom in to load source"
        case .queued: return "Source preview queued"
        case .loading: return "Loading source preview"
        case .ready: return "Captured source preview"
        case .markdown: return "Captured Markdown preview"
        case .sourceFallback: return "Markdown preview unavailable — captured source shown"
        case .empty: return "Empty captured file"
        case .readerOnly: return "Preview limit — open in reader"
        case .unavailable: return "Preview unavailable — open in reader"
        case .capacityLimited: return "Preview memory full — open in reader"
        }
    }
}

/// Source capture remains authoritative even when native artifacts are present.
/// Injected transports opt into the additional immutable-artifact capability.
struct AtlasViewportSourceTransport: Sendable {
    let load: @Sendable (String, UInt64, AtlasSearchCancellation) throws -> AtlasProjectSourcePacket?
    var artifacts: AtlasViewportArtifactTransport? = nil
    static let live = Self(load: { path, handle, cancellation in
        try AtlasProjectWorker.source(path: path, handle: handle, cancellation: cancellation)
    }, artifacts: .live)
}

struct AtlasViewportArtifactTransport: Sendable {
    let environment: @Sendable (AtlasSearchCancellation) throws -> String
    let load: @Sendable (UInt64, String, Int, AtlasSearchCancellation) throws -> Data?
    let store: @Sendable (UInt64, String, Data, Bool, AtlasSearchCancellation) throws -> Bool
    static let live = Self(environment: { try AtlasViewportFontEnvironment.shared.snapshot($0) },
        load: { try AtlasProjectWorker.artifact(handle: $0, key: $1, limit: $2, cancellation: $3) },
        store: { try AtlasProjectWorker.storeArtifact(handle: $0, key: $1, data: $2, repair: $3, cancellation: $4) })
}

/// Notification callbacks only advance a locked generation. Registry/file
/// inspection runs on the viewport worker, never in the callback or main actor.
private final class AtlasViewportFontEnvironment: @unchecked Sendable {
    static let shared = AtlasViewportFontEnvironment()
    private let lock = NSLock()
    private var generation = UUID()
    private var cached: (generation: UUID, value: String)?
    private var observers: [(NotificationCenter, NSObjectProtocol)] = []
    private init() {
        let name = Notification.Name(kCTFontManagerRegisteredFontsChangedNotification as String)
        for center in [NotificationCenter.default, DistributedNotificationCenter.default()] {
            let observer = center.addObserver(forName: name, object: nil, queue: nil) { [weak self] _ in
                guard let self else { return }
                self.lock.lock(); self.generation = UUID(); self.cached = nil; self.lock.unlock()
            }
            observers.append((center, observer))
        }
    }
    deinit { for (center, observer) in observers { center.removeObserver(observer) } }
    func snapshot(_ cancellation: AtlasSearchCancellation) throws -> String {
        guard !cancellation.isCanceled else { throw AtlasPreparationError.canceled }
        lock.lock()
        let requested = generation, previous = cached
        lock.unlock()
        if let previous, previous.generation == requested { return previous.value }
        let registry = try AtlasProjectCache.nativeFontEnvironment(canceled: { cancellation.isCanceled })
        var writer = AtlasBinaryWriter(byteLimit: 16_384)
        try writer.string(registry)
        try writer.string(ProcessInfo.processInfo.operatingSystemVersionString)
        try writer.string(CTFontCopyPostScriptName(NSFont.monospacedSystemFont(ofSize: 13, weight: .regular) as CTFont) as String)
        let value = SHA256.hash(data: writer.data).map { String(format: "%02x", $0) }.joined()
        lock.lock()
        let current = generation == requested
        if current { cached = (requested, value) }
        lock.unlock()
        guard current, !cancellation.isCanceled else { throw AtlasPreparationError.canceled }
        return value
    }
}

struct AtlasViewportPreparation: Equatable, Sendable {
    var glyphArchiveHit = false
    var rasterArchiveHit = false
    var shapedDocuments = 0
    var rasterizedTiles = 0
    var restoredColorLines = 0
    var artifactsWritten = 0
}

/// One work item's native codec state. It is created and used only on the
/// serial viewport worker, and owns no source/document after that item ends.
private final class AtlasViewportArtifacts {
    static let byteLimit = 16 * 1024 * 1024
    // Rust may materialize a bounded 64 MiB artifact before the smaller native
    // limit is checked. Allow both foreign/local copies and the existing 256 MiB
    // physical-font table budget, separately from source/glyph/raster overlap.
    static let scratchBytes = 384 * 1024 * 1024
    let key: String
    private let environment: String
    private let handle: UInt64
    private let transport: AtlasViewportArtifactTransport
    private let cancellation: AtlasSearchCancellation
    private let fonts: AtlasPreparedFonts
    private var glyphRepair = false
    private var glyphLoaded = false
    private var rejectRelatedRasters = false
    private(set) var receipt = AtlasViewportPreparation()

    private func checkEnvelope(_ data: Data, marker: String, key: String) throws {
        guard data.count <= Self.byteLimit else { throw AtlasPreparationError.limit }
        var expected = AtlasBinaryWriter(byteLimit: 128)
        try expected.string(marker); try expected.string(key)
        guard data.starts(with: expected.data) else { throw AtlasPreparationError.invalid }
    }

    init?(root: String, path: String, sourceKey: [UInt8]?, handle: UInt64, workingBytes: Int,
          transport: AtlasViewportArtifactTransport?, cancellation: AtlasSearchCancellation) {
        guard handle != 0, let sourceKey, sourceKey.count == 32, let transport,
              workingBytes <= AtlasViewportLimits.preparationBytes - Self.scratchBytes,
              !cancellation.isCanceled,
              let environment = try? transport.environment(cancellation), environment.utf8.count <= 16_384 else { return nil }
        var writer = AtlasBinaryWriter(byteLimit: 64 * 1024)
        do {
            try writer.string("fcb-viewport-glyphs/1;" + AtlasProjectCache.layoutVersion)
            try writer.string(root); try writer.string(path); try writer.bytes(Data(sourceKey)); try writer.string(environment)
        } catch { return nil }
        key = SHA256.hash(data: writer.data).map { String(format: "%02x", $0) }.joined()
        self.environment = environment; self.handle = handle; self.transport = transport; self.cancellation = cancellation
        fonts = AtlasPreparedFonts(canceled: { cancellation.isCanceled })
    }

    func loadGlyphs(path: String, capture: AtlasHighlightCapture, limits: AtlasViewportLimits) -> AtlasDocument? {
        do {
            guard !cancellation.isCanceled,
                  let data = try transport.load(handle, key, Self.byteLimit, cancellation) else { return nil }
            do {
                try checkEnvelope(data, marker: "FCBPT5", key: key)
                let restored = try AtlasDocumentArchive.decode(data, path: path, key: key, fonts: fonts,
                    glyphLimit: limits.glyphs, pixelLimit: 0, runLimit: limits.runs, includeRasters: false,
                    byteLimit: Self.byteLimit, captureRunLimit: AtlasViewportLimits.captureRuns, tileLimit: limits.tiles,
                    expectedCapture: capture, canceled: { self.cancellation.isCanceled })
                glyphLoaded = true; receipt.glyphArchiveHit = true
                receipt.restoredColorLines = restored.restoredColorLineCount
                return restored
            } catch AtlasPreparationError.limit { rejectRelatedRasters = true; return nil }
            catch AtlasPreparationError.canceled { return nil }
            catch AtlasPreparationError.fontUnavailable { glyphRepair = true; rejectRelatedRasters = true; return nil }
            catch { glyphRepair = true; rejectRelatedRasters = true; return nil }
        } catch { return nil }
    }

    func saveGlyphs(_ document: AtlasDocument) {
        guard !glyphLoaded, !cancellation.isCanceled, environmentIsCurrent() else { return }
        do {
            let data = try AtlasDocumentArchive.encode(document, key: key, fonts: fonts, includeRasters: false,
                byteLimit: Self.byteLimit, canceled: { self.cancellation.isCanceled })
            guard environmentIsCurrent() else { return }
            if try transport.store(handle, key, data, glyphRepair, cancellation) { receipt.artifactsWritten += 1 }
        } catch { /* Optional immutable cache publication never rejects source. */ }
    }

    func prepareRasters(_ tiles: [AtlasTextTile], pixelBudget: Int, perTile: Int,
                        tileLimit: Int) -> Bool {
        guard !tiles.isEmpty, !cancellation.isCanceled else { return false }
        let sizes = tiles.map { $0.rasterSize(pixelBudget: perTile) }
        guard let rasterKey = try? AtlasOverviewArchive.viewportKey(sourceKey: key, tiles: tiles, sizes: sizes,
            pixelBudget: pixelBudget) else { return false }
        var repair = glyphRepair
        do {
            if let data = try transport.load(handle, rasterKey, Self.byteLimit, cancellation) {
                if !glyphLoaded || rejectRelatedRasters {
                    // Only glyph rows validated against their physical font
                    // tables authorize saved pixels. A cold rebuild can select
                    // another fallback face despite an unchanged registry stamp.
                    repair = true
                } else {
                    do {
                        try checkEnvelope(data, marker: "FCBOV2", key: rasterKey)
                        let images = try AtlasOverviewArchive.decodePixels(data, key: rasterKey, sizes: sizes,
                            byteLimit: Self.byteLimit, pixelLimit: pixelBudget, tileLimit: tileLimit,
                            canceled: { self.cancellation.isCanceled })
                        guard !cancellation.isCanceled else { return false }
                        for (tile, image) in zip(tiles, images) { tile.raster = image }
                        receipt.rasterArchiveHit = true
                        return true
                    } catch AtlasPreparationError.limit { }
                    catch AtlasPreparationError.canceled { return false }
                    catch { repair = true }
                }
            }
        } catch { }
        guard !cancellation.isCanceled else { return false }
        for tile in tiles {
            guard !cancellation.isCanceled else { return false }
            tile.prepareRaster(pixelBudget: perTile); receipt.rasterizedTiles += 1
        }
        do {
            let data = try AtlasOverviewArchive.encodePixels(tiles, key: rasterKey, byteLimit: Self.byteLimit,
                canceled: { self.cancellation.isCanceled })
            guard environmentIsCurrent() else { return false }
            if try transport.store(handle, rasterKey, data, repair, cancellation) { receipt.artifactsWritten += 1 }
        } catch { }
        return true
    }

    func environmentIsCurrent() -> Bool {
        guard !cancellation.isCanceled, let current = try? transport.environment(cancellation) else { return false }
        return current.utf8.elementsEqual(environment.utf8)
    }
}

/// Native rendering adaptation on the existing viewport worker. A transport
/// receives the immutable accepted source, never a live path to reopen. Keeping
/// this seam independent of reader/display types preserves source-only hosts.
struct AtlasViewportMarkdownTransport: Sendable {
    let prepare: @Sendable (AtlasDocument, AtlasViewportLimits, AtlasSearchCancellation) -> AtlasDocument?

    static func supports(_ path: [UInt8]) -> Bool {
        guard !path.isEmpty, path.count <= 16_384, let dot = path.lastIndex(of: 46),
              path.lastIndex(of: 47).map({ $0 < dot }) ?? true else { return false }
        let suffix = path.suffix(from: dot + 1)
        guard (2...8).contains(suffix.count) else { return false }
        let lower = suffix.map { (65...90).contains($0) ? $0 + 32 : $0 }
        return lower.elementsEqual("md".utf8) || lower.elementsEqual("markdown".utf8)
            || lower.elementsEqual("mdown".utf8)
    }
}

/// Both documents are immutable after the worker transfers this owner. `base`
/// has original prepared rows, while `document` has newly fitted display tiles.
/// A scope change borrows base rows and makes another private display document;
/// it never moves tiles that an older frame may still be drawing.
final class AtlasViewportPreparedSource: @unchecked Sendable {
    let rawPath: [UInt8]
    let base: AtlasDocument
    let document: AtlasDocument
    let cost: AtlasViewportCost
    let cachedMarkdown: AtlasDocument?
    let markdownRequested: Bool
    let preparation: AtlasViewportPreparation
    init(rawPath: [UInt8], base: AtlasDocument, document: AtlasDocument, cost: AtlasViewportCost,
         cachedMarkdown: AtlasDocument? = nil, markdownRequested: Bool = false,
         preparation: AtlasViewportPreparation = AtlasViewportPreparation()) {
        self.rawPath = rawPath; self.base = base; self.document = document; self.cost = cost
        self.cachedMarkdown = cachedMarkdown; self.markdownRequested = markdownRequested
        self.preparation = preparation
    }
}

final class AtlasViewportWork: @unchecked Sendable {
    let id = UUID()
    let root: String
    let layoutRevision: UUID
    let entry: AtlasProjectCatalog.Entry
    let sourceCacheHandle: UInt64
    let limits: AtlasViewportLimits
    let cancellation = AtlasSearchCancellation()
    let accessLease: AnyObject?
    let cacheLease: AnyObject?
    let reuse: AtlasViewportPreparedSource?
    let retirementLease: AtlasNativeRetirement.Lease
    let markdownRequested: Bool
    init(root: String, layoutRevision: UUID, entry: AtlasProjectCatalog.Entry,
         sourceCacheHandle: UInt64, limits: AtlasViewportLimits, accessLease: AnyObject?,
         cacheLease: AnyObject?, reuse: AtlasViewportPreparedSource?, retirementLease: AtlasNativeRetirement.Lease,
         markdownRequested: Bool = false) {
        self.root = root; self.layoutRevision = layoutRevision; self.entry = entry
        self.sourceCacheHandle = sourceCacheHandle; self.limits = limits
        self.accessLease = accessLease; self.cacheLease = cacheLease; self.reuse = reuse
        self.retirementLease = retirementLease
        self.markdownRequested = markdownRequested
    }
}

enum AtlasViewportWorkResult: Sendable {
    case prepared(AtlasViewportPreparedSource), refused(AtlasViewportPreviewState), canceled
}

/// One worker per residency model, with no internal backlog. A replacement
/// viewport updates the model's bounded demand list while this entered request
/// drains. Its root/cache leases survive cancellation and the foreign return.
final class AtlasViewportWorker: Sendable {
    private let queue = DispatchQueue(label: "dev.frankencode.browser.viewport-source", qos: .userInitiated)
    private let transport: AtlasViewportSourceTransport
    private let markdown: AtlasViewportMarkdownTransport?
    init(transport: AtlasViewportSourceTransport, markdown: AtlasViewportMarkdownTransport? = nil) {
        self.transport = transport; self.markdown = markdown
    }

    func submit(_ work: AtlasViewportWork,
                completion: @escaping @MainActor @Sendable (AtlasViewportWorkResult) -> Void) {
        queue.async { [transport, markdown] in
            let result = autoreleasepool { Self.prepare(work, transport: transport, markdown: markdown) }
            DispatchQueue.main.async {
                // Keep grants alive through delivery as well as foreign I/O.
                withExtendedLifetime(work) { completion(result) }
            }
        }
    }

    private static func prepare(_ work: AtlasViewportWork, transport: AtlasViewportSourceTransport,
                                markdown: AtlasViewportMarkdownTransport?) -> AtlasViewportWorkResult {
        guard !work.cancellation.isCanceled else { return .canceled }
        guard let path = work.entry.sourcePath, path.utf8.elementsEqual(work.entry.rawPath) else {
            return .refused(.readerOnly)
        }
        let base: AtlasDocument
        let reusedSource: Bool
        var artifacts: AtlasViewportArtifacts?
        var preparation = AtlasViewportPreparation()
        if let reuse = work.reuse, reuse.rawPath == work.entry.rawPath,
           reuse.base.source.path.utf8.elementsEqual(work.entry.rawPath) {
            base = reuse.base
            reusedSource = true
        } else {
            reusedSource = false
            guard work.entry.observedBytes <= UInt64(AtlasViewportLimits.sourceFileBytes) else {
                return .refused(.readerOnly)
            }
            let full = work.root + (work.root.hasSuffix("/") ? "" : "/") + path
            let packet: AtlasProjectSourcePacket
            do {
                guard let value = try transport.load(full, work.sourceCacheHandle, work.cancellation) else {
                    return work.cancellation.isCanceled ? .canceled : .refused(.unavailable)
                }
                packet = value
            } catch { return work.cancellation.isCanceled ? .canceled : .refused(.unavailable) }
            guard !work.cancellation.isCanceled else { return .canceled }
            guard packet.json.count <= AtlasViewportLimits.responseBytes else { return .refused(.readerOnly) }
            let wire: BoundedCapture
            do { wire = try JSONDecoder().decode(BoundedCapture.self, from: packet.json) }
            catch AtlasProjectIOError.limit { return .refused(.readerOnly) }
            catch { return .refused(.unavailable) }
            let capture = wire.capture, bytes = capture.text.utf8.count, units = capture.text.utf16.count
            guard bytes <= AtlasViewportLimits.sourceFileBytes else { return .refused(.readerOnly) }
            // Admit source/JSON/attributed-text, cold CTLines, replay arrays and
            // raster overlap BEFORE CoreText allocation. These conservative
            // native estimates supplement the exact glyph/run/pixel caps; they
            // are not a claim about AppKit's private or total process memory.
            let working = packet.json.count * 2 + units * 96 + capture.runs.count * 1024
                + AtlasViewportLimits.fileRasterPixels * 8 + 8 * 1024 * 1024
            guard working <= AtlasViewportLimits.preparationBytes else { return .refused(.readerOnly) }
            guard bytes <= work.limits.sourceBytes else { return .refused(.capacityLimited) }
            artifacts = AtlasViewportArtifacts(root: work.root, path: path, sourceKey: packet.key,
                handle: work.sourceCacheHandle, workingBytes: working, transport: transport.artifacts,
                cancellation: work.cancellation)
            guard !work.cancellation.isCanceled else { return .canceled }
            if let restored = artifacts?.loadGlyphs(path: path, capture: capture, limits: work.limits) {
                base = restored
            } else {
                guard !work.cancellation.isCanceled else { return .canceled }
                preparation.shapedDocuments += 1
                guard let cold = AtlasDocument(path: path, capture: capture, glyphLimit: work.limits.glyphs,
                    runLimit: work.limits.runs, tileLimit: work.limits.tiles,
                    canceled: { work.cancellation.isCanceled }) else {
                    return work.cancellation.isCanceled ? .canceled : .refused(.readerOnly)
                }
                guard cold.tiles.count <= work.limits.tiles else { return .refused(.readerOnly) }
                var tiles: [AtlasTextTile] = []
                tiles.reserveCapacity(cold.tiles.count)
                let bridgedSource = capture.text as NSString
                for (part, tile) in cold.tiles.enumerated() {
                    guard !work.cancellation.isCanceled else { return .canceled }
                    guard let nativeHeader = tile.header,
                          var header = AtlasPreparedLine(nativeHeader,
                            source: (path as NSString).lastPathComponent + (part > 0 ? " · \(part + 1)" : "")) else {
                        return .refused(.unavailable)
                    }
                    if header.hasColorGlyphs { header.retainedColorLine = nativeHeader }
                    var rows: [AtlasPreparedLine] = []
                    rows.reserveCapacity(tile.lines.count)
                    for line in tile.lines {
                        guard !work.cancellation.isCanceled else { return .canceled }
                        guard var row = autoreleasepool(invoking: { AtlasPreparedLine(line, source: bridgedSource) }) else {
                            return .refused(.unavailable)
                        }
                        if row.hasColorGlyphs { row.retainedColorLine = line }
                        rows.append(row)
                    }
                    tiles.append(AtlasTextTile(path: path, preparedLines: rows, header: header, sourceRange: tile.sourceRange))
                }
                base = AtlasDocument(source: cold.source, capture: capture, tiles: tiles)
                artifacts?.saveGlyphs(base)
            }
            // Force the only preparation-time lazy field before publication.
            _ = base.preparationCounts
        }
        guard !work.cancellation.isCanceled else { return .canceled }
        // The immutable base always contains original source rows. Preview is
        // a separate cache entry, never a substitute written into source-cache
        // artifacts. Scope changes and warm toggles reuse this exact capture.
        var cachedMarkdown = reusedSource ? work.reuse?.cachedMarkdown : nil
        if work.markdownRequested, cachedMarkdown == nil,
           !(reusedSource && work.reuse?.markdownRequested == true),
           base.source.text.utf8.count <= 64 * 1024, let markdown {
            if let prepared = markdown.prepare(base, work.limits, work.cancellation),
               prepared.presentation == .markdown,
               prepared.source.path.utf8.elementsEqual(base.source.path.utf8),
               prepared.capture.text.utf8.elementsEqual(base.capture.text.utf8),
               prepared.source.text.utf8.elementsEqual(base.source.text.utf8) {
                cachedMarkdown = prepared
                _ = prepared.preparationCounts
            }
        }
        guard !work.cancellation.isCanceled else { return .canceled }
        let parcel = CGRect(x: work.entry.x, y: work.entry.y, width: work.entry.width, height: work.entry.height)
        let sourceCounts = base.preparationCounts
        let sourceBytes = base.source.text.utf8.count
        var refusal: AtlasViewportPreviewState = .readerOnly
        func fit(_ preview: AtlasDocument?) -> (document: AtlasDocument, display: [AtlasTextTile], cost: AtlasViewportCost)? {
            guard !work.cancellation.isCanceled else { return nil }
            let previewCounts = preview?.preparationCounts ?? (glyphs: 0, runs: 0)
            let extraBytes = preview?.presentationBytes ?? 0
            guard sourceCounts.glyphs <= work.limits.glyphs, sourceCounts.runs <= work.limits.runs,
                  previewCounts.glyphs <= work.limits.glyphs - sourceCounts.glyphs,
                  previewCounts.runs <= work.limits.runs - sourceCounts.runs,
                  extraBytes >= 0, extraBytes <= work.limits.estimatedBytes else {
                refusal = .capacityLimited; return nil
            }
            let presented = work.markdownRequested ? (preview ?? base) : base
            let document = AtlasDocument(source: base.source, capture: base.capture, tiles: presented.tiles,
                presentation: presented.presentation, presentationBytes: presented.presentationBytes)
            let display: [AtlasTextTile]
            if base.source.text.isEmpty { document.displayTiles = []; display = [] }
            else {
                guard let fitted = AtlasParcelLayout.reflow(document, inside: parcel) else {
                    refusal = .readerOnly; return nil
                }
                display = fitted
            }
            let tileCount = base.tiles.count + (preview?.tiles.count ?? 0) + display.count
            let counts = (glyphs: sourceCounts.glyphs + previewCounts.glyphs,
                          runs: sourceCounts.runs + previewCounts.runs)
            let pixelBudget = min(AtlasViewportLimits.fileRasterPixels, work.limits.pixels)
            let estimatedWithoutRaster = sourceBytes * 6 + counts.glyphs * 40 + counts.runs * 256
                + tileCount * 1024 + extraBytes + 4096
            guard tileCount <= work.limits.tiles, display.isEmpty || display.count <= pixelBudget,
                  estimatedWithoutRaster <= work.limits.estimatedBytes else {
                refusal = .capacityLimited; return nil
            }
            let perTile = max(1, pixelBudget / max(1, display.count))
            // Only exact source glyphs use the persistent source raster key.
            // Optional rich Markdown owns a different native presentation.
            let restoredOrPrepared = presented.presentation == .source
                && (artifacts?.prepareRasters(display, pixelBudget: pixelBudget, perTile: perTile,
                    tileLimit: work.limits.tiles) ?? false)
            if !restoredOrPrepared {
                for tile in display {
                    guard !work.cancellation.isCanceled else { return nil }
                    tile.prepareRaster(pixelBudget: perTile); preparation.rasterizedTiles += 1
                }
            }
            let pixels = display.reduce(0) { $0 + ($1.raster.map { $0.width * $0.height } ?? 0) }
            let cost = AtlasViewportCost(sourceBytes: sourceBytes, glyphs: counts.glyphs, runs: counts.runs,
                tiles: tileCount, pixels: pixels, estimatedBytes: estimatedWithoutRaster + pixels * 4)
            guard cost.fits(work.limits) else { refusal = .capacityLimited; return nil }
            return (document, display, cost)
        }
        var fitted = fit(cachedMarkdown)
        if fitted == nil, cachedMarkdown != nil, !work.cancellation.isCanceled {
            // Optional rich detail may never displace affordable exact source.
            // One bounded fallback uses the same already accepted base; there
            // is no recapture, second parse, or retry loop on a later redraw.
            cachedMarkdown = nil
            fitted = fit(nil)
        }
        guard !work.cancellation.isCanceled else { return .canceled }
        guard let fitted else { return .refused(refusal) }
        let document = fitted.document, display = fitted.display, cost = fitted.cost
        _ = document.preparationCounts
        guard !work.cancellation.isCanceled else { return .canceled }
        if let artifacts {
            guard artifacts.environmentIsCurrent() else { return .canceled }
            let cached = artifacts.receipt
            preparation.glyphArchiveHit = cached.glyphArchiveHit
            preparation.rasterArchiveHit = cached.rasterArchiveHit
            preparation.rasterizedTiles += cached.rasterizedTiles
            preparation.restoredColorLines = cached.restoredColorLines
            preparation.artifactsWritten = cached.artifactsWritten
        }
        func storage(_ tile: AtlasTextTile) -> Any {
            (tile.lines, tile.header, tile.preparedLines, tile.preparedHeader, tile.raster, tile.display)
        }
        // Storage values contain no document/tile/source objects and cannot
        // point back to this lease. Scope clones may share their inner arrays;
        // both lifetimes remain conservatively charged until their final owner.
        let payload = (base.source.text, base.capture.runs, base.tiles.map(storage),
                       cachedMarkdown?.tiles.map(storage), display.map(storage))
        guard work.retirementLease.publish(storage: payload, bytes: cost.estimatedBytes) else { return .refused(.capacityLimited) }
        document.nativeResidencyOwner = work.retirementLease
        for tile in display { tile.nativeResidencyOwner = work.retirementLease }
        if let cachedMarkdown, cachedMarkdown.nativeResidencyOwner == nil {
            cachedMarkdown.nativeResidencyOwner = work.retirementLease
            for tile in cachedMarkdown.tiles { tile.nativeResidencyOwner = work.retirementLease }
        }
        if !reusedSource {
            base.nativeResidencyOwner = work.retirementLease
            base.source.nativeResidencyOwner = work.retirementLease
            for tile in base.tiles { tile.nativeResidencyOwner = work.retirementLease }
        }
        return .prepared(AtlasViewportPreparedSource(rawPath: work.entry.rawPath, base: base,
            document: document, cost: cost, cachedMarkdown: cachedMarkdown,
            markdownRequested: work.markdownRequested, preparation: preparation))
    }

    private struct BoundedCapture: Decodable {
        let capture: AtlasHighlightCapture
        private enum CodingKeys: CodingKey { case schema, text, runs }
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            let schema = try c.decode(String.self, forKey: .schema)
            let text = try c.decode(String.self, forKey: .text)
            guard schema == "fcb.source-document/1", text.utf8.count <= AtlasViewportLimits.sourceFileBytes else {
                throw AtlasProjectIOError.limit
            }
            var input = try c.nestedUnkeyedContainer(forKey: .runs), runs: [AtlasHighlightCapture.Run] = []
            if let count = input.count, count > AtlasViewportLimits.captureRuns { throw AtlasProjectIOError.limit }
            var cursor = 0
            let length = text.utf16.count
            while !input.isAtEnd {
                guard runs.count < AtlasViewportLimits.captureRuns else { throw AtlasProjectIOError.limit }
                let run = try input.decode(AtlasHighlightCapture.Run.self)
                guard let start = Int(run.start), String(start) == run.start, start == cursor,
                      let count = Int(run.length), String(count) == run.length, count > 0,
                      count <= length - cursor, run.role.utf8.count <= 32 else { throw AtlasProjectIOError.invalidResponse }
                cursor += count; runs.append(run)
            }
            guard cursor == length else { throw AtlasProjectIOError.invalidResponse }
            capture = AtlasHighlightCapture(schema: schema, text: text, runs: runs)
        }
    }
}

