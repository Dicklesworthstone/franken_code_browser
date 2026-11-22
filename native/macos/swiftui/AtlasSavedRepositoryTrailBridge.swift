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
        guard handle > 0, handle == request.identity.owner, request.limits.isValid else { return nil }
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
