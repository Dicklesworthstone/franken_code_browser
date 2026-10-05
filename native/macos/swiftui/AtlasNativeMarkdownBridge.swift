import Foundation

@_silgen_name("fcb_reader_create") private func markdown_reader_create() -> UInt64
@_silgen_name("fcb_reader_close") private func markdown_reader_close(_ handle: UInt64) -> UInt8
@_silgen_name("fcb_reader_open") private func markdown_reader_open(_ handle: UInt64,
    _ path: UnsafePointer<CChar>?, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_copy_range") private func markdown_reader_copy(_ handle: UInt64,
    _ start: UInt64, _ end: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_document") private func markdown_reader_prepare(_ handle: UInt64,
    _ generation: UInt64, _ width: UInt64, _ source: UInt64, _ lines: UInt64,
    _ items: UInt64, _ blocks: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_document_window") private func markdown_reader_window(_ handle: UInt64,
    _ generation: UInt64, _ first: UInt64, _ count: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_document_headings") private func markdown_reader_headings(_ handle: UInt64,
    _ generation: UInt64, _ first: UInt64, _ count: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_free_string") private func markdown_free_string(_ pointer: UnsafeMutablePointer<CChar>?)

/// No parser, file walk, renderer, clipboard, network request or executor here.
/// Existing Rust worker services retain and decode the capture. The native host
/// supplies scheduling and a grant lease covering open through final close.
struct AtlasNativeMarkdownBridge: AtlasMarkdownBridge, Sendable {
    private func reply(_ work: () -> UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer = work() else { return nil }
        defer { markdown_free_string(pointer) }
        // The Rust producer bounds and NUL-terminates its owned response. Check
        // the consumer's stricter framing limit before decoding any JSON.
        var length = 0
        while length <= AtlasMarkdownAssembly.maxResponseBytes && pointer[length] != 0 { length += 1 }
        guard length <= AtlasMarkdownAssembly.maxResponseBytes else { return nil }
        return String(validatingCString: pointer)
    }
    func create() -> UInt64 { markdown_reader_create() }
    func close(_ handle: UInt64) -> Bool { markdown_reader_close(handle) == 1 }
    func open(_ handle: UInt64, path: String, limit: UInt64) -> String? {
        path.withCString { path in reply { markdown_reader_open(handle, path, limit) } }
    }
    func copy(_ handle: UInt64, count: UInt64) -> String? {
        reply { markdown_reader_copy(handle, 0, count) }
    }
    func prepare(_ handle: UInt64, width: UInt64) -> String? {
        reply { markdown_reader_prepare(handle, 1, width, 262_144, 8192, 8192, 4096) }
    }
    func window(_ handle: UInt64, first: UInt64) -> String? {
        reply { markdown_reader_window(handle, 1, first, 128) }
    }
    func headings(_ handle: UInt64, first: UInt64) -> String? {
        reply { markdown_reader_headings(handle, 1, first, 128) }
    }
}
