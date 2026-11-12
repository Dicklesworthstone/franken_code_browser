import Foundation
import Dispatch
#if canImport(CoreGraphics)
import CoreGraphics
#endif
#if canImport(ImageIO)
import ImageIO
#endif
#if canImport(CryptoKit)
import CryptoKit
#endif

/// A bounded consumer of the shared engine's already-shaped display list.
/// These coordinates, glyph IDs and source regions are never reconstructed by
/// parsing Markdown or reshaping its strings in the native host.
enum AtlasDocumentDisplayLimits {
    static let pageItems: UInt64 = 128
    static let items: UInt64 = 8192
    static let pageGlyphs = 16_384
    static let runGlyphs = 4096
    static let fonts = 6
    static let fontBytes: UInt64 = 8 * 1024 * 1024
    static let fontChunk: UInt64 = 64 * 1024
}

struct AtlasDocumentDisplayRect: Decodable, Equatable, Sendable {
    let x, y, width, height: Double
    var maxX: Double { x + width }
    var maxY: Double { y + height }
    var valid: Bool {
        [x, y, width, height, maxX, maxY].allSatisfy { $0.isFinite && abs($0) <= 10_000_000 }
            && width >= 0 && height >= 0
    }
}

struct AtlasDocumentDisplayFontSpec: Equatable, Sendable {
    let index: UInt64
    let bytes: UInt64
}

/// Immutable faces are constructed on the existing reader worker, before
/// publication. Drawing uses these exact bytes, not a similarly named system
/// font. The immutable CGFont and its retained provider have no UI ownership.
final class AtlasDocumentDisplayFont: @unchecked Sendable {
    let index: UInt64
    let data: Data
#if canImport(CoreGraphics)
    let face: CGFont
#endif
    init(index: UInt64, data: Data) throws {
        guard !data.isEmpty, data.count <= Int(AtlasDocumentDisplayLimits.fontBytes) else {
            throw AtlasReaderError.invalidResponse
        }
        self.index = index
        self.data = data
#if canImport(CoreGraphics)
        guard let provider = CGDataProvider(data: data as CFData), let face = CGFont(provider),
              face.numberOfGlyphs > 0 else { throw AtlasReaderError.invalidResponse }
        self.face = face
#endif
    }
    func contains(glyph: UInt16) -> Bool {
#if canImport(CoreGraphics)
        Int(glyph) < face.numberOfGlyphs
#else
        // Portable wire tests do not claim native font validation.
        true
#endif
    }
}

struct AtlasDocumentDisplayInventory: Sendable {
    let document: AtlasReaderDocumentInventory
    let width: UInt64
    let bounds: AtlasDocumentDisplayRect
    let totalItems: UInt64
    let unresolvedImages: UInt64
    let qualifiedSources: Bool
    let fonts: [AtlasDocumentDisplayFontSpec]
    func matches(_ other: Self) -> Bool {
        document.matches(other.document) && width == other.width && bounds == other.bounds
            && totalItems == other.totalItems && unresolvedImages == other.unresolvedImages
            && qualifiedSources == other.qualifiedSources && fonts == other.fonts
    }
}

struct AtlasDocumentDisplayGlyph: Sendable {
    let id: UInt16
    let font: UInt64
    let x, y: Double
}

struct AtlasDocumentDisplayItem: Identifiable, Sendable {
    enum Content: Sendable {
        case text(String, role: String, size: Double, glyphs: [AtlasDocumentDisplayGlyph])
        case vector(shape: String, role: String, stroke: Double)
        case image(alt: String)
        case anchor(String, heading: Bool, level: UInt64)
    }
    let id: UInt64
    let bounds: AtlasDocumentDisplayRect
    let original: AtlasDocumentByteRange?
    let content: Content
    let imageReference: AtlasDocumentDisplayImageReference?
    init(id: UInt64, bounds: AtlasDocumentDisplayRect, original: AtlasDocumentByteRange?,
         content: Content, imageReference: AtlasDocumentDisplayImageReference? = nil) {
        self.id = id; self.bounds = bounds; self.original = original
        self.content = content; self.imageReference = imageReference
    }
    var readingText: String? {
        switch content {
        case .text(let text, _, _, _): text
        case .image(let alt): alt.isEmpty ? "Image" : "Image: " + alt
        case .vector, .anchor: nil
        }
    }
}

struct AtlasDocumentDisplayPage: Sendable {
    let id: UUID
    let inventory: AtlasDocumentDisplayInventory
    let first: UInt64
    let items: [AtlasDocumentDisplayItem]
    let next: UInt64?
    private(set) var images: [AtlasDocumentDisplayImage] = []
    private(set) var imageRevision = UUID()
    private(set) var imageNotice: String?
    init(id: UUID, inventory: AtlasDocumentDisplayInventory, first: UInt64,
         items: [AtlasDocumentDisplayItem], next: UInt64?) {
        self.id = id; self.inventory = inventory; self.first = first
        self.items = items; self.next = next
    }
    /// Page geometry is a slice of the upstream coordinate system. Pages may
    /// overlap vertically when many items occupy the same table/paragraph.
    var top: Double { items.map(\.bounds.y).min() ?? inventory.bounds.y }
    var bottom: Double { items.map(\.bounds.maxY).max() ?? top }
    var imageTargets: [AtlasDocumentDisplayImageTarget] { items.compactMap(imageTarget) }
    func imageTarget(_ item: AtlasDocumentDisplayItem) -> AtlasDocumentDisplayImageTarget? {
        guard inventory.qualifiedSources, case .image = item.content, let reference = item.imageReference,
              let original = item.original,
              original.valid(upTo: inventory.document.identity.capturedBytes, nonempty: true),
              original.start >= inventory.document.sourceBase,
              item.bounds.width > 0, item.bounds.height > 0 else { return nil }
        return .init(inventory: inventory, itemIndex: item.id, requestID: reference.requestID,
            destination: reference.destination, bounds: item.bounds, original: item.original)
    }
    /// A page owns bounded decoded resources. No pixel buffer can attach to a
    /// different capture, document generation, width or parser asset reference.
    func withImages(_ images: [AtlasDocumentDisplayImage], notice: String? = nil) throws -> Self {
        guard images.count <= AtlasDocumentImageLimits.pageImages,
              notice.map({ $0.utf8.count <= 2048 }) ?? true else {
            throw AtlasReaderError.invalidResponse
        }
        var bytes = 0, indexes = Set<UInt64>()
        for image in images {
            guard indexes.insert(image.target.itemIndex).inserted,
                  let item = items.first(where: { $0.id == image.target.itemIndex }),
                  let target = imageTarget(item), target.matches(image.target),
                  image.byteCount <= AtlasDocumentImageLimits.pageBytes - bytes else {
                throw AtlasReaderError.invalidResponse
            }
            bytes += image.byteCount
        }
        var page = self
        page.images = images; page.imageRevision = UUID(); page.imageNotice = notice
        return page
    }
    func image(for item: AtlasDocumentDisplayItem) -> AtlasDocumentDisplayImage? {
        guard let target = imageTarget(item) else { return nil }
        return images.first { $0.target.matches(target) }
    }
    func validateFonts(_ fonts: [AtlasDocumentDisplayFont]) throws {
        guard fonts.count == inventory.fonts.count else { throw AtlasReaderError.invalidResponse }
        for spec in inventory.fonts {
            guard let font = fonts.first(where: { $0.index == spec.index }),
                  UInt64(font.data.count) == spec.bytes else { throw AtlasReaderError.invalidResponse }
        }
        for item in items {
            if case .text(_, _, _, let glyphs) = item.content {
                for glyph in glyphs {
                    guard let font = fonts.first(where: { $0.index == glyph.font }),
                          font.contains(glyph: glyph.id) else { throw AtlasReaderError.invalidResponse }
                }
            }
        }
    }
}

/// A parser-owned asset reference. Destination text never supplies a root grant.
struct AtlasDocumentDisplayImageReference: Sendable {
    let requestID: UInt64
    let destination: String
}
struct AtlasDocumentDisplayImageTarget: Sendable {
    let inventory: AtlasDocumentDisplayInventory
    let itemIndex: UInt64
    let requestID: UInt64
    let destination: String
    let bounds: AtlasDocumentDisplayRect
    let original: AtlasDocumentByteRange?
    func matches(_ other: Self) -> Bool {
        inventory.matches(other.inventory) && itemIndex == other.itemIndex
            && requestID == other.requestID && destination.utf8.elementsEqual(other.destination.utf8)
            && bounds == other.bounds && original == other.original
    }
}
enum AtlasDocumentImageLimits {
    static let encodedBytes = 4 * 1024 * 1024
    static let encodedCacheBytes: UInt64 = 8 * 1024 * 1024
    static let chunkBytes: UInt64 = 64 * 1024
    static let sourceDimension = 8192
    static let sourcePixels = 16 * 1024 * 1024
    // At most eight bits per component: a source RGBA raster is at most 64 MiB.
    // Codec-private working memory is separate from retained thumbnail bytes;
    // these input/output bounds are not a total-process memory guarantee.
    static let sourceRasterBytes = sourcePixels * 4
    static let thumbnailDimension = 1024
    static let thumbnailBytes = thumbnailDimension * thumbnailDimension * 4
    static let pageImages = 16
    static let pageBytes = 16 * 1024 * 1024
    static let retainedBytes = 128 * 1024 * 1024
    static let retainedImages = 256
}
enum AtlasDocumentImageError: Error, Equatable {
    case unavailable, wrongExecutor, unsupported, invalidImage, resourceLimit
}
enum AtlasDocumentImageFormat: String, Sendable { case png, jpeg }

/// A reservation follows its immutable pixels through off-thread retirement.
/// Both queued retirement records and resident/retiring pixels have fixed caps.
fileprivate final class AtlasDocumentImageBudget: @unchecked Sendable {
    static let shared = AtlasDocumentImageBudget()
    private let lock = NSLock()
    private let queue = DispatchQueue(label: "dev.frankencode.document-image-retirement", qos: .utility)
    private var bytes = 0, images = 0
    func reserve(_ count: Int) throws -> AtlasDocumentImageReservation {
        lock.lock(); defer { lock.unlock() }
        guard count > 0, count <= AtlasDocumentImageLimits.retainedBytes - bytes,
              images < AtlasDocumentImageLimits.retainedImages else {
            throw AtlasDocumentImageError.resourceLimit
        }
        bytes += count; images += 1
        return AtlasDocumentImageReservation(budget: self, bytes: count)
    }
    func release(_ count: Int) {
        lock.lock(); defer { lock.unlock() }
        bytes -= count; images -= 1
    }
    func retire(_ work: @escaping @Sendable () -> Void) { queue.async(execute: work) }
}
fileprivate final class AtlasDocumentImageReservation: @unchecked Sendable {
    private let budget: AtlasDocumentImageBudget
    private let bytes: Int
    // The constructing worker transfers sole release authority to the
    // retirement payload. No shared caller can release this token concurrently.
    private var active = true
    init(budget: AtlasDocumentImageBudget, bytes: Int) { self.budget = budget; self.bytes = bytes }
    func close() {
        guard active else { return }
        active = false; budget.release(bytes)
    }
    deinit { close() }
}
#if canImport(CoreGraphics)
private final class AtlasDocumentImageStorage: @unchecked Sendable {
    private(set) var image: CGImage?
    private let reservation: AtlasDocumentImageReservation
    init(image: CGImage, reservation: AtlasDocumentImageReservation) {
        self.image = image; self.reservation = reservation
    }
    func retire() {
        // Clear the last privately owned pixel reference before reopening
        // admission. Drawing keeps the outer resource alive until it returns.
        image = nil
        reservation.close()
    }
}
#endif

/// The compressed bytes and ImageIO source are released on the worker. The only
/// native resource retained here is a CGImage over canonical sRGB RGBA pixels;
/// drawing never runs a codec or accesses a source pathname.
final class AtlasDocumentDisplayImage: @unchecked Sendable {
    let target: AtlasDocumentDisplayImageTarget
    let format: AtlasDocumentImageFormat
    let sourceWidth: Int
    let sourceHeight: Int
    let width: Int
    let height: Int
    let byteCount: Int
#if canImport(CoreGraphics)
    private let storage: AtlasDocumentImageStorage
#endif
    fileprivate init(target: AtlasDocumentDisplayImageTarget, format: AtlasDocumentImageFormat,
                     sourceWidth: Int, sourceHeight: Int, width: Int, height: Int, rgba: Data,
                     reservation: AtlasDocumentImageReservation) throws {
        guard width > 0, height > 0, width <= AtlasDocumentImageLimits.thumbnailDimension,
              height <= AtlasDocumentImageLimits.thumbnailDimension,
              rgba.count == width * height * 4 else { throw AtlasDocumentImageError.invalidImage }
        self.target = target; self.format = format
        self.sourceWidth = sourceWidth; self.sourceHeight = sourceHeight
        self.width = width; self.height = height; byteCount = rgba.count
#if canImport(CoreGraphics)
        guard let colorSpace = CGColorSpace(name: CGColorSpace.sRGB),
              let provider = CGDataProvider(data: rgba as CFData),
              let image = CGImage(width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32,
                  bytesPerRow: width * 4, space: colorSpace,
                  bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue
                      | CGBitmapInfo.byteOrder32Big.rawValue), provider: provider, decode: nil,
                  shouldInterpolate: true, intent: .defaultIntent) else {
            throw AtlasDocumentImageError.resourceLimit
        }
        storage = AtlasDocumentImageStorage(image: image, reservation: reservation)
#endif
    }
    deinit {
#if canImport(CoreGraphics)
        let storage = storage
        AtlasDocumentImageBudget.shared.retire { storage.retire() }
#endif
    }
#if canImport(CoreGraphics)
    /// The caller's coordinate system is the document's top-left, positive-Y
    /// down space. A local flip presents the source pixels upright in that slot.
    func draw(in context: CGContext, slot: AtlasDocumentDisplayRect) {
        guard slot.valid, slot.width > 0, slot.height > 0, let image = storage.image else { return }
        let box = fittedBounds(in: slot)
        context.saveGState()
        context.clip(to: CGRect(x: slot.x, y: slot.y, width: slot.width, height: slot.height))
        context.interpolationQuality = .high
        context.translateBy(x: box.x, y: box.maxY)
        context.scaleBy(x: 1, y: -1)
        context.draw(image, in: CGRect(x: 0, y: 0, width: box.width, height: box.height))
        context.restoreGState()
        // Keep the retirement lease alive through the final synchronous draw,
        // including under optimized ARC lifetime shortening.
        withExtendedLifetime(self) {}
    }
#endif
    /// Fit in the existing engine-owned slot without changing flow or anchors.
    func fittedBounds(in slot: AtlasDocumentDisplayRect) -> AtlasDocumentDisplayRect {
        let scale = min(slot.width / Double(width), slot.height / Double(height))
        let width = Double(width) * scale, height = Double(height) * scale
        return .init(x: slot.x + (slot.width - width) / 2, y: slot.y + (slot.height - height) / 2,
            width: width, height: height)
    }
}
enum AtlasDocumentImageDecoder {
    /// Called by the serialized reader worker. remainingBytes is the unspent
    /// retained page allowance. ImageIO calls themselves are noninterruptible;
    /// cancellation is checked before and after them and before publication.
    static func decode(_ data: Data, target: AtlasDocumentDisplayImageTarget,
                       remainingBytes: Int, canceled: () -> Bool) throws -> AtlasDocumentDisplayImage {
        if canceled() { throw AtlasReaderError.canceled }
        guard !Thread.isMainThread else { throw AtlasDocumentImageError.wrongExecutor }
        guard !data.isEmpty, data.count <= AtlasDocumentImageLimits.encodedBytes,
              remainingBytes > 0, remainingBytes <= AtlasDocumentImageLimits.pageBytes,
              target.bounds.valid, target.bounds.width > 0, target.bounds.height > 0 else {
            throw AtlasDocumentImageError.resourceLimit
        }
#if canImport(ImageIO) && canImport(CoreGraphics)
        return try autoreleasepool {
            let format: AtlasDocumentImageFormat
            if data.starts(with: [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]) { format = .png }
            else if data.starts(with: [0xff, 0xd8, 0xff]) { format = .jpeg }
            else { throw AtlasDocumentImageError.unsupported }
            let options = [kCGImageSourceShouldCache: false, kCGImageSourceShouldAllowFloat: false] as CFDictionary
            guard let source = CGImageSourceCreateWithData(data as CFData, options),
                  let type = CGImageSourceGetType(source),
                  (type as String) == (format == .png ? "public.png" : "public.jpeg"),
                  CGImageSourceGetStatus(source) == .statusComplete else {
                throw AtlasDocumentImageError.invalidImage
            }
            if canceled() { throw AtlasReaderError.canceled }
            guard CGImageSourceGetCount(source) == 1 else { throw AtlasDocumentImageError.unsupported }
            guard let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, options) as? [String: Any],
                  let width = integer(properties[kCGImagePropertyPixelWidth as String]),
                  let height = integer(properties[kCGImagePropertyPixelHeight as String]),
                  let depth = integer(properties[kCGImagePropertyDepth as String]), depth <= 8,
                  width <= AtlasDocumentImageLimits.sourceDimension,
                  height <= AtlasDocumentImageLimits.sourceDimension,
                  width * height <= AtlasDocumentImageLimits.sourcePixels else {
                throw AtlasDocumentImageError.resourceLimit
            }
            if canceled() { throw AtlasReaderError.canceled }
            let maximum = min(AtlasDocumentImageLimits.thumbnailDimension,
                max(1, Int(ceil(max(target.bounds.width, target.bounds.height) * 2))))
            // Conservative admission before ImageIO thumbnail allocation.
            let admittedBytes = min(width, maximum) * min(height, maximum) * 4
            guard admittedBytes <= remainingBytes else { throw AtlasDocumentImageError.resourceLimit }
            let reservation = try AtlasDocumentImageBudget.shared.reserve(admittedBytes)
            let thumbnailOptions = [
                kCGImageSourceCreateThumbnailFromImageAlways: true,
                kCGImageSourceCreateThumbnailWithTransform: true,
                kCGImageSourceThumbnailMaxPixelSize: maximum,
                kCGImageSourceShouldCacheImmediately: true,
                kCGImageSourceShouldAllowFloat: false
            ] as CFDictionary
            guard let thumbnail = CGImageSourceCreateThumbnailAtIndex(source, 0, thumbnailOptions),
                  CGImageSourceGetStatusAtIndex(source, 0) == .statusComplete,
                  thumbnail.width > 0, thumbnail.height > 0,
                  thumbnail.width <= maximum, thumbnail.height <= maximum else {
                throw AtlasDocumentImageError.invalidImage
            }
            if canceled() { throw AtlasReaderError.canceled }
            let outputBytes = thumbnail.width * thumbnail.height * 4
            guard outputBytes <= remainingBytes, outputBytes <= admittedBytes else {
                throw AtlasDocumentImageError.resourceLimit
            }
            var rgba = Data(count: outputBytes)
            let drawn = rgba.withUnsafeMutableBytes { storage -> Bool in
                guard let colorSpace = CGColorSpace(name: CGColorSpace.sRGB),
                      let context = CGContext(data: storage.baseAddress, width: thumbnail.width,
                          height: thumbnail.height, bitsPerComponent: 8, bytesPerRow: thumbnail.width * 4,
                          space: colorSpace, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
                              | CGBitmapInfo.byteOrder32Big.rawValue) else { return false }
                context.setBlendMode(.copy)
                context.draw(thumbnail, in: CGRect(x: 0, y: 0, width: CGFloat(thumbnail.width), height: CGFloat(thumbnail.height)))
                return true
            }
            guard drawn else { throw AtlasDocumentImageError.resourceLimit }
            if canceled() { throw AtlasReaderError.canceled }
            let image = try AtlasDocumentDisplayImage(target: target, format: format, sourceWidth: width,
                sourceHeight: height, width: thumbnail.width, height: thumbnail.height, rgba: rgba, reservation: reservation)
            if canceled() { throw AtlasReaderError.canceled }
            return image
        }
#else
        throw AtlasDocumentImageError.unavailable
#endif
    }
#if canImport(ImageIO) && canImport(CoreGraphics)
    private static func integer(_ value: Any?) -> Int? {
        guard let value = value as? NSNumber, CFGetTypeID(value) != CFBooleanGetTypeID() else { return nil }
        let number = value.doubleValue
        guard number.isFinite, number > 0, number.rounded(.towardZero) == number,
              number <= Double(AtlasDocumentImageLimits.sourceDimension) else { return nil }
        return Int(number)
    }
#endif
}

enum AtlasDocumentImageAuthority: Sendable {
    case current(requestedRoot: [UInt8], assetRoot: [UInt8])
    case saved(AtlasSavedDocumentImageSource)
}
struct AtlasDocumentImageAuthorization: Sendable {
    let document: AtlasReaderDocumentInventory
    let displayWidth: UInt64
    let captureSHA256: String
    let authority: AtlasDocumentImageAuthority
    let sourceRelative: [UInt8]
}
struct AtlasDocumentImageReceipt: Sendable {
    let target: AtlasDocumentDisplayImageTarget
    let authorization: AtlasDocumentImageAuthorization
    let totalBytes: UInt64
    let format: AtlasDocumentImageFormat
    let sha256: String
    let firstChunk: Data
    // Chunks from another archive member cannot reuse an equal content digest.
    let savedImageMember: UInt64?
}

/// No header is accepted from a different source/capture, no string counter is
/// rounded through a JSON number, and arrays are bounded while decoding.
enum AtlasDocumentDisplayWire {
    private struct Key: CodingKey {
        let stringValue: String
        var intValue: Int? { nil }
        init(_ value: String) { stringValue = value }
        init?(stringValue: String) { self.init(stringValue) }
        init?(intValue: Int) { return nil }
    }
    private struct Fields: Decodable {
        let header: AtlasReaderWire.Header
        let values: KeyedDecodingContainer<Key>
        init(from decoder: Decoder) throws {
            header = try .init(from: decoder)
            values = try decoder.container(keyedBy: Key.self)
        }
        func text(_ name: String) throws -> String { try values.decode(String.self, forKey: Key(name)) }
        func number(_ name: String) throws -> UInt64 {
            guard let n = AtlasReaderWire.integer(try text(name)) else { throw AtlasReaderError.invalidResponse }
            return n
        }
        func optionalNumber(_ name: String) throws -> UInt64? {
            guard let text = try values.decodeIfPresent(String.self, forKey: Key(name)) else { return nil }
            guard let n = AtlasReaderWire.integer(text) else { throw AtlasReaderError.invalidResponse }
            return n
        }
        func flag(_ name: String) throws -> Bool { try values.decode(Bool.self, forKey: Key(name)) }
        func array<T: Decodable>(_ name: String, limit: Int) throws -> [T] {
            var c = try values.nestedUnkeyedContainer(forKey: Key(name))
            if let count = c.count, count > limit { throw AtlasReaderError.invalidResponse }
            var result: [T] = []
            while !c.isAtEnd {
                guard result.count < limit else { throw AtlasReaderError.invalidResponse }
                result.append(try c.decode(T.self))
            }
            return result
        }
        func validate(_ document: AtlasReaderDocumentInventory, command: String) throws {
            guard try header.identity(handle: document.identity.owner).matches(document.identity),
                  header.command == command, try number("document_generation") == document.generation,
                  try text("rendering") == "bundled-frankenmarkdown-display",
                  try flag("glyph_shaped"), try !flag("native_shaped"),
                  try text("source_mapping") == "enclosing-ast-blocks-not-glyph-exact" else {
                throw AtlasReaderError.invalidResponse
            }
        }
        func digest(_ name: String) throws -> String {
            let value = try text(name)
            guard value.utf8.count == 64,
                  value.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else {
                throw AtlasReaderError.invalidResponse
            }
            return value
        }
        func nativePath(_ name: String) throws -> [UInt8] {
            let path = try values.decode(AtlasReaderWire.Header.NativePath.self, forKey: Key(name))
            guard path.encoding == "unix-bytes", path.display.utf8.count <= 65_536 else {
                throw AtlasReaderError.invalidResponse
            }
            let bytes = try AtlasReaderWire.pathBytes(path.hex)
            guard !bytes.isEmpty, !bytes.contains(0) else { throw AtlasReaderError.invalidResponse }
            return bytes
        }
        func imageLimits() throws {
            let retained = try number("retained_images"), bytes = try number("retained_image_bytes")
            guard try number("max_image_bytes") == UInt64(AtlasDocumentImageLimits.encodedBytes),
                  try number("max_image_cache_bytes") == AtlasDocumentImageLimits.encodedCacheBytes,
                  try number("max_images") == UInt64(AtlasDocumentImageLimits.pageImages),
                  try number("max_image_chunk_bytes") == AtlasDocumentImageLimits.chunkBytes,
                  retained <= UInt64(AtlasDocumentImageLimits.pageImages),
                  bytes <= AtlasDocumentImageLimits.encodedCacheBytes,
                  (retained == 0) == (bytes == 0) else { throw AtlasReaderError.invalidResponse }
        }
        func savedImageSource(_ source: AtlasSavedDocumentImageSource) throws {
            guard try number("saved_archive_owner") == source.archiveOwner,
                  try digest("saved_archive_digest") == source.archiveDigest,
                  try number("saved_source_member") == source.sourceMember,
                  try !flag("live_root_authority"),
                  !values.contains(Key("requested_root")), !values.contains(Key("asset_root")) else {
                throw AtlasReaderError.invalidResponse
            }
        }
        func imageIdentity(_ target: AtlasDocumentDisplayImageTarget,
                           authorization: AtlasDocumentImageAuthorization, command: String) throws -> UInt64? {
            guard target.inventory.qualifiedSources, let original = target.original,
                  original.valid(upTo: target.inventory.document.identity.capturedBytes, nonempty: true),
                  original.start >= target.inventory.document.sourceBase,
                  authorization.document.matches(target.inventory.document),
                  authorization.displayWidth == target.inventory.width,
                  try header.identity(handle: target.inventory.document.identity.owner)
                    .matches(target.inventory.document.identity),
                  header.command == command,
                  try number("document_generation") == target.inventory.document.generation,
                  try number("display_width_points") == target.inventory.width,
                  try digest("capture_sha256") == authorization.captureSHA256,
                  try nativePath("source_relative") == authorization.sourceRelative,
                  try number("item_index") == target.itemIndex,
                  try number("request_id") == target.requestID,
                  try text("destination").utf8.elementsEqual(target.destination.utf8),
                  values.contains(Key("enclosing_original_range")),
                  try values.decodeIfPresent(AtlasDocumentByteRange.self, forKey: Key("enclosing_original_range")) == target.original,
                  try text("format_validation") == "signature-only", try !flag("decoded") else {
                throw AtlasReaderError.invalidResponse
            }
            let savedMember: UInt64?
            switch authorization.authority {
            case .current(let requestedRoot, let assetRoot):
                guard try nativePath("requested_root") == requestedRoot,
                      try nativePath("asset_root") == assetRoot,
                      try text("image_observation") == "current-root-capture" else {
                    throw AtlasReaderError.invalidResponse
                }
                savedMember = nil
            case .saved(let source):
                try savedImageSource(source)
                let member = try number("image_member")
                guard member < source.members,
                      try digest("image_archive_digest") == source.archiveDigest,
                      try text("image_observation") == "verified-saved-member" else {
                    throw AtlasReaderError.invalidResponse
                }
                savedMember = member
            }
            try imageLimits()
            return savedMember
        }
        func imageBytes(total: UInt64, offset: UInt64) throws -> Data {
            guard offset < total, try number("image_offset") == offset else {
                throw AtlasReaderError.invalidResponse
            }
            let count = min(AtlasDocumentImageLimits.chunkBytes, total - offset)
            guard try number("image_bytes") == count,
                  try optionalNumber("next_image_offset") == (offset + count < total ? offset + count : nil) else {
                throw AtlasReaderError.invalidResponse
            }
            let hex = try text("image_hex")
            guard hex.utf8.count == Int(count * 2),
                  hex.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else {
                throw AtlasReaderError.invalidResponse
            }
            var bytes = Data(); bytes.reserveCapacity(Int(count))
            let input = Array(hex.utf8)
            func nibble(_ b: UInt8) -> UInt8 { b <= 57 ? b - 48 : b - 87 }
            for i in stride(from: 0, to: input.count, by: 2) { bytes.append(nibble(input[i]) * 16 + nibble(input[i + 1])) }
            return bytes
        }
        func inventory(_ document: AtlasReaderDocumentInventory) throws -> AtlasDocumentDisplayInventory {
            struct Font: Decodable { let font_index, byte_length: String }
            let width = try number("display_width_points"), total = try number("total_display_items")
            let images = try number("unresolved_images"), qualified = try flag("source_ranges_qualified")
            let bounds = try values.decode(AtlasDocumentDisplayRect.self, forKey: Key("display_bounds"))
            let rows: [Font] = try array("fonts", limit: AtlasDocumentDisplayLimits.fonts)
            var bytes: UInt64 = 0
            let fonts = try rows.enumerated().map { offset, row -> AtlasDocumentDisplayFontSpec in
                guard let index = AtlasReaderWire.integer(row.font_index), index == UInt64(offset),
                      let count = AtlasReaderWire.integer(row.byte_length), count > 0,
                      count <= AtlasDocumentDisplayLimits.fontBytes - bytes else {
                    throw AtlasReaderError.invalidResponse
                }
                bytes += count
                return .init(index: index, bytes: count)
            }
            guard (160...4096).contains(width), total <= AtlasDocumentDisplayLimits.items,
                  images <= total, bounds.valid, bounds.x >= 0, bounds.y >= 0 else {
                throw AtlasReaderError.invalidResponse
            }
            return .init(document: document, width: width, bounds: bounds, totalItems: total,
                unresolvedImages: images, qualifiedSources: qualified, fonts: fonts)
        }
    }
    private struct Glyph: Decodable {
        let glyph_id, font_index: String
        let x, y: Double
    }
    private struct Item: Decodable {
        let fields: KeyedDecodingContainer<Key>
        let glyphs: [Glyph]
        init(from decoder: Decoder) throws {
            fields = try decoder.container(keyedBy: Key.self)
            if fields.contains(Key("glyphs")) {
                var c = try fields.nestedUnkeyedContainer(forKey: Key("glyphs"))
                if let count = c.count, count > AtlasDocumentDisplayLimits.runGlyphs {
                    throw AtlasReaderError.invalidResponse
                }
                var glyphs: [Glyph] = []
                while !c.isAtEnd {
                    guard glyphs.count < AtlasDocumentDisplayLimits.runGlyphs else { throw AtlasReaderError.invalidResponse }
                    glyphs.append(try c.decode(Glyph.self))
                }
                self.glyphs = glyphs
            } else { glyphs = [] }
        }
        func text(_ key: String) throws -> String { try fields.decode(String.self, forKey: Key(key)) }
        func value(index: UInt64, inventory: AtlasDocumentDisplayInventory,
                   glyphCount: inout Int, textBytes: inout Int) throws -> AtlasDocumentDisplayItem {
            let bounds = try fields.decode(AtlasDocumentDisplayRect.self, forKey: Key("bounds"))
            let original = try fields.decodeIfPresent(AtlasDocumentByteRange.self, forKey: Key("enclosing_original_range"))
            guard AtlasReaderWire.integer(try text("item_index")) == index, bounds.valid,
                  bounds.x >= inventory.bounds.x - 1, bounds.y >= inventory.bounds.y - 1,
                  bounds.maxX <= inventory.bounds.maxX + 1, bounds.maxY <= inventory.bounds.maxY + 1 else {
                throw AtlasReaderError.invalidResponse
            }
            if let original {
                guard inventory.qualifiedSources,
                      original.valid(upTo: inventory.document.identity.capturedBytes, nonempty: true),
                      original.start >= inventory.document.sourceBase else { throw AtlasReaderError.invalidResponse }
            }
            let kind = try text("kind")
            let content: AtlasDocumentDisplayItem.Content
            var imageReference: AtlasDocumentDisplayImageReference?
            switch kind {
            case "text":
                let text = try self.text("text"), role = try self.text("color_role")
                let size = try fields.decode(Double.self, forKey: Key("font_size"))
                textBytes += text.utf8.count; glyphCount += glyphs.count
                guard textBytes <= 256 * 1024, glyphCount <= AtlasDocumentDisplayLimits.pageGlyphs,
                      size.isFinite, (1...256).contains(size), role.utf8.count <= 64,
                      text.isEmpty || !glyphs.isEmpty else { throw AtlasReaderError.invalidResponse }
                let glyphs = try glyphs.map { glyph -> AtlasDocumentDisplayGlyph in
                    guard let id = AtlasReaderWire.integer(glyph.glyph_id), id > 0, let exact = UInt16(exactly: id),
                          let font = AtlasReaderWire.integer(glyph.font_index),
                          inventory.fonts.contains(where: { $0.index == font }),
                          glyph.x.isFinite, glyph.y.isFinite,
                          abs(glyph.x) <= 1_000_000, abs(glyph.y) <= 1_000_000 else {
                        throw AtlasReaderError.invalidResponse
                    }
                    return .init(id: exact, font: font, x: glyph.x, y: glyph.y)
                }
                content = .text(text, role: role, size: size, glyphs: glyphs)
            case "vector":
                let shape = try text("shape"), role = try text("color_role")
                let stroke = try fields.decode(Double.self, forKey: Key("stroke_width"))
                guard ["horizontal-rule", "table-border", "quote-accent", "checkbox-outline",
                       "checkbox-check"].contains(shape),
                      stroke.isFinite, (0...64).contains(stroke), role.utf8.count <= 64,
                      glyphs.isEmpty else { throw AtlasReaderError.invalidResponse }
                content = .vector(shape: shape, role: role, stroke: stroke)
            case "image":
                let alt = try text("alt_text"), destination = try text("destination")
                guard try !fields.decode(Bool.self, forKey: Key("is_resolved")), glyphs.isEmpty,
                      alt.utf8.count <= 16_384, destination.utf8.count <= 4096,
                      let requestID = AtlasReaderWire.integer(try text("request_id")) else {
                    throw AtlasReaderError.invalidResponse
                }
                content = .image(alt: alt)
                imageReference = .init(requestID: requestID, destination: destination)
            case "anchor":
                let id = try text("anchor_id")
                let heading = try fields.decode(Bool.self, forKey: Key("is_heading"))
                guard id.utf8.count <= 4096, !id.utf8.contains(0), glyphs.isEmpty,
                      let level = AtlasReaderWire.integer(try text("level")), level <= 6 else {
                    throw AtlasReaderError.invalidResponse
                }
                content = .anchor(id, heading: heading, level: level)
            default: throw AtlasReaderError.invalidResponse
            }
            return .init(id: index, bounds: bounds, original: original, content: content,
                imageReference: imageReference)
        }
    }
    static func page(_ json: String?, document: AtlasReaderDocumentInventory,
                     command: String, first expectedFirst: UInt64?, width: UInt64? = nil,
                     sourceAnchor: UInt64? = nil,
                     previous: AtlasDocumentDisplayInventory? = nil) throws -> AtlasDocumentDisplayPage {
        do {
            let data = try AtlasReaderWire.admit(json, handle: document.identity.owner)
            let w = try JSONDecoder().decode(Fields.self, from: data)
            try w.validate(document, command: command)
            let inventory = try w.inventory(document)
            let first = try w.number("first_display_item")
            guard width == nil || width == inventory.width,
                  previous == nil || previous!.matches(inventory),
                  expectedFirst == nil || expectedFirst == first, first <= inventory.totalItems else {
                throw AtlasReaderError.invalidResponse
            }
            if let sourceAnchor {
                guard inventory.qualifiedSources,
                      sourceAnchor >= document.sourceBase,
                      sourceAnchor <= document.identity.capturedBytes,
                      try w.number("original_anchor") == sourceAnchor else {
                    throw AtlasReaderError.invalidResponse
                }
            }
            var rows = try w.values.nestedUnkeyedContainer(forKey: Key("display_items"))
            if let count = rows.count, count > Int(AtlasDocumentDisplayLimits.pageItems) {
                throw AtlasReaderError.invalidResponse
            }
            var items: [AtlasDocumentDisplayItem] = []
            var glyphCount = 0, textBytes = 0
            while !rows.isAtEnd {
                guard items.count < Int(AtlasDocumentDisplayLimits.pageItems) else { throw AtlasReaderError.invalidResponse }
                // Validate each bounded run before decoding the next. This
                // prevents accumulating 128 maximum-size glyph arrays first.
                let row = try rows.decode(Item.self)
                items.append(try row.value(index: first + UInt64(items.count), inventory: inventory,
                    glyphCount: &glyphCount, textBytes: &textBytes))
            }
            let remaining = inventory.totalItems - first
            guard UInt64(items.count) <= remaining, remaining == 0 || !items.isEmpty else {
                throw AtlasReaderError.invalidResponse
            }
            let end = first + UInt64(items.count), next = try w.optionalNumber("next_display_item")
            guard next == (end < inventory.totalItems ? end : nil) else { throw AtlasReaderError.invalidResponse }
            if let sourceAnchor {
                if sourceAnchor == document.identity.capturedBytes {
                    guard first == inventory.totalItems, items.isEmpty else {
                        throw AtlasReaderError.invalidResponse
                    }
                } else {
                    // An echoed request offset does not prove that the returned
                    // page begins at its enclosing captured source block.
                    guard let original = items.first?.original,
                          original.start <= sourceAnchor, sourceAnchor < original.end else {
                        throw AtlasReaderError.invalidResponse
                    }
                }
            }
            return .init(id: UUID(), inventory: inventory, first: first, items: items, next: next)
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }
    static func imageAuthorization(_ json: String?, document: AtlasReaderDocumentInventory,
                                   root: String, sourceRelative: String) throws -> AtlasDocumentImageAuthorization {
        do {
            let w = try JSONDecoder().decode(Fields.self,
                from: AtlasReaderWire.admit(json, handle: document.identity.owner))
            let h = w.header, identity = document.identity
            let verified = try w.number("source_verification_bytes_read"), calls = try w.number("source_verification_read_calls")
            let reused = try w.flag("authorization_reused"), width = try w.number("display_width_points")
            // Authorization alone is allowed to re-read the current source.
            // Never normalize this read count to reuse the zero-I/O header check.
            guard h.command == "document-images-authorize",
                  AtlasReaderWire.integer(h.owner) == identity.owner,
                  AtlasReaderWire.integer(h.file_id) == identity.file,
                  AtlasReaderWire.integer(h.source_revision) == identity.revision,
                  AtlasReaderWire.integer(h.captured_bytes) == identity.capturedBytes,
                  h.encoding == identity.encoding, !h.native_presented,
                  h.path.encoding == "unix-bytes", h.path.display.utf8.count <= 65_536,
                  try AtlasReaderWire.pathBytes(h.path.hex) == identity.pathBytes,
                  AtlasReaderWire.integer(h.additional_source_bytes_read) == verified,
                  try w.number("document_generation") == document.generation,
                  try w.flag("images_authorized"), try w.text("source_authority") == "verified-current-source",
                  (160...4096).contains(width),
                  reused ? (verified == 0 && calls == 0)
                    : (verified == identity.capturedBytes && (identity.capturedBytes == 0 || calls > 0)),
                  try w.nativePath("requested_root") == Array(root.utf8),
                  try w.nativePath("source_relative") == Array(sourceRelative.utf8) else {
                throw AtlasReaderError.invalidResponse
            }
            let assetRoot = try w.nativePath("asset_root")
            guard assetRoot.first == 47 else { throw AtlasReaderError.invalidResponse }
            try w.imageLimits()
            return .init(document: document, displayWidth: width, captureSHA256: try w.digest("capture_sha256"),
                authority: .current(requestedRoot: Array(root.utf8), assetRoot: assetRoot),
                sourceRelative: Array(sourceRelative.utf8))
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }
    /// Saved success still uses the retained reader schema. Saved-handle errors
    /// have their own schema/owner and are admitted only for this exact provider.
    /// Unknown schemas, success-shaped saved responses and owner swaps fail shut.
    private static func admitImage(_ json: String?, document: AtlasReaderDocumentInventory,
                                   saved: AtlasSavedDocumentImageSource?) throws -> Data {
        guard let json else { throw AtlasReaderError.unavailable }
        guard json.utf8.count <= AtlasReaderLimits.responseBytes else { throw AtlasReaderError.invalidResponse }
        let data = Data(json.utf8)
        struct Status: Decodable {
            let schema, status, owner: String
            let exit_code: String?
            let error: Diagnostic?
            struct Diagnostic: Decodable { let code: String }
        }
        let status = try JSONDecoder().decode(Status.self, from: data)
        if status.schema == "fcb.saved-repository/1" {
            guard let saved, status.status == "error",
                  AtlasReaderWire.integer(status.owner) == saved.archiveOwner,
                  status.exit_code == "2" || status.exit_code == "130",
                  let code = status.error?.code, !code.isEmpty, code.utf8.count <= 512,
                  code.unicodeScalars.allSatisfy({ $0.value >= 32 && $0.value != 127 }) else {
                throw AtlasReaderError.invalidResponse
            }
            throw AtlasReaderError.engine(code)
        }
        return try AtlasReaderWire.admit(json, handle: document.identity.owner)
    }
    static func savedImageAuthorization(_ json: String?, document: AtlasReaderDocumentInventory,
        source: AtlasSavedDocumentImageSource) throws -> AtlasDocumentImageAuthorization {
        do {
            let w = try JSONDecoder().decode(Fields.self,
                from: admitImage(json, document: document, saved: source))
            let width = try w.number("display_width_points")
            guard try w.header.identity(handle: document.identity.owner).matches(document.identity),
                  document.identity.owner != source.archiveOwner,
                  w.header.command == "document-images-authorize",
                  try w.number("document_generation") == document.generation,
                  (160...4096).contains(width),
                  try w.flag("images_authorized"), try w.text("source_authority") == "verified-saved-member",
                  try w.number("source_verification_bytes_read") == 0,
                  try w.number("source_verification_read_calls") == 0,
                  try w.nativePath("source_relative") == document.identity.pathBytes else {
                throw AtlasReaderError.invalidResponse
            }
            _ = try w.flag("authorization_reused")
            try w.savedImageSource(source); try w.imageLimits()
            return .init(document: document, displayWidth: width, captureSHA256: try w.digest("capture_sha256"),
                authority: .saved(source), sourceRelative: document.identity.pathBytes)
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }
    static func imagePrepared(_ json: String?, target: AtlasDocumentDisplayImageTarget,
                              authorization: AtlasDocumentImageAuthorization) throws -> AtlasDocumentImageReceipt {
        do {
            let saved: AtlasSavedDocumentImageSource?
            if case .saved(let source) = authorization.authority { saved = source } else { saved = nil }
            let w = try JSONDecoder().decode(Fields.self,
                from: admitImage(json, document: target.inventory.document, saved: saved))
            let savedMember = try w.imageIdentity(target, authorization: authorization, command: "document-image-prepare")
            let total = try w.number("total_image_bytes"), read = try w.number("image_bytes_read")
            let calls = try w.number("image_read_calls")
            guard total > 0, total <= UInt64(AtlasDocumentImageLimits.encodedBytes),
                  try w.number("retained_images") > 0, try w.number("retained_image_bytes") >= total,
                  (read == 0 && calls == 0) || (read == total && calls > 0),
                  let format = AtlasDocumentImageFormat(rawValue: try w.text("image_format")) else {
                throw AtlasReaderError.invalidResponse
            }
            return .init(target: target, authorization: authorization, totalBytes: total, format: format,
                sha256: try w.digest("image_sha256"), firstChunk: try w.imageBytes(total: total, offset: 0),
                savedImageMember: savedMember)
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }
    static func imageChunk(_ json: String?, receipt: AtlasDocumentImageReceipt, offset: UInt64) throws -> Data {
        do {
            let w = try JSONDecoder().decode(Fields.self,
                from: AtlasReaderWire.admit(json, handle: receipt.target.inventory.document.identity.owner))
            let savedMember = try w.imageIdentity(receipt.target, authorization: receipt.authorization, command: "document-image-chunk")
            guard savedMember == receipt.savedImageMember,
                  try w.number("total_image_bytes") == receipt.totalBytes,
                  try w.text("image_format") == receipt.format.rawValue,
                  try w.digest("image_sha256") == receipt.sha256,
                  try w.number("retained_images") > 0, try w.number("retained_image_bytes") >= receipt.totalBytes,
                  try w.number("image_bytes_read") == 0, try w.number("image_read_calls") == 0 else {
                throw AtlasReaderError.invalidResponse
            }
            return try w.imageBytes(total: receipt.totalBytes, offset: offset)
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }
    static func verifyImage(_ data: Data, receipt: AtlasDocumentImageReceipt) throws {
        guard UInt64(data.count) == receipt.totalBytes,
              data.count <= AtlasDocumentImageLimits.encodedBytes else { throw AtlasReaderError.invalidResponse }
#if canImport(CryptoKit)
        let digest = SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
        guard digest == receipt.sha256 else { throw AtlasReaderError.invalidResponse }
#else
        throw AtlasDocumentImageError.unavailable
#endif
    }
    static func fontChunk(_ json: String?, inventory: AtlasDocumentDisplayInventory,
                          spec: AtlasDocumentDisplayFontSpec, offset: UInt64) throws -> Data {
        do {
            let data = try AtlasReaderWire.admit(json, handle: inventory.document.identity.owner)
            let w = try JSONDecoder().decode(Fields.self, from: data)
            try w.validate(inventory.document, command: "document-display-font")
            guard try w.number("display_width_points") == inventory.width,
                  try w.number("font_index") == spec.index, try w.number("font_offset") == offset,
                  try w.number("total_font_bytes") == spec.bytes, offset < spec.bytes else {
                throw AtlasReaderError.invalidResponse
            }
            let count = min(AtlasDocumentDisplayLimits.fontChunk, spec.bytes - offset)
            guard try w.number("font_bytes") == count,
                  try w.optionalNumber("next_font_offset") == (offset + count < spec.bytes ? offset + count : nil) else {
                throw AtlasReaderError.invalidResponse
            }
            let hex = try w.text("font_hex")
            guard hex.utf8.count == Int(count * 2),
                  hex.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else {
                throw AtlasReaderError.invalidResponse
            }
            var result = Data(); result.reserveCapacity(Int(count))
            let bytes = Array(hex.utf8)
            func nibble(_ b: UInt8) -> UInt8 { b <= 57 ? b - 48 : b - 87 }
            for i in stride(from: 0, to: bytes.count, by: 2) { result.append(nibble(bytes[i]) * 16 + nibble(bytes[i + 1])) }
            return result
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }
}
