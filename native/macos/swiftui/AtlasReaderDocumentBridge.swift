import Foundation

@_silgen_name("fcb_reader_document")
private func fcb_reader_document(_ handle: UInt64, _ generation: UInt64, _ width: UInt64,
    _ source: UInt64, _ lines: UInt64, _ items: UInt64, _ blocks: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_document_window")
private func fcb_reader_document_window(_ handle: UInt64, _ generation: UInt64, _ first: UInt64, _ count: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_document_headings")
private func fcb_reader_document_headings(_ handle: UInt64, _ generation: UInt64, _ first: UInt64, _ count: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_document_heading")
private func fcb_reader_document_heading(_ handle: UInt64, _ generation: UInt64, _ slug: UnsafePointer<CChar>?, _ count: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_document_from_source")
private func fcb_reader_document_from_source(_ handle: UInt64, _ generation: UInt64, _ offset: UInt64, _ count: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_document_source")
private func fcb_reader_document_source(_ handle: UInt64, _ generation: UInt64, _ start: UInt64, _ end: UInt64, _ context: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_document_copy")
private func fcb_reader_document_copy(_ handle: UInt64, _ generation: UInt64, _ start: UInt64, _ end: UInt64, _ mode: UInt8) -> UnsafeMutablePointer<CChar>?

@_silgen_name("fcb_reader_document_links_prepare")
private func fcb_reader_document_links_prepare(_ handle: UInt64, _ generation: UInt64, _ maxLinks: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_document_links")
private func fcb_reader_document_links(_ handle: UInt64, _ generation: UInt64, _ first: UInt64, _ count: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_document_link")
private func fcb_reader_document_link(_ handle: UInt64, _ generation: UInt64, _ link: UInt64, _ count: UInt64) -> UnsafeMutablePointer<CChar>?

@_silgen_name("fcb_reader_document_display")
private func fcb_reader_document_display(_ handle: UInt64, _ generation: UInt64, _ width: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_document_display_page")
private func fcb_reader_document_display_page(_ handle: UInt64, _ generation: UInt64, _ first: UInt64, _ count: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_document_display_font")
private func fcb_reader_document_display_font(_ handle: UInt64, _ generation: UInt64, _ font: UInt64, _ offset: UInt64, _ count: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_reader_document_display_from_source")
private func fcb_reader_document_display_from_source(_ handle: UInt64, _ generation: UInt64, _ offset: UInt64, _ count: UInt64) -> UnsafeMutablePointer<CChar>?

extension AtlasReaderDocumentTransport {
    static let native = Self(prepare: { handle, generation, width in
        take(fcb_reader_document(handle, generation, width, AtlasReaderDocumentLimits.sourceBytes,
            AtlasReaderDocumentLimits.flowLines, AtlasReaderDocumentLimits.flowItems, AtlasReaderDocumentLimits.blocks))
    }, window: { take(fcb_reader_document_window($0, $1, $2, $3)) },
    headings: { take(fcb_reader_document_headings($0, $1, $2, $3)) },
    heading: { handle, generation, slug, count in
        slug.withCString { take(fcb_reader_document_heading(handle, generation, $0, count)) }
    }, fromSource: { take(fcb_reader_document_from_source($0, $1, $2, $3)) },
    source: { take(fcb_reader_document_source($0, $1, $2, $3, $4)) },
    copy: { take(fcb_reader_document_copy($0, $1, $2, $3, $4)) },
    prepareLinks: { take(fcb_reader_document_links_prepare($0, $1, $2)) },
    links: { take(fcb_reader_document_links($0, $1, $2, $3)) },
    link: { take(fcb_reader_document_link($0, $1, $2, $3)) },
    prepareDisplay: { take(fcb_reader_document_display($0, $1, $2)) },
    displayPage: { take(fcb_reader_document_display_page($0, $1, $2, $3)) },
    displayFont: { take(fcb_reader_document_display_font($0, $1, $2, $3, $4)) },
    displayFromSource: { take(fcb_reader_document_display_from_source($0, $1, $2, $3)) })

    private static func take(_ pointer: UnsafeMutablePointer<CChar>?) -> String? {
        guard let pointer else { return nil }
        defer { fcb_free_string(pointer) }
        var length = 0
        while length <= AtlasReaderLimits.responseBytes && pointer[length] != 0 { length += 1 }
        guard length <= AtlasReaderLimits.responseBytes else { return nil }
        return String(validatingCString: pointer)
    }
}
