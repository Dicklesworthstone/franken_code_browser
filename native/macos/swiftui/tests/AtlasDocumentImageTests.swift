import Foundation
import Dispatch
import CoreGraphics
import ImageIO

/// Native codec fixtures exercise the production ImageIO path with encoded
/// PNG/JPEG bytes and an independent pixel oracle. They do not qualify the
/// Rust filesystem grant or a physical-Mac window presentation.
private final class ImageFixtureResult<Value: Sendable>: @unchecked Sendable {
    private let ready = DispatchSemaphore(value: 0)
    private var result: Result<Value, Error>?
    func run(_ work: @Sendable () throws -> Value) {
        result = Result(catching: work); ready.signal()
    }
    func value() throws -> Value {
        ready.wait()
        return try result!.get()
    }
}

@main private enum AtlasDocumentImageTests {
    private static let queue = DispatchQueue(label: "document-image-native-fixtures")
    static func worker<Value: Sendable>(_ work: @escaping @Sendable () throws -> Value) throws -> Value {
        let result = ImageFixtureResult<Value>()
        queue.async { result.run(work) }
        return try result.value()
    }
    static func page(count: Int = 1, generation: UInt64 = 5, width: UInt64 = 200) -> AtlasDocumentDisplayPage {
        let identity = AtlasReaderIdentity(owner: 7, file: 2, revision: 3, capturedBytes: 20,
            pathBytes: Array("/p/README.md".utf8), encoding: "utf8", displayPath: "/p/README.md")
        let document = AtlasReaderDocumentInventory(identity: identity, generation: generation, width: 80,
            totalLines: 1, totalHeadings: 0, renderedBytes: 5, sourceBase: 0)
        let inventory = AtlasDocumentDisplayInventory(document: document, width: width,
            bounds: .init(x: 0, y: 0, width: Double(width), height: Double(count * 20)),
            totalItems: UInt64(count), unresolvedImages: UInt64(count), qualifiedSources: true, fonts: [])
        let items = (0..<count).map { index in
            AtlasDocumentDisplayItem(id: UInt64(index),
                bounds: .init(x: 0, y: Double(index * 20), width: Double(width), height: 20),
                original: .init(start: 0, end: 20), content: .image(alt: "Fixture \(index)"),
                imageReference: .init(requestID: UInt64(index + 100), destination: "images/\(index).png"))
        }
        return .init(id: UUID(), inventory: inventory, first: 0, items: items, next: nil)
    }
    static func source(width: Int, height: Int, transparent: Bool = false) -> CGImage {
        let colors: [[UInt8]] = [[255, 0, 0, 255], [0, 255, 0, 255],
            [0, 0, 255, 255], [255, 255, 255, transparent ? 0 : 255]]
        var pixels = Data(); pixels.reserveCapacity(width * height * 4)
        for y in 0..<height {
            for x in 0..<width { pixels.append(contentsOf: colors[(y < height / 2 ? 0 : 2) + (x < width / 2 ? 0 : 1)]) }
        }
        guard let space = CGColorSpace(name: CGColorSpace.sRGB), let provider = CGDataProvider(data: pixels as CFData),
              let image = CGImage(width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32,
                  bytesPerRow: width * 4, space: space,
                  bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.last.rawValue | CGBitmapInfo.byteOrder32Big.rawValue),
                  provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent) else {
            preconditionFailure("Could not construct real fixture pixels")
        }
        return image
    }
    static func encoded(_ image: CGImage, type: String, orientation: Int = 1, frames: Int = 1) -> Data {
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data as CFMutableData, type as CFString, frames, nil) else {
            preconditionFailure("Native fixture encoder is unavailable")
        }
        if frames > 1 {
            let options = [kCGImagePropertyPNGDictionary: [kCGImagePropertyAPNGLoopCount: 0]] as CFDictionary
            CGImageDestinationSetProperties(destination, options)
        }
        for _ in 0..<frames {
            var properties: [CFString: Any] = [
                kCGImagePropertyOrientation: orientation,
                kCGImageDestinationLossyCompressionQuality: 1.0
            ]
            if frames > 1 {
                properties[kCGImagePropertyPNGDictionary] = [kCGImagePropertyAPNGDelayTime: 0.1]
            }
            CGImageDestinationAddImage(destination, image, properties as CFDictionary)
        }
        precondition(CGImageDestinationFinalize(destination), "Native fixture encoding failed")
        return data as Data
    }
    static func decode(_ bytes: Data, target: AtlasDocumentDisplayImageTarget,
                       remaining: Int = AtlasDocumentImageLimits.pageBytes) throws -> AtlasDocumentDisplayImage {
        try AtlasDocumentImageDecoder.decode(bytes, target: target, remainingBytes: remaining, canceled: { false })
    }
    static func rendered(_ resource: AtlasDocumentDisplayImage) -> [UInt8] {
        var data = Data(count: resource.width * resource.height * 4)
        data.withUnsafeMutableBytes { bytes in
            let context = CGContext(data: bytes.baseAddress, width: resource.width, height: resource.height,
                bitsPerComponent: 8, bytesPerRow: resource.width * 4,
                space: CGColorSpace(name: CGColorSpace.sRGB)!,
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue)!
            // Match the AppKit canvas's top-left coordinate system. This calls
            // the same production drawing method as the actual document view.
            context.translateBy(x: 0, y: CGFloat(resource.height)); context.scaleBy(x: 1, y: -1)
            resource.draw(in: context, slot: .init(x: 0, y: 0,
                width: Double(resource.width), height: Double(resource.height)))
        }
        return Array(data)
    }
    static func pixel(_ bytes: [UInt8], width: Int, x: Int, y: Int) -> [UInt8] {
        let start = (y * width + x) * 4
        return Array(bytes[start..<start + 4])
    }
    static func near(_ actual: [UInt8], _ expected: [UInt8], tolerance: Int) -> Bool {
        zip(actual, expected).allSatisfy { abs(Int($0.0) - Int($0.1)) <= tolerance }
    }
    static func rejected(_ work: () throws -> Void, _ label: String) {
        do { try work(); preconditionFailure("Image fixture accepted \(label)") }
        catch { }
    }
    static func alteredDimensions(_ original: Data, width: UInt32, height: UInt32) -> Data {
        var data = original
        precondition(data.count > 33)
        for (offset, value) in [(16, width), (20, height)] {
            for byte in 0..<4 { data[offset + byte] = UInt8(truncatingIfNeeded: value >> UInt32((3 - byte) * 8)) }
        }
        var crc: UInt32 = 0xffff_ffff
        for byte in data[12..<29] {
            crc ^= UInt32(byte)
            for _ in 0..<8 { crc = (crc >> 1) ^ (crc & 1 == 0 ? 0 : 0xedb8_8320) }
        }
        crc ^= 0xffff_ffff
        for byte in 0..<4 { data[29 + byte] = UInt8(truncatingIfNeeded: crc >> UInt32((3 - byte) * 8)) }
        return data
    }
    static func main() throws {
        let input = try worker { encoded(source(width: 4, height: 4, transparent: true), type: "public.png") }
        let target = page().imageTargets[0]
        do {
            _ = try decode(input, target: target)
            preconditionFailure("Image decoding was allowed on the UI thread")
        } catch AtlasDocumentImageError.wrongExecutor { }

        try worker {
            precondition(!Thread.isMainThread)
            let original = page(), target = original.imageTargets[0]
            let png = try decode(input, target: target)
            precondition(png.format == .png && png.width == 4 && png.height == 4 && png.byteCount == 64)
            let pixels = rendered(png)
            precondition(pixel(pixels, width: 4, x: 0, y: 0) == [255, 0, 0, 255])
            precondition(pixel(pixels, width: 4, x: 3, y: 0) == [0, 255, 0, 255])
            precondition(pixel(pixels, width: 4, x: 0, y: 3) == [0, 0, 255, 255])
            precondition(pixel(pixels, width: 4, x: 3, y: 3) == [0, 0, 0, 0])
            precondition(!near([32, 128, 170, 255], [255, 0, 0, 255], tolerance: 20),
                "The independent pixel oracle must reject a fabricated gradient")
            let attached = try original.withImages([png], notice: "One local image decoded.")
            precondition(attached.image(for: original.items[0]) === png && attached.id == original.id)
            precondition(attached.imageRevision != original.imageRevision && attached.imageNotice != nil)
            rejected({ _ = try original.withImages([png, png]) }, "duplicate image identity")
            rejected({ _ = try original.withImages(Array(repeating: png, count: 17)) }, "page image count")
            rejected({ _ = try page(generation: 6).withImages([png]) }, "stale document generation")
            rejected({ _ = try page(width: 201).withImages([png]) }, "stale layout width")
            rejected({ _ = try decode(input, target: target, remaining: 3) }, "retained byte admission")

            let jpegData = encoded(source(width: 32, height: 24), type: "public.jpeg", orientation: 6)
            let jpeg = try decode(jpegData, target: target)
            precondition(jpeg.format == .jpeg && jpeg.sourceWidth == 32 && jpeg.sourceHeight == 24)
            precondition(jpeg.width == 24 && jpeg.height == 32, "EXIF orientation was ignored")
            let oriented = rendered(jpeg)
            for (x, y, expected) in [(6, 8, [UInt8(0), 0, 255, 255]), (18, 8, [255, 0, 0, 255]),
                                      (6, 24, [255, 255, 255, 255]), (18, 24, [0, 255, 0, 255])] {
                precondition(near(pixel(oriented, width: jpeg.width, x: x, y: y), expected, tolerance: 35),
                    "JPEG orientation or source pixel colors changed")
            }
            let fitted = jpeg.fittedBounds(in: .init(x: 10, y: 20, width: 120, height: 40))
            precondition(fitted.width == 30 && fitted.height == 40 && fitted.x == 55 && fitted.y == 20)

            let largeData = encoded(source(width: 2048, height: 1024), type: "public.png")
            let thumbnail = try decode(largeData, target: target)
            precondition(thumbnail.width <= 400 && thumbnail.height <= 400)
            precondition(thumbnail.width == thumbnail.height * 2 && thumbnail.byteCount < 1024 * 1024)
            let animated = encoded(source(width: 4, height: 4), type: "public.png", frames: 2)
            let animatedSource = CGImageSourceCreateWithData(animated as CFData, nil)!
            precondition(CGImageSourceGetCount(animatedSource) == 2, "Animated fixture lost its second frame")
            rejected({ _ = try decode(animated, target: target) }, "animated PNG")
            rejected({ _ = try decode(Data(input.prefix(input.count / 2)), target: target) }, "truncated PNG")
            rejected({ _ = try decode(Data(jpegData.prefix(jpegData.count / 2)), target: target) }, "truncated JPEG")
            rejected({ _ = try decode(Data("GIF89a".utf8), target: target) }, "unsupported codec")
            rejected({ _ = try decode(Data(repeating: 0, count: AtlasDocumentImageLimits.encodedBytes + 1),
                                     target: target) }, "encoded byte cap")
            rejected({ _ = try decode(alteredDimensions(input, width: 8193, height: 4),
                                     target: target) }, "oversized header dimension")
            rejected({ _ = try decode(alteredDimensions(input, width: 8192, height: 8192),
                                     target: target) }, "decoded source pixel bomb")
            for stopAt in 1...6 {
                var polls = 0
                do {
                    _ = try AtlasDocumentImageDecoder.decode(input, target: target,
                        remainingBytes: AtlasDocumentImageLimits.pageBytes) {
                        polls += 1; return polls >= stopAt
                    }
                    preconditionFailure("Image cancellation boundary \(stopAt) published pixels")
                } catch AtlasReaderError.canceled { }
            }

            // A faulty caller that neglects per-item remainingBytes still
            // cannot attach more than the aggregate page pixel allowance.
            let many = page(count: 5, width: 1024)
            let squareData = encoded(source(width: 1024, height: 1024), type: "public.png")
            let images = try many.imageTargets.map { try decode(squareData, target: $0) }
            precondition(images.reduce(0) { $0 + $1.byteCount } == 20 * 1024 * 1024)
            rejected({ _ = try many.withImages(images) }, "aggregate page pixels")
        }
        print("Actual native PNG/JPEG pixels, alpha, EXIF orientation, thumbnail bounds, cancellation and stale resource fixtures passed")
    }
}
