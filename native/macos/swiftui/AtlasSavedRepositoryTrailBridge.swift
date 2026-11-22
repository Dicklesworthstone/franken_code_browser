import Foundation

@_silgen_name("fcb_saved_trail_attach") private func savedTrailAttach(_ handle: UInt64,
    _ generation: UInt64, _ path: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_trail_page") private func savedTrailPage(_ handle: UInt64,
    _ generation: UInt64, _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_trail_detach") private func savedTrailDetach(_ handle: UInt64,
    _ generation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_open_trail_reader") private func savedTrailReader(_ handle: UInt64,
    _ reader: UInt64, _ generation: UInt64, _ item: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_trail_write") private func savedTrailWrite(_ handle: UInt64,
    _ operation: UInt64, _ parentGeneration: UInt64, _ parentDigest: UnsafePointer<CChar>?,
    _ member: UInt64, _ start: UInt64, _ end: UInt64,
    _ title: UnsafePointer<UInt8>?, _ titleBytes: UInt64,
    _ rationale: UnsafePointer<UInt8>?, _ rationaleBytes: UInt64,
    _ destination: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_trail_write_receipt") private func savedTrailWriteReceipt(_ handle: UInt64,
    _ operation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_trail_export") private func savedTrailExport(_ handle: UInt64,
    _ operation: UInt64, _ generation: UInt64, _ digest: UnsafePointer<CChar>?, _ destination: UnsafePointer<CChar>?,
    _ entries: UInt64, _ verificationBytes: UInt64, _ selectedBytes: UInt64,
    _ outputBytes: UInt64, _ outputScalars: UInt64) -> UnsafeMutablePointer<CChar>?
private typealias SavedTrailExportPoll = @convention(c) (UnsafeMutableRawPointer?) -> Int32
@_silgen_name("fcb_saved_open_trail_reader_with_archive_cancelable") private func savedTrailArchiveReader(_ handle: UInt64,
    _ reader: UInt64, _ generation: UInt64, _ item: UInt64, _ digest: UnsafePointer<CChar>?,
    _ path: UnsafePointer<CChar>?, _ validationBytes: UInt64,
    _ poll: SavedTrailExportPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_trail_export_with_archives_cancelable") private func savedTrailExportWithArchives(_ handle: UInt64,
    _ operation: UInt64, _ generation: UInt64, _ digest: UnsafePointer<CChar>?, _ destination: UnsafePointer<CChar>?,
    _ entries: UInt64, _ verificationBytes: UInt64, _ selectedBytes: UInt64, _ outputBytes: UInt64, _ outputScalars: UInt64,
    _ paths: UnsafePointer<UnsafePointer<CChar>?>?, _ count: UInt64, _ validationBytes: UInt64,
    _ poll: SavedTrailExportPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_saved_trail_export_receipt") private func savedTrailExportReceipt(_ handle: UInt64,
    _ operation: UInt64) -> UnsafeMutablePointer<CChar>?
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
    }, write: { handle, request in
        guard handle > 0, handle == request.identity.owner else { return nil }
        let title = Array(request.title.utf8), rationale = Array(request.rationale.utf8)
        return (request.parent?.digest ?? "").withCString { parentDigest in
            request.destination.withCString { destination in
                title.withUnsafeBufferPointer { title in
                    rationale.withUnsafeBufferPointer { rationale in
                        take(savedTrailWrite(handle, request.operation, request.parent?.generation ?? 0, parentDigest,
                            request.selection.member.id, request.selection.range.start, request.selection.range.end,
                            title.baseAddress, UInt64(title.count), rationale.baseAddress, UInt64(rationale.count), destination))
                    }
                }
            }
        }
    }, writeReceipt: { handle, operation in
        guard handle > 0, operation > 0 else { return nil }
        return take(savedTrailWriteReceipt(handle, operation))
    }, export: { handle, request in
        guard handle > 0, handle == request.identity.owner, request.limits.isValid,
              request.additionalArchives.isEmpty else { return nil }
        let limits = request.limits
        return request.trail.digest.withCString { digest in
            request.destination.withCString { destination in
                take(savedTrailExport(handle, request.operation, request.trail.generation, digest, destination,
                    limits.entries, limits.verificationBytes, limits.selectedBytes, limits.outputBytes, limits.outputScalars))
            }
        }
    }, exportReceipt: { handle, operation in
        guard handle > 0, operation > 0 else { return nil }
        return take(savedTrailExportReceipt(handle, operation))
    }, exportWithArchives: { handle, request, cancellation in
        guard handle > 0, handle == request.identity.owner, request.limits.isValid, !cancellation.isCanceled else { return nil }
        let poll: SavedTrailExportPoll = { context in
            guard let context else { return 1 }
            return Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().isCanceled ? 1 : 0
        }
        let limits = request.limits
        return withExtendedLifetime(cancellation) {
            withArchivePaths(request.additionalArchives) { paths in
                request.trail.digest.withCString { digest in
                    request.destination.withCString { destination in
                        take(savedTrailExportWithArchives(handle, request.operation, request.trail.generation, digest, destination,
                            limits.entries, limits.verificationBytes, limits.selectedBytes, limits.outputBytes, limits.outputScalars,
                            paths, UInt64(request.additionalArchives.count), limits.archiveValidationBytes, poll,
                            Unmanaged.passUnretained(cancellation).toOpaque()))
                    }
                }
            }
        }
    }, openArchiveReader: { handle, reader, request, cancellation in
        guard handle > 0, handle == request.trail.identity.owner, reader > 0, reader != handle,
              !cancellation.isCanceled else { return nil }
        let poll: SavedTrailExportPoll = { context in
            guard let context else { return 1 }
            return Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().isCanceled ? 1 : 0
        }
        return withExtendedLifetime(cancellation) {
            request.trail.digest.withCString { digest in
                request.path.withCString { path in
                    take(savedTrailArchiveReader(handle, reader, request.trail.generation, request.entry.id,
                        digest, path, request.validationBytes, poll, Unmanaged.passUnretained(cancellation).toOpaque()))
                }
            }
        }
    })
    /// Nest the bounded string lifetimes around the synchronous foreign call;
    /// neither a temporary C string nor its pointer array may escape it.
    private static func withArchivePaths<T>(_ paths: [String],
        _ call: (UnsafePointer<UnsafePointer<CChar>?>?) -> T) -> T {
        var pointers: [UnsafePointer<CChar>?] = []
        pointers.reserveCapacity(paths.count)
        func visit(_ index: Int) -> T {
            guard index < paths.count else {
                return pointers.withUnsafeBufferPointer { call($0.baseAddress) }
            }
            return paths[index].withCString { pointer in
                pointers.append(pointer)
                defer { pointers.removeLast() }
                return visit(index + 1)
            }
        }
        return visit(0)
    }
    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { savedTrailFree(pointer) }
        var count = 0
        while count <= AtlasSavedWire.maximumResponseBytes && pointer[count] != 0 { count += 1 }
        guard count > 0, count <= AtlasSavedWire.maximumResponseBytes else { return nil }
        return String(data: Data(bytes: pointer, count: count), encoding: .utf8)
    }
}
