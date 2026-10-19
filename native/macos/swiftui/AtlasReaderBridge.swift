import Foundation
import CryptoKit

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
@_silgen_name("fcb_reader_fork")
private func fcb_reader_fork(_ source: UInt64, _ destination: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_open_bytes")
private func fcb_reader_open_bytes(_ handle: UInt64, _ label: UnsafePointer<CChar>?,
    _ bytes: UnsafePointer<UInt8>?, _ length: UInt64) -> UnsafeMutablePointer<CChar>?

@_silgen_name("fcb_reader_compare")
private func fcb_reader_compare(_ before: UInt64, _ after: UInt64, _ generation: UInt64,
    _ edits: UInt64, _ work: UInt64) -> UnsafeMutablePointer<CChar>?

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
        }, fork: { take(fcb_reader_fork($0, $1)) },
        openBytes: { handle, label, bytes in
            guard bytes.count <= Int(AtlasReaderLimits.captureBytes),
                  !label.isEmpty, label.utf8.count <= 16_384, !label.utf8.contains(0) else { return nil }
            // Invoked only by the coordinator's bounded worker queue. The
            // original UTF-8 (including BOM, NUL and normalization form) stays
            // alive until both native hashing and Rust's owned copy finish.
            let digest = SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
            let json = label.withCString { path in
                bytes.withUnsafeBytes { raw in
                    take(fcb_reader_open_bytes(handle, path, raw.bindMemory(to: UInt8.self).baseAddress,
                        UInt64(bytes.count)))
                }
            }
            return SuppliedResponse(json: json, sha256: digest)
        }, comparison: AtlasReaderComparisonTransport(domain: UUID(), compare: {
            take(fcb_reader_compare($0, $1, $2, $3, $4))
        }))

    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { fcb_free_string(pointer) }
        var length = 0
        while length <= AtlasReaderLimits.responseBytes && pointer[length] != 0 { length += 1 }
        guard length > 0, length <= AtlasReaderLimits.responseBytes else { return nil }
        return String(data: Data(bytes: pointer, count: length), encoding: .utf8)
    }
}
