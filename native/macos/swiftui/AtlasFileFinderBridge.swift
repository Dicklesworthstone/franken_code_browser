import Foundation

private typealias FileFinderPoll = @convention(c) (UnsafeMutableRawPointer?) -> Int32
@_silgen_name("fcb_atlas_create") private func createFileCatalog() -> UInt64
@_silgen_name("fcb_atlas_open_cancelable")
private func openFileCatalog(_ handle: UInt64, _ root: UnsafePointer<CChar>?, _ limit: UInt64,
    _ poll: FileFinderPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_find_files_cancelable")
private func findCatalogFiles(_ handle: UInt64, _ generation: UInt64, _ query: UnsafePointer<CChar>?,
    _ limit: UInt64, _ mode: UInt8, _ caseMode: UInt8,
    _ poll: FileFinderPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_file_results")
private func fileResults(_ handle: UInt64, _ generation: UInt64, _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_file_select")
private func selectCatalogFile(_ handle: UInt64, _ generation: UInt64, _ file: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_close") private func closeFileCatalog(_ handle: UInt64) -> UInt8
@_silgen_name("fcb_free_string") private func freeFileString(_ pointer: UnsafeMutablePointer<CChar>?)

extension AtlasFileFinderTransport {
    static let native = Self(create: createFileCatalog, open: { handle, root, limit, flag in
        withFlag(flag) { poll, context in
            root.withCString { take(openFileCatalog(handle, $0, limit, poll, context)) }
        }
    }, find: { handle, generation, query, limit, flag in
        withFlag(flag) { poll, context in
            query.text.withCString {
                take(findCatalogFiles(handle, generation, $0, limit, query.mode.rawValue, query.matchCase ? 1 : 0, poll, context))
            }
        }
    }, page: { take(fileResults($0, $1, $2, $3)) }, select: { take(selectCatalogFile($0, $1, $2)) },
    close: { closeFileCatalog($0) != 0 }, retirementFailed: {
        FileHandle.standardError.write(Data("ATLAS_FILE_CATALOG_RETIREMENT_FAILED\n".utf8))
    })

    private static func withFlag<T>(_ flag: AtlasSearchCancellation,
        _ work: (FileFinderPoll, UnsafeMutableRawPointer) -> T) -> T {
        let poll: FileFinderPoll = { context in
            guard let context else { return 1 }
            return Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().isCanceled ? 1 : 0
        }
        return withExtendedLifetime(flag) { work(poll, Unmanaged.passUnretained(flag).toOpaque()) }
    }
    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { freeFileString(pointer) }
        // C owns a NUL-terminated allocation. Bound copying before Foundation
        // materialization; unknown fields count toward the same response cap.
        let count = strnlen(pointer, 4 * 1024 * 1024 + 1)
        guard count <= 4 * 1024 * 1024 else { return nil }
        return String(data: Data(bytes: pointer, count: count), encoding: .utf8)
    }
}
