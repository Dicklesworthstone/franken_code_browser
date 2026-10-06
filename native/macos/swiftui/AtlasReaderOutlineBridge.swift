import Foundation

@_silgen_name("fcb_reader_outline")
private func fcb_reader_outline(_ handle: UInt64, _ generation: UInt64,
    _ language: UnsafePointer<CChar>?, _ items: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_symbols")
private func fcb_reader_symbols(_ handle: UInt64, _ generation: UInt64,
    _ needle: UnsafePointer<CChar>?, _ mode: UInt8, _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_symbol")
private func fcb_reader_symbol(_ handle: UInt64, _ generation: UInt64,
    _ symbol: UInt64, _ context: UInt64) -> UnsafeMutablePointer<CChar>?

extension AtlasReaderOutlineTransport {
    static let native = Self(prepare: { handle, generation, language, items in
        if let language {
            return language.withCString { take(fcb_reader_outline(handle, generation, $0, items)) }
        }
        return take(fcb_reader_outline(handle, generation, nil, items))
    }, symbols: { handle, generation, needle, mode, start, limit in
        needle.withCString { take(fcb_reader_symbols(handle, generation, $0, mode, start, limit)) }
    }, symbol: { take(fcb_reader_symbol($0, $1, $2, $3)) })

    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { fcb_free_string(pointer) }
        return String(cString: pointer)
    }
}
