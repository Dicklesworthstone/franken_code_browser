import Foundation

@_silgen_name("fcb_reader_references")
private func fcb_reader_references(_ handle: UInt64, _ generation: UInt64,
    _ name: UnsafePointer<CChar>, _ items: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_reference_page")
private func fcb_reader_reference_page(_ handle: UInt64, _ generation: UInt64,
    _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_reference")
private func fcb_reader_reference(_ handle: UInt64, _ generation: UInt64,
    _ reference: UInt64, _ context: UInt64) -> UnsafeMutablePointer<CChar>?

extension AtlasReaderReferencesTransport {
    static let native = Self(prepare: { handle, generation, name, items in
        name.withCString { take(fcb_reader_references(handle, generation, $0, items)) }
    }, page: { take(fcb_reader_reference_page($0, $1, $2, $3)) },
        reference: { take(fcb_reader_reference($0, $1, $2, $3)) })

    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { fcb_free_string(pointer) }
        var length = 0
        while length <= AtlasReaderLimits.responseBytes && pointer[length] != 0 { length += 1 }
        guard length > 0, length <= AtlasReaderLimits.responseBytes else { return nil }
        return String(data: Data(bytes: pointer, count: length), encoding: .utf8)
    }
}
