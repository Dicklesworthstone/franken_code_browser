import Foundation

@_silgen_name("fcb_reader_resolve_selection")
private func fcb_reader_resolve_selection(_ handle: UInt64, _ windowStart: UInt64,
    _ windowEnd: UInt64, _ utf8Start: UInt64, _ utf8End: UInt64) -> UnsafeMutablePointer<CChar>?

extension AtlasReaderTextSelectionTransport {
    static let native = Self(resolve: { handle, windowStart, windowEnd, utf8Start, utf8End in
        guard let pointer = fcb_reader_resolve_selection(handle, windowStart, windowEnd, utf8Start, utf8End) else {
            return nil
        }
        defer { fcb_free_string(pointer) }
        var length = 0
        while length <= AtlasReaderLimits.responseBytes && pointer[length] != 0 { length += 1 }
        guard length > 0, length <= AtlasReaderLimits.responseBytes else { return nil }
        return String(data: Data(bytes: pointer, count: length), encoding: .utf8)
    })
}
