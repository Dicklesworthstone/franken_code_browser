import Foundation

private typealias SnapshotExportPoll = @convention(c) (UnsafeMutableRawPointer?) -> Int32
@_silgen_name("fcb_snapshot_save_cancelable") private func snapshotExportSave(
    _ root: UnsafePointer<CChar>?, _ destination: UnsafePointer<CChar>?,
    _ files: UInt64, _ fileBytes: UInt64, _ sourceBytes: UInt64, _ respectIgnores: UInt64,
    _ poll: SnapshotExportPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_free_string") private func snapshotExportFree(_ pointer: UnsafeMutablePointer<CChar>?)

extension AtlasSnapshotExportTransport {
    static let native = Self { request, cancellation in
        // Pre-entry refusal is knowable. After entry, cancellation is only a
        // request to the writer; its receipt may describe an existing file.
        guard !cancellation.isCanceled else { throw AtlasSnapshotExportError.notStarted }
        let poll: SnapshotExportPoll = { context in
            guard let context else { return 1 }
            return Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().isCanceled ? 1 : 0
        }
        return try withExtendedLifetime(cancellation) {
            let pointer = request.root.withCString { root in
                request.destination.withCString { destination in
                    snapshotExportSave(root, destination, request.limits.files, request.limits.fileBytes,
                        request.limits.sourceBytes, request.limits.respectIgnores ? 1 : 0,
                        poll, Unmanaged.passUnretained(cancellation).toOpaque())
                }
            }
            guard let pointer else { return nil }
            defer { snapshotExportFree(pointer) }
            let count = strnlen(pointer, AtlasSnapshotExportReceipt.maximumResponseBytes + 1)
            guard count <= AtlasSnapshotExportReceipt.maximumResponseBytes,
                  let text = String(data: Data(bytes: pointer, count: count), encoding: .utf8) else {
                throw AtlasSnapshotExportError.unverifiedOutcome
            }
            // No post-call cancellation check: never discard a write effect.
            return text
        }
    }
}
