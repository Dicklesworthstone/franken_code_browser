import Foundation

@_silgen_name("fcb_reader_create")
private func fcb_reader_create() -> UInt64
@_silgen_name("fcb_reader_open")
private func fcb_reader_open(_ handle: UInt64, _ path: UnsafePointer<CChar>?, _ bytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_window")
private func fcb_reader_window(_ handle: UInt64, _ offset: UInt64, _ bytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_lines")
private func fcb_reader_lines(_ handle: UInt64, _ first: UInt64, _ count: UInt64, _ bytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_cancel")
private func fcb_reader_cancel(_ handle: UInt64) -> UInt8
@_silgen_name("fcb_reader_close")
private func fcb_reader_close(_ handle: UInt64) -> UInt8

extension AtlasReaderTransport {
    static let native = Self(
        create: { fcb_reader_create() },
        open: { handle, path, bytes in
            path.withCString { take(fcb_reader_open(handle, $0, bytes)) }
        },
        window: { take(fcb_reader_window($0, $1, $2)) },
        lines: { take(fcb_reader_lines($0, $1, $2, $3)) },
        cancel: { fcb_reader_cancel($0) == 1 },
        close: { fcb_reader_close($0) == 1 },
        retirementFailed: {
            // One bounded, source-free diagnostic per failed retirement. The
            // Rust registry retains its capacity charge; failure is not success.
            let message = Data("FCB_READER_RETIREMENT_UNAVAILABLE\n".utf8)
            try? FileHandle.standardError.write(contentsOf: message)
        })

    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { fcb_free_string(pointer) }
        return String(cString: pointer)
    }
}
