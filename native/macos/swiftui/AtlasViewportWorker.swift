import Foundation
import CoreGraphics
import CoreText
import Dispatch

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
    case cold, queued, loading, ready, empty, readerOnly, unavailable, capacityLimited
    var label: String {
        switch self {
        case .cold: return "Zoom in to load source"
        case .queued: return "Source preview queued"
        case .loading: return "Loading source preview"
        case .ready: return "Captured source preview"
        case .empty: return "Empty captured file"
        case .readerOnly: return "Preview limit — open in reader"
        case .unavailable: return "Preview unavailable — open in reader"
        case .capacityLimited: return "Preview memory full — open in reader"
        }
    }
}

/// The production transport performs only the existing Rust capture/highlight
/// operation. The source cache owns no native documents or font state here.
struct AtlasViewportSourceTransport: Sendable {
    let load: @Sendable (String, UInt64, AtlasSearchCancellation) throws -> AtlasProjectSourcePacket?
    static let live = Self(load: { path, handle, cancellation in
        try AtlasProjectWorker.source(path: path, handle: handle, cancellation: cancellation)
    })
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
    init(rawPath: [UInt8], base: AtlasDocument, document: AtlasDocument, cost: AtlasViewportCost) {
        self.rawPath = rawPath; self.base = base; self.document = document; self.cost = cost
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
    init(root: String, layoutRevision: UUID, entry: AtlasProjectCatalog.Entry,
         sourceCacheHandle: UInt64, limits: AtlasViewportLimits, accessLease: AnyObject?,
         cacheLease: AnyObject?, reuse: AtlasViewportPreparedSource?, retirementLease: AtlasNativeRetirement.Lease) {
        self.root = root; self.layoutRevision = layoutRevision; self.entry = entry
        self.sourceCacheHandle = sourceCacheHandle; self.limits = limits
        self.accessLease = accessLease; self.cacheLease = cacheLease; self.reuse = reuse
        self.retirementLease = retirementLease
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
    init(transport: AtlasViewportSourceTransport) { self.transport = transport }

    func submit(_ work: AtlasViewportWork,
                completion: @escaping @MainActor @Sendable (AtlasViewportWorkResult) -> Void) {
        queue.async { [transport] in
            let result = autoreleasepool { Self.prepare(work, transport: transport) }
            DispatchQueue.main.async {
                // Keep grants alive through delivery as well as foreign I/O.
                withExtendedLifetime(work) { completion(result) }
            }
        }
    }

    private static func prepare(_ work: AtlasViewportWork, transport: AtlasViewportSourceTransport) -> AtlasViewportWorkResult {
        guard !work.cancellation.isCanceled else { return .canceled }
        guard let path = work.entry.sourcePath, path.utf8.elementsEqual(work.entry.rawPath) else {
            return .refused(.readerOnly)
        }
        let base: AtlasDocument
        let reusedSource: Bool
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
            // Force the only preparation-time lazy field before publication.
            _ = base.preparationCounts
        }
        guard !work.cancellation.isCanceled else { return .canceled }
        let document = AtlasDocument(source: base.source, capture: base.capture, tiles: base.tiles)
        let parcel = CGRect(x: work.entry.x, y: work.entry.y, width: work.entry.width, height: work.entry.height)
        let display: [AtlasTextTile]
        if base.source.text.isEmpty { document.displayTiles = []; display = [] }
        else {
            guard let fitted = AtlasParcelLayout.reflow(document, inside: parcel) else { return .refused(.readerOnly) }
            display = fitted
        }
        guard display.count <= work.limits.tiles, !work.cancellation.isCanceled else {
            return work.cancellation.isCanceled ? .canceled : .refused(.readerOnly)
        }
        let pixelBudget = min(AtlasViewportLimits.fileRasterPixels, work.limits.pixels)
        guard display.isEmpty || display.count <= pixelBudget else { return .refused(.capacityLimited) }
        let perTile = max(1, pixelBudget / max(1, display.count))
        for tile in display {
            guard !work.cancellation.isCanceled else { return .canceled }
            tile.prepareRaster(pixelBudget: perTile)
        }
        let counts = base.preparationCounts
        let pixels = display.reduce(0) { $0 + ($1.raster.map { $0.width * $0.height } ?? 0) }
        let sourceBytes = base.source.text.utf8.count
        let tileCount = base.tiles.count + display.count
        let estimated = sourceBytes * 6 + counts.glyphs * 40 + counts.runs * 256
            + tileCount * 1024 + pixels * 4 + 4096
        let cost = AtlasViewportCost(sourceBytes: sourceBytes, glyphs: counts.glyphs, runs: counts.runs,
            tiles: tileCount, pixels: pixels, estimatedBytes: estimated)
        guard cost.fits(work.limits) else { return .refused(.capacityLimited) }
        _ = document.preparationCounts
        guard !work.cancellation.isCanceled else { return .canceled }
        func storage(_ tile: AtlasTextTile) -> Any {
            (tile.lines, tile.header, tile.preparedLines, tile.preparedHeader, tile.raster)
        }
        // Storage values contain no document/tile/source objects and cannot
        // point back to this lease. Scope clones may share their inner arrays;
        // both lifetimes remain conservatively charged until their final owner.
        let payload = (base.source.text, base.capture.runs, base.tiles.map(storage), display.map(storage))
        guard work.retirementLease.publish(storage: payload, bytes: estimated) else { return .refused(.capacityLimited) }
        document.nativeResidencyOwner = work.retirementLease
        for tile in display { tile.nativeResidencyOwner = work.retirementLease }
        if !reusedSource {
            base.nativeResidencyOwner = work.retirementLease
            base.source.nativeResidencyOwner = work.retirementLease
            for tile in base.tiles { tile.nativeResidencyOwner = work.retirementLease }
        }
        return .prepared(AtlasViewportPreparedSource(rawPath: work.entry.rawPath, base: base, document: document, cost: cost))
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
