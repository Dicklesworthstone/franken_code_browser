import Foundation

@_silgen_name("fcb_reader_find")
private func fcb_reader_find(_ handle: UInt64, _ generation: UInt64, _ needle: UnsafePointer<CChar>?,
                             _ matches: UInt64, _ scanBytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_find_with_options")
private func fcb_reader_find_with_options(_ handle: UInt64, _ generation: UInt64, _ needle: UnsafePointer<CChar>?,
    _ matches: UInt64, _ scanBytes: UInt64, _ flags: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_find_page")
private func fcb_reader_find_page(_ handle: UInt64, _ generation: UInt64, _ needle: UnsafePointer<CChar>?,
    _ startAfter: UInt64, _ matches: UInt64, _ scanBytes: UInt64, _ flags: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_hit")
private func fcb_reader_hit(_ handle: UInt64, _ generation: UInt64, _ index: UInt64,
                            _ context: UInt64) -> UnsafeMutablePointer<CChar>?

extension AtlasReaderSearchTransport {
    static let native = Self(find: { handle, generation, needle, matches, bytes in
        needle.withCString { take(fcb_reader_find(handle, generation, $0, matches, bytes)) }
    }, hit: { take(fcb_reader_hit($0, $1, $2, $3)) }, findWithOptions: { handle, generation, needle, matches, bytes, flags in
        needle.withCString { take(fcb_reader_find_with_options(handle, generation, $0, matches, bytes, flags)) }
    }, findPage: { handle, generation, needle, startAfter, matches, bytes, flags in
        needle.withCString { take(fcb_reader_find_page(handle, generation, $0, startAfter, matches, bytes, flags)) }
    })

    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { fcb_free_string(pointer) }
        return String(cString: pointer)
    }
}
