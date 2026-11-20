import Foundation

private typealias ReaderExportPoll = @convention(c) (UnsafeMutableRawPointer?) -> Int32
@_silgen_name("fcb_reader_export_original_cancelable")
private func readerExportOriginal(_ handle: UInt64, _ start: UInt64, _ end: UInt64,
    _ destination: UnsafePointer<CChar>?, _ poll: ReaderExportPoll?,
    _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_free_string") private func readerExportFree(_ pointer: UnsafeMutablePointer<CChar>?)

extension AtlasReaderExportTransport {
    static let native = Self(domain: AtlasReaderTransport.native.comparison?.domain) { handle, request, cancellation in
        guard !cancellation.isCanceled else { throw AtlasReaderExportError.notStarted }
        let poll: ReaderExportPoll = { context in
            guard let context else { return 1 }
            return Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().isCanceled ? 1 : 0
        }
        return try withExtendedLifetime(cancellation) {
            let pointer = request.destination.withCString {
                readerExportOriginal(handle, request.target.start, request.target.end, $0,
                    poll, Unmanaged.passUnretained(cancellation).toOpaque())
            }
            guard let pointer else { return nil }
            defer { readerExportFree(pointer) }
            let count = strnlen(pointer, AtlasReaderExportReceipt.maximumResponseBytes + 1)
            guard count > 0, count <= AtlasReaderExportReceipt.maximumResponseBytes,
                  let text = String(data: Data(bytes: pointer, count: count), encoding: .utf8) else {
                throw AtlasReaderExportError.unverifiedOutcome
            }
            return text // No post-call cancellation check: preserve write effects.
        }
    }
}
