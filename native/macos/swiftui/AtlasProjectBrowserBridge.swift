import Foundation

@_silgen_name("fcb_catalog_create") private func catalogCreate() -> UInt64
@_silgen_name("fcb_catalog_open") private func catalogOpen(_ handle: UInt64, _ root: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_catalog_info") private func catalogInfo(_ handle: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_catalog_page") private func catalogPage(_ handle: UInt64, _ page: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_catalog_advance") private func catalogAdvance(_ handle: UInt64, _ page: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_catalog_stop") private func catalogStop(_ handle: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_catalog_open_reader") private func catalogReader(_ handle: UInt64, _ reader: UInt64,
    _ page: UInt64, _ file: UInt64, _ bytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_catalog_cancel") private func catalogCancel(_ handle: UInt64) -> UInt8
@_silgen_name("fcb_catalog_close") private func catalogClose(_ handle: UInt64) -> UInt8
@_silgen_name("fcb_free_string") private func catalogFree(_ pointer: UnsafeMutablePointer<CChar>?)

extension AtlasProjectBrowserTransport {
    static let native = Self(create: { catalogCreate() }, run: { handle, request in
        switch request {
        case .open(let root):
            guard root.hasPrefix("/"), root.utf8.count <= 16_384, !root.utf8.contains(0) else { return nil }
            return root.withCString { take(catalogOpen(handle, $0)) }
        case .info: return take(catalogInfo(handle))
        case .page(let page): return take(catalogPage(handle, page))
        case .advance(let page): return take(catalogAdvance(handle, page))
        case .stop: return take(catalogStop(handle))
        case .openReader(let reader, let page, let file, let bytes):
            return take(catalogReader(handle, reader, page, file, bytes))
        }
    }, cancel: { catalogCancel($0) == 1 }, close: { catalogClose($0) == 1 }, retirementFailed: {
        try? FileHandle.standardError.write(contentsOf: Data("FCB_CATALOG_RETIREMENT_UNAVAILABLE\n".utf8))
    }, readerDomain: AtlasReaderTransport.native.comparison?.domain)

    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { catalogFree(pointer) }
        let length = strnlen(pointer, AtlasProjectBrowserWire.maximumResponseBytes + 1)
        guard length > 0, length <= AtlasProjectBrowserWire.maximumResponseBytes else { return nil }
        return String(data: Data(bytes: pointer, count: length), encoding: .utf8)
    }
}
