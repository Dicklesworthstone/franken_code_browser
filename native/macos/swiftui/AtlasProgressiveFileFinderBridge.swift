import Foundation

@_silgen_name("fcb_file_finder_create") private func createProgressiveFinder() -> UInt64
@_silgen_name("fcb_file_finder_open")
private func openProgressiveFinder(_ handle: UInt64, _ root: UnsafePointer<CChar>?, _ generation: UInt64,
    _ query: UnsafePointer<UInt8>?, _ queryBytes: UInt64, _ mode: UInt32, _ matchCase: UInt8,
    _ maximumResults: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_file_finder_info")
private func progressiveFinderInfo(_ handle: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_file_finder_page")
private func progressiveFinderPage(_ handle: UInt64, _ generation: UInt64, _ revision: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_file_finder_advance")
private func advanceProgressiveFinder(_ handle: UInt64, _ generation: UInt64, _ after: UInt64,
    _ batches: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_file_finder_stop")
private func stopProgressiveFinder(_ handle: UInt64, _ generation: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_file_finder_open_reader")
private func openProgressiveFinderReader(_ handle: UInt64, _ reader: UInt64, _ generation: UInt64,
    _ revision: UInt64, _ file: UInt64, _ bytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_file_finder_cancel") private func cancelProgressiveFinder(_ handle: UInt64) -> UInt8
@_silgen_name("fcb_file_finder_close") private func closeProgressiveFinder(_ handle: UInt64) -> UInt8
@_silgen_name("fcb_free_string") private func freeProgressiveFinderString(_ pointer: UnsafeMutablePointer<CChar>?)

extension AtlasProgressiveFileTransport {
    // This ABI's order differs from the old atlas finder's UInt8 enum.
    static func nativeMode(_ mode: AtlasFileFindMode) -> UInt32 {
        switch mode { case .exact: 0; case .prefix: 1; case .fuzzy: 2 }
    }
    static let native = Self(create: createProgressiveFinder, run: { handle, request in
        switch request {
        case .open(let root, let generation, let query):
            guard root.hasPrefix("/"), root.utf8.count <= 16_384, !root.utf8.contains(0) else { return nil }
            let bytes = Array(query.text.utf8)
            return root.withCString { path in
                bytes.withUnsafeBufferPointer {
                    take(openProgressiveFinder(handle, path, generation, $0.baseAddress, UInt64($0.count),
                        nativeMode(query.mode), query.matchCase ? 1 : 0, 256))
                }
            }
        case .info: return take(progressiveFinderInfo(handle))
        case .page(let generation, let revision): return take(progressiveFinderPage(handle, generation, revision))
        case .advance(let generation, let after, let batches):
            return take(advanceProgressiveFinder(handle, generation, after, batches))
        case .stop(let generation): return take(stopProgressiveFinder(handle, generation))
        case .openReader(let reader, let generation, let revision, let file, let bytes):
            return take(openProgressiveFinderReader(handle, reader, generation, revision, file, bytes))
        }
    }, cancel: { cancelProgressiveFinder($0) != 0 }, close: { closeProgressiveFinder($0) != 0 },
    retirementFailed: {
        FileHandle.standardError.write(Data("ATLAS_PROGRESSIVE_FILE_FINDER_RETIREMENT_FAILED\n".utf8))
    }, readerDomain: AtlasReaderTransport.native.comparison?.domain)

    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { freeProgressiveFinderString(pointer) }
        let count = strnlen(pointer, AtlasProgressiveFileWire.maximumResponseBytes + 1)
        guard count <= AtlasProgressiveFileWire.maximumResponseBytes else { return nil }
        return String(data: Data(bytes: pointer, count: count), encoding: .utf8)
    }
}
