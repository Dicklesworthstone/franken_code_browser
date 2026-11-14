import Foundation

@_silgen_name("fcb_saved_trail_attach") private func savedTrailAttach(_ handle: UInt64,
    _ generation: UInt64, _ path: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_trail_page") private func savedTrailPage(_ handle: UInt64,
    _ generation: UInt64, _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_trail_detach") private func savedTrailDetach(_ handle: UInt64,
    _ generation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_open_trail_reader") private func savedTrailReader(_ handle: UInt64,
    _ reader: UInt64, _ generation: UInt64, _ item: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_free_string") private func savedTrailFree(_ pointer: UnsafeMutablePointer<CChar>?)

extension AtlasSavedTrailTransport {
    static let native = Self(run: { handle, request in
        guard handle > 0 else { return nil }
        switch request {
        case .attach(let generation, let path):
            guard generation > 0, path.hasPrefix("/"), path.utf8.count <= 16_384, !path.utf8.contains(0) else { return nil }
            return path.withCString { take(savedTrailAttach(handle, generation, $0)) }
        case .page(let generation, let start, let limit):
            guard generation > 0, start <= 1024, (1...AtlasSavedTrailPage.pageSize).contains(limit) else { return nil }
            return take(savedTrailPage(handle, generation, start, limit))
        case .detach(let generation):
            guard generation > 0 else { return nil }
            return take(savedTrailDetach(handle, generation))
        case .openReader(let reader, let generation, let item):
            guard reader > 0, reader != handle, generation > 0, item < 1024 else { return nil }
            return take(savedTrailReader(handle, reader, generation, item))
        }
    })
    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { savedTrailFree(pointer) }
        var count = 0
        while count <= AtlasSavedWire.maximumResponseBytes && pointer[count] != 0 { count += 1 }
        guard count > 0, count <= AtlasSavedWire.maximumResponseBytes else { return nil }
        return String(data: Data(bytes: pointer, count: count), encoding: .utf8)
    }
}
