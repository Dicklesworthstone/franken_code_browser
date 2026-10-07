import Foundation

private typealias IndexPoll = @convention(c) (UnsafeMutableRawPointer?) -> Int32
@_silgen_name("fcb_atlas_create") private func indexedCreate() -> UInt64
@_silgen_name("fcb_atlas_open_cancelable") private func indexedOpen(_ h: UInt64, _ root: UnsafePointer<CChar>?,
    _ files: UInt64, _ poll: IndexPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_index_begin") private func indexedBuildBegin(_ h: UInt64, _ g: UInt64,
    _ files: UInt64, _ fileBytes: UInt64, _ sourceBytes: UInt64, _ grams: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_index_step_cancelable") private func indexedBuildStep(_ h: UInt64, _ g: UInt64,
    _ poll: IndexPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_search_indexed_begin") private func indexedQueryBegin(_ h: UInt64, _ g: UInt64, _ index: UInt64,
    _ text: UnsafePointer<CChar>?, _ hits: UInt64, _ bytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_search_step_cancelable") private func indexedQueryStep(_ h: UInt64, _ g: UInt64,
    _ poll: IndexPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_search_page") private func indexedPage(_ h: UInt64, _ g: UInt64,
    _ start: UInt64, _ count: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_index_open_reader") private func indexedReader(_ h: UInt64, _ reader: UInt64,
    _ index: UInt64, _ file: UInt64, _ revision: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_close") private func indexedClose(_ h: UInt64) -> UInt8
@_silgen_name("fcb_free_string") private func indexedFree(_ p: UnsafeMutablePointer<CChar>?)

extension AtlasIndexedSearchTransport {
    static let native = Self(create: { indexedCreate() }, open: { h, root, flag in
        try polling(flag) { poll, context in root.withCString { indexedOpen(h, $0, 4096, poll, context) } }
    }, indexBegin: { h, g in try take(indexedBuildBegin(h, g, 4096, 1024 * 1024, 32 * 1024 * 1024, 2 * 1024 * 1024)) },
    indexStep: { h, g, flag in try polling(flag) { indexedBuildStep(h, g, $0, $1) } },
    queryBegin: { h, g, index, query in try query.withCString { try take(indexedQueryBegin(h, g, index, $0, 1000, 32 * 1024 * 1024)) } },
    queryStep: { h, g, flag in try polling(flag) { indexedQueryStep(h, g, $0, $1) } },
    page: { try take(indexedPage($0, $1, $2, $3)) }, sourceReader: { try take(indexedReader($0, $1, $2, $3, $4)) },
    close: { indexedClose($0) != 0 }, retirementFailed: {
        FileHandle.standardError.write(Data("ATLAS_INDEX_RETIREMENT_FAILED\n".utf8))
    })

    private static func polling(_ flag: AtlasSearchCancellation,
        call: (IndexPoll, UnsafeMutableRawPointer) -> UnsafeMutablePointer<CChar>?) throws -> String? {
        if flag.isCanceled { throw AtlasSearchError.canceled }
        let poll: IndexPoll = { context in
            guard let context else { return 1 }
            return Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().isCanceled ? 1 : 0
        }
        return try withExtendedLifetime(flag) {
            let pointer = call(poll, Unmanaged.passUnretained(flag).toOpaque())
            // take owns the allocation even when cancellation races its return.
            let result = try take(pointer)
            if flag.isCanceled { throw AtlasSearchError.canceled }
            return result
        }
    }
    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) throws -> String? {
        guard let pointer else { return nil }
        defer { indexedFree(pointer) }
        let count = strnlen(pointer, 8 * 1024 * 1024 + 1)
        guard count <= 8 * 1024 * 1024,
              let text = String(data: Data(bytes: pointer, count: count), encoding: .utf8) else {
            throw AtlasSearchError.invalidResponse
        }
        return text
    }
}
