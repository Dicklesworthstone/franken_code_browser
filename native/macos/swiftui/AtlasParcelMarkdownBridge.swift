import Foundation
import CryptoKit

@_silgen_name("fcb_reader_create") private func parcel_reader_create() -> UInt64
@_silgen_name("fcb_reader_open_bytes") private func parcel_reader_open_bytes(
    _ handle: UInt64, _ label: UnsafePointer<CChar>?, _ bytes: UnsafePointer<UInt8>?,
    _ count: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_close") private func parcel_reader_close(_ handle: UInt64) -> UInt8
@_silgen_name("fcb_free_string") private func parcel_free_string(_ value: UnsafeMutablePointer<CChar>?)

extension AtlasParcelMarkdownTransport {
    static let native = Self(create: { parcel_reader_create() }, supply: { handle, path, bytes in
        guard bytes.count <= AtlasParcelMarkdownWorker.sourceBytes,
              !path.isEmpty, path.utf8.count <= 16_384, !path.utf8.contains(0) else { return nil }
        let sha256 = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
        let json = path.withCString { label in
            bytes.withUnsafeBytes { raw -> String? in
                guard let pointer = parcel_reader_open_bytes(handle, label,
                    raw.bindMemory(to: UInt8.self).baseAddress, UInt64(bytes.count)) else { return nil }
                defer { parcel_free_string(pointer) }
                var count = 0
                while count <= AtlasReaderLimits.responseBytes && pointer[count] != 0 { count += 1 }
                guard count <= AtlasReaderLimits.responseBytes else { return nil }
                return String(validatingCString: pointer)
            }
        }
        return .init(json: json, sha256: sha256)
    }, close: { parcel_reader_close($0) == 1 }, document: .native)
}

extension AtlasViewportMarkdownTransport {
    static let native = Self(prepare: { source, limits, cancellation in
        guard !cancellation.isCanceled else { return nil }
        do {
            let packet = try AtlasParcelMarkdownWorker.load(path: source.source.path,
                source: source.source.text, transport: .native,
                canceled: { cancellation.isCanceled })
            return AtlasParcelDocumentDisplay.prepare(source: source, pages: packet.pages,
                fonts: packet.fonts, captureSHA256: packet.captureSHA256,
                glyphLimit: min(limits.glyphs, AtlasParcelMarkdownWorker.maximumGlyphs),
                runLimit: limits.runs, tileLimit: min(limits.tiles, 64),
                canceled: { cancellation.isCanceled })
        } catch { return nil }
    })
}
