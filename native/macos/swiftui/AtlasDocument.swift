import SwiftUI
import AppKit
import CoreText
import os
import Dispatch

/// Every native preview reserves a finite lifetime slot and byte allowance
/// before preparation. Its payload remains charged after model eviction, while
/// an accepted frame, GPU flight, or selected source still owns the lease.
/// Final release uses that already reserved slot; a single coalesced drain
/// destroys payloads off-main before returning admission credit.
final class AtlasNativeRetirement: @unchecked Sendable {
    private final class Payload: @unchecked Sendable {
        let value: Any
        init(_ value: Any) { self.value = value }
    }
    final class Lease: @unchecked Sendable {
        private let pool: AtlasNativeRetirement
        private let slot: Int
        private var payload: Payload?
        fileprivate init(pool: AtlasNativeRetirement, slot: Int) { self.pool = pool; self.slot = slot }

        /// Called once by the private preparation worker before publication.
        /// The payload must contain storage values, never an object that itself
        /// owns this lease. Shrinking preserves admission until final release.
        func publish(storage: Any, bytes: Int) -> Bool {
            guard payload == nil, pool.shrink(slot: slot, bytes: bytes) else { return false }
            payload = Payload(storage)
            return true
        }
        deinit { pool.enqueue(slot: slot, payload: payload) }
    }

    private let lock = NSLock()
    private let queue = DispatchQueue(label: "dev.frankencode.browser.native-retirement", qos: .utility)
    private let maximumBytes: Int
    private var free: [Int]
    private var charges: [Int: Int] = [:]
    private var pending: [(Int, Payload?)] = []
    private var ready: [(Int, Payload?)] = []
    private var handoffScheduled = false
    private var draining = false
    private var bytes = 0
    private var wake: (@Sendable () -> Void)?

    init(maximumSlots: Int, maximumBytes: Int) {
        precondition(maximumSlots > 0 && maximumSlots <= 520 && maximumBytes > 0)
        self.maximumBytes = maximumBytes; free = Array(0..<maximumSlots)
        pending.reserveCapacity(maximumSlots)
        ready.reserveCapacity(maximumSlots)
    }
    var reservedBytes: Int { lock.lock(); defer { lock.unlock() }; return bytes }
    var reservedSlots: Int { lock.lock(); defer { lock.unlock() }; return charges.count }
    func onAvailability(_ callback: @escaping @Sendable () -> Void) {
        lock.lock(); wake = callback; lock.unlock()
    }
    func reserve(bytes requested: Int) -> Lease? {
        lock.lock(); defer { lock.unlock() }
        guard requested > 0, requested <= maximumBytes - bytes, let slot = free.popLast() else { return nil }
        charges[slot] = requested; bytes += requested
        return Lease(pool: self, slot: slot)
    }
    private func shrink(slot: Int, bytes actual: Int) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard let old = charges[slot], actual >= 0, actual <= old else { return false }
        charges[slot] = actual; bytes -= old - actual
        return true
    }
    private func enqueue(slot: Int, payload: Payload?) {
        lock.lock()
        precondition(charges[slot] != nil)
        pending.append((slot, payload))
        let start = !handoffScheduled
        handoffScheduled = true
        lock.unlock()
        if start {
            // Let any main-thread owner finish its stored-property teardown
            // before releasing the payload's final shared array references.
            // This is one coalesced handoff, not one closure per tile or job.
            DispatchQueue.main.async { [self] in promote() }
        }
    }
    private func promote() {
        lock.lock()
        ready.append(contentsOf: pending); pending.removeAll(keepingCapacity: true)
        handoffScheduled = false
        let start = !draining && !ready.isEmpty
        if start { draining = true }
        lock.unlock()
        if start { queue.async { [self] in drain() } }
    }
    private func drain() {
        while true {
            lock.lock()
            guard !ready.isEmpty else {
                draining = false
                let callback = wake
                lock.unlock()
                callback?()
                return
            }
            let slot = ready[ready.count - 1].0
            var payload = ready.removeLast().1
            lock.unlock()
            // Force final storage release here, before making the reservation
            // reusable. Nested leases may enqueue into their own reserved slots.
            withExtendedLifetime(payload) {}
            payload = nil
            lock.lock()
            if let charge = charges.removeValue(forKey: slot) { bytes -= charge }
            free.append(slot)
            lock.unlock()
        }
    }
}

enum Monokai {
    static let background = NSColor(srgbRed: 0x16 / 255.0, green: 0x1A / 255.0, blue: 0x1D / 255.0, alpha: 1)
    static func color(_ role: String) -> NSColor {
        let value: UInt32
        switch role {
        case "keyword", "operator": value = 0xF92672
        case "type": value = 0x66D9EF
        case "func", "function": value = 0xA6E22E
        case "str", "string": value = 0xE6DB74
        case "number": value = 0xAE81FF
        case "comment": value = 0x75715E
        default: value = 0xF8F8F2
        }
        return NSColor(srgbRed: Double((value >> 16) & 255) / 255,
                       green: Double((value >> 8) & 255) / 255,
                       blue: Double(value & 255) / 255, alpha: 1)
    }
}

struct AtlasHighlightCapture: Decodable {
    struct Run: Decodable { let start: String; let length: String; let role: String }
    let schema: String
    let text: String
    let runs: [Run]

    func validatedRuns() -> [(NSRange, String)]? {
        guard schema == "fcb.source-document/1" else { return nil }
        let length = text.utf16.count
        var cursor = 0
        var result: [(NSRange, String)] = []
        for run in runs {
            guard let start = Int(run.start), let count = Int(run.length),
                  start == cursor, count > 0, count <= length - cursor else { return nil }
            result.append((NSRange(location: start, length: count), run.role))
            cursor += count
        }
        return cursor == length ? result : nil
    }
}

/// Cached platform-shaped lines. Wheel/drag never tokenize or shape source.
final class AtlasTextTile {
    private static let renderMutationClock = OSAllocatedUnfairLock(initialState: UInt64(0))
    static var renderMutationEpoch: UInt64 { renderMutationClock.withLock { $0 } }
    private func noteRenderMutation() { Self.renderMutationClock.withLock { $0 &+= 1 } }
    static let width = 648.0
    static let lineHeight = 16.0
    let path: String
    let lines: [CTLine]
    let header: CTLine?
    var preparedLines: [AtlasPreparedLine]? { didSet { noteRenderMutation() } }
    var preparedHeader: AtlasPreparedLine? { didSet { noteRenderMutation() } }
    let sourceRange: NSRange
    var rect = CGRect.zero { didSet { noteRenderMutation() } }
    var parcelRect: CGRect? { didSet { noteRenderMutation() } }
    var parcelFirst = false { didSet { noteRenderMutation() } }
    var contentScale: Double { rect.width / Self.width }
    var raster: CGImage? { didSet { noteRenderMutation() } }
    // Set only during private preparation. A retained frame owns this credit
    // for as long as it can draw the tile, independently of the model's LRU.
    var nativeResidencyOwner: AtlasNativeRetirement.Lease?
    var lineCount: Int { preparedLines?.count ?? lines.count }
    var height: Double { 22 + Double(lineCount) * Self.lineHeight }

    init(path: String, lines: [CTLine], sourceRange: NSRange, part: Int, retainedHeader: CTLine? = nil) {
        self.path = path
        self.lines = lines
        self.sourceRange = sourceRange
        let title = (path as NSString).lastPathComponent + (part > 0 ? " · \(part + 1)" : "")
        header = retainedHeader ?? Self.makeHeader(title)
    }

    static func makeHeader(_ title: String) -> CTLine {
        CTLineCreateWithAttributedString(NSAttributedString(string: title, attributes: [
            .font: NSFont.monospacedSystemFont(ofSize: 10, weight: .medium),
            .foregroundColor: Monokai.color("comment")
        ]))
    }

    init(path: String, preparedLines: [AtlasPreparedLine], header: AtlasPreparedLine, sourceRange: NSRange) {
        self.path = path
        self.lines = []
        self.header = nil
        self.preparedLines = preparedLines
        self.preparedHeader = header
        self.sourceRange = sourceRange
    }

    /// Both cached overview and close-up use these exact same colored glyphs.
    func draw(in context: CGContext, visibleRows: Range<Int>? = nil) {
        context.textMatrix = CGAffineTransform(scaleX: 1, y: -1)
        context.textPosition = CGPoint(x: 4, y: 12)
        if let header { CTLineDraw(header, context) }
        else { preparedHeader?.draw(in: context, origin: CGPoint(x: 4, y: 12)) }
        for index in visibleRows ?? (0..<lineCount) {
            context.textPosition = CGPoint(x: 4, y: 34 + Double(index) * Self.lineHeight)
            if let preparedLines {
                preparedLines[index].draw(in: context, origin: context.textPosition)
            } else { CTLineDraw(lines[index], context) }
        }
    }

    func prepareRaster(pixelBudget: Int) {
        let budget = max(1, pixelBudget)
        let scale = min(1, sqrt(Double(budget) / (Self.width * height)))
        let width = max(1, min(budget, Int(Self.width * scale)))
        let height = max(1, min(budget / width, Int(self.height * scale)))
        guard let bitmap = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
            bytesPerRow: width * 4, space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return }
        bitmap.translateBy(x: 0, y: Double(height))
        bitmap.scaleBy(x: Double(width) / Self.width, y: -Double(height) / self.height)
        draw(in: bitmap)
        raster = bitmap.makeImage()
    }
}

final class AtlasDocument {
    let source: AtlasSource
    let capture: AtlasHighlightCapture
    let tiles: [AtlasTextTile]
    // Original archive pages never change. Presentation may repartition their
    // already-shaped rows into balanced columns for the current parcel.
    var displayTiles: [AtlasTextTile]?
    var renderTiles: [AtlasTextTile] { displayTiles ?? tiles }
    let restoredColorLineCount: Int
    var nativeResidencyOwner: AtlasNativeRetirement.Lease?
    lazy var preparationCounts: (glyphs: Int, runs: Int) = {
        var glyphs = 0, runs = 0
        for tile in tiles {
            if let lines = tile.preparedLines {
                for line in lines + [tile.preparedHeader].compactMap({ $0 }) {
                    runs += line.runs.count
                    glyphs += line.runs.reduce(0) { $0 + $1.glyphs.count }
                    if let retained = line.retainedColorLine {
                        glyphs += CTLineGetGlyphCount(retained)
                        runs += CFArrayGetCount(CTLineGetGlyphRuns(retained))
                    }
                }
            } else {
                for line in tile.lines + [tile.header].compactMap({ $0 }) {
                    glyphs += CTLineGetGlyphCount(line)
                    runs += CFArrayGetCount(CTLineGetGlyphRuns(line))
                }
            }
        }
        return (glyphs, runs)
    }()

    // Reader presentation is needed only for selection; opening the atlas does
    // not construct a second attributed copy of every source file.
    lazy var styledSource: AttributedString = AttributedString(AtlasDocument.style(capture))

    static func style(_ capture: AtlasHighlightCapture) -> NSMutableAttributedString {
        style(text: capture.text, runs: capture.validatedRuns() ?? [])
    }

    /// Both whole-source and retained-page readers use these presentation
    /// attributes. Callers supply already validated, page-local UTF-16 runs;
    /// styling never tokenizes, normalizes, decorates, or reads the source.
    static func style(text: String, runs: [(NSRange, String)]) -> NSMutableAttributedString {
        let paragraph = NSMutableParagraphStyle()
        paragraph.defaultTabInterval = 8 * 7.83
        let styled = NSMutableAttributedString(string: text, attributes: [
            .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular),
            .paragraphStyle: paragraph,
            .foregroundColor: Monokai.color("plain")
        ])
        for (range, role) in runs {
            styled.addAttribute(.foregroundColor, value: Monokai.color(role), range: range)
        }
        return styled
    }

    init(source: AtlasSource, capture: AtlasHighlightCapture, tiles: [AtlasTextTile], restoredColorLineCount: Int = 0) {
        self.restoredColorLineCount = restoredColorLineCount
        self.source = source
        self.capture = capture
        self.tiles = tiles
    }

    init?(path: String, capture: AtlasHighlightCapture, glyphLimit: Int = 16 * 1024 * 1024,
          runLimit: Int = 1024 * 1024, tileLimit: Int = Int.max, canceled: () -> Bool = { false }) {
        guard glyphLimit >= 0, runLimit >= 0, tileLimit >= 0, capture.text.utf16.count <= glyphLimit,
              capture.validatedRuns() != nil, !canceled() else { return nil }
        self.capture = capture
        restoredColorLineCount = 0
        source = AtlasSource(path: path, text: capture.text)
        let styled = Self.style(capture)
        let typesetter = CTTypesetterCreateWithAttributedString(styled)
        var result: [AtlasTextTile] = []
        var lines: [CTLine] = []
        var offset = 0
        var start = 0
        var remainingGlyphs = glyphLimit, remainingRuns = runLimit
        while offset < styled.length {
            // Newline-heavy captures can have many rows with few glyphs.
            // Refuse before shaping another tile, never return a text prefix.
            guard result.count < tileLimit, !canceled() else { return nil }
            let count = CTTypesetterSuggestLineBreak(typesetter, offset, AtlasTextTile.width - 8)
            guard count > 0, count <= styled.length - offset else { return nil }
            let line = CTTypesetterCreateLine(typesetter, CFRange(location: offset, length: count))
            let glyphs = CTLineGetGlyphCount(line), runs = CFArrayGetCount(CTLineGetGlyphRuns(line))
            guard glyphs <= remainingGlyphs, runs <= remainingRuns else { return nil }
            remainingGlyphs -= glyphs; remainingRuns -= runs
            lines.append(line)
            offset += count
            if lines.count == 80 || offset == styled.length {
                let tile = AtlasTextTile(path: path, lines: lines,
                    sourceRange: NSRange(location: start, length: offset - start), part: result.count)
                let headerGlyphs = tile.header.map { CTLineGetGlyphCount($0) } ?? 0
                let headerRuns = tile.header.map { CFArrayGetCount(CTLineGetGlyphRuns($0)) } ?? 0
                guard headerGlyphs <= remainingGlyphs, headerRuns <= remainingRuns else { return nil }
                remainingGlyphs -= headerGlyphs; remainingRuns -= headerRuns
                result.append(tile)
                lines = []
                start = offset
            }
        }
        tiles = result
    }
}
