import Foundation

private typealias ProjectLinksPoll = @convention(c) (UnsafeMutableRawPointer?) -> Int32
@_silgen_name("fcb_atlas_links_begin_cancelable")
private func projectLinksBegin(_ handle: UInt64, _ generation: UInt64, _ index: UInt64,
    _ links: UInt64, _ bytes: UInt64, _ poll: ProjectLinksPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_links_step_cancelable")
private func projectLinksStep(_ handle: UInt64, _ generation: UInt64,
    _ poll: ProjectLinksPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_links_page")
private func projectLinksPage(_ handle: UInt64, _ generation: UInt64, _ direction: UInt64,
    _ path: UnsafePointer<CChar>?, _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_links_open_reader")
private func projectLinksReader(_ handle: UInt64, _ reader: UInt64, _ generation: UInt64,
    _ link: UInt64, _ side: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_imports_begin_cancelable")
private func projectImportsBegin(_ handle: UInt64, _ generation: UInt64, _ index: UInt64,
    _ links: UInt64, _ bytes: UInt64, _ poll: ProjectLinksPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_imports_step_cancelable")
private func projectImportsStep(_ handle: UInt64, _ generation: UInt64,
    _ poll: ProjectLinksPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_imports_page")
private func projectImportsPage(_ handle: UInt64, _ generation: UInt64, _ direction: UInt64,
    _ path: UnsafePointer<CChar>?, _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_imports_open_reader")
private func projectImportsReader(_ handle: UInt64, _ reader: UInt64, _ generation: UInt64,
    _ link: UInt64, _ side: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_free_string")
private func releaseProjectLinksString(_ pointer: UnsafeMutablePointer<CChar>?)

extension AtlasProjectLinksTransport {
    static let native = Self(index: .native, readerDomain: AtlasReaderTransport.native.comparison?.domain,
        begin: { handle, generation, index, flag in
            try polling(flag) { poll, context in
                try take(projectLinksBegin(handle, generation, index, 4096, 32 * 1024 * 1024, poll, context))
            }
        }, step: { handle, generation, flag in
            try polling(flag) { poll, context in try take(projectLinksStep(handle, generation, poll, context)) }
        }, page: { handle, generation, direction, path, start, limit in
            if let path {
                return try path.withCString { try take(projectLinksPage(handle, generation, direction.code, $0, start, limit)) }
            }
            return try take(projectLinksPage(handle, generation, direction.code, nil, start, limit))
        }, openReader: { handle, reader, generation, link, side in
            try take(projectLinksReader(handle, reader, generation, link, side.code))
        }, imports: .init(begin: { handle, generation, index, flag in
            try polling(flag) { poll, context in
                try take(projectImportsBegin(handle, generation, index, 4096, 32 * 1024 * 1024, poll, context))
            }
        }, step: { handle, generation, flag in
            try polling(flag) { poll, context in try take(projectImportsStep(handle, generation, poll, context)) }
        }, page: { handle, generation, direction, path, start, limit in
            if let path {
                return try path.withCString { try take(projectImportsPage(handle, generation, direction.code, $0, start, limit)) }
            }
            return try take(projectImportsPage(handle, generation, direction.code, nil, start, limit))
        }, openReader: { handle, reader, generation, link, side in
            try take(projectImportsReader(handle, reader, generation, link, side.code))
        }))

    private static func polling<T>(_ flag: AtlasSearchCancellation,
        _ work: (ProjectLinksPoll, UnsafeMutableRawPointer) throws -> T) rethrows -> T {
        let poll: ProjectLinksPoll = { context in
            guard let context else { return 1 }
            return Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().isCanceled ? 1 : 0
        }
        return try withExtendedLifetime(flag) {
            try work(poll, Unmanaged.passUnretained(flag).toOpaque())
        }
    }

    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) throws -> String? {
        guard let pointer else { return nil }
        defer { releaseProjectLinksString(pointer) }
        let count = strnlen(pointer, 8 * 1024 * 1024 + 1)
        guard count > 0, count <= 8 * 1024 * 1024,
              let json = String(data: Data(bytes: pointer, count: count), encoding: .utf8) else {
            throw AtlasProjectLinksError.invalidResponse
        }
        return json
    }
}
