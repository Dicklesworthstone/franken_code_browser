import Foundation

/// Native adaptation only. FrankenMarkdown owns parsing, flow, heading slugs
/// and enclosing-source provenance. Display offsets below are local UTF-16,
/// never the engine's global rendered UTF-8 or original source byte offsets.
enum AtlasMarkdownError: Error, Equatable, Sendable {
    case invalidRequest, sourceLimit, unavailable, invalidResponse, changedSource, canceled, closeFailed

    var message: String {
        switch self {
        case .invalidRequest: return "Markdown preview requires a valid project-relative file and width."
        case .sourceLimit: return "Markdown preview currently admits complete UTF-8 documents up to 256 KiB. Source remains available."
        case .unavailable: return "The shared Markdown engine could not prepare this document within its source and flow limits. Source remains available."
        case .invalidResponse: return "Markdown preview was not published because its capture, layout or source map could not be validated."
        case .changedSource: return "The file changed since this source was captured. Reopen the file before preparing a new preview."
        case .canceled: return "Markdown preparation canceled. Source remains available."
        case .closeFailed: return "The Markdown reader could not confirm release of its backend handle."
        }
    }
}

struct AtlasMarkdownRequest: Sendable {
    static let maxSourceBytes = 262_144
    let id: UUID
    let root: String
    let path: String
    let source: String
    let width: Int
    var fullPath: String { root + (root.hasSuffix("/") ? "" : "/") + path }

    init(root: String, path: String, source: String, width: Int, id: UUID = UUID()) throws {
        guard !root.isEmpty, root.utf8.count <= 16_384, !root.utf8.contains(0),
              !path.isEmpty, path.utf8.count <= 16_384, !path.utf8.contains(0),
              path.utf8.first != 47, (4...512).contains(width),
              !path.utf8.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                  $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
              }) else { throw AtlasMarkdownError.invalidRequest }
        guard source.utf8.count <= Self.maxSourceBytes else { throw AtlasMarkdownError.sourceLimit }
        self.id = id; self.root = root; self.path = path; self.source = source; self.width = width
    }

    static func supports(path: String) -> Bool {
        ["md", "markdown", "mdown"].contains((path as NSString).pathExtension.lowercased())
    }
}

/// All integer fields on this ABI are canonical, full-width decimal strings.
struct AtlasMarkdownInteger: Decodable, Equatable, Sendable {
    let value: UInt64
    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        let text = try container.decode(String.self)
        guard let value = UInt64(text), String(value) == text else {
            throw AtlasMarkdownError.invalidResponse
        }
        self.value = value
    }
}

struct AtlasMarkdownRange: Decodable, Equatable, Sendable {
    let start: AtlasMarkdownInteger
    let end: AtlasMarkdownInteger
    var lower: UInt64 { start.value }
    var upper: UInt64 { end.value }
}

struct AtlasMarkdownRow: Decodable, Sendable {
    let flow_line: AtlasMarkdownInteger
    let text: String
    let rendered_utf8_range: AtlasMarkdownRange
    let enclosing_original_range: AtlasMarkdownRange?
}

struct AtlasMarkdownHeading: Decodable, Sendable {
    let slug: String
    let title: String
    let rendered_utf8_offset: AtlasMarkdownInteger
    let original_range: AtlasMarkdownRange
}

struct AtlasMarkdownEnvelope: Decodable, Equatable, Sendable {
    let schema, status, command: String
    let owner, file_id, source_revision, captured_bytes: AtlasMarkdownInteger
    let additional_source_bytes_read: AtlasMarkdownInteger
    let encoding: String

    func sameCapture(as other: Self) -> Bool {
        owner == other.owner && file_id == other.file_id && source_revision == other.source_revision
            && captured_bytes == other.captured_bytes && encoding == other.encoding
    }
}

struct AtlasMarkdownSummary: Decodable, Equatable, Sendable {
    let document_generation: AtlasMarkdownInteger
    let document_ready, layout_complete, native_shaped: Bool
    let rendering, source_mapping: String
    let width_columns, total_flow_lines, total_headings, rendered_utf8_bytes, parser_source_base: AtlasMarkdownInteger
}

struct AtlasMarkdownPage: Decodable {
    let first_flow_line: AtlasMarkdownInteger
    let flow_lines: [AtlasMarkdownRow]
    let next_flow_line: AtlasMarkdownInteger?
    let whole_document_visible: Bool
}

struct AtlasMarkdownHeadingPage: Decodable {
    let headings: [AtlasMarkdownHeading]
    let next_heading: AtlasMarkdownInteger?
}

struct AtlasMarkdownCopy: Decodable {
    let copy_domain: String
    let original_range: AtlasMarkdownRange
    let original_hex: String
}

/// Complete, immutable UI packet. No Rust handle, file grant, parser or native
/// view crosses the worker boundary. Original source is retained separately by
/// the reader; this packet's request ID is checked before native publication.
struct AtlasMarkdownDocument: Sendable {
    let requestID: UUID
    let width: Int
    let rows: [AtlasMarkdownRow]
    let headings: [AtlasMarkdownHeading]
    let displayText: String
    let displayRanges: [NSRange]
    let headingRows: [Int?]

    init(request: AtlasMarkdownRequest, rows: [AtlasMarkdownRow], headings: [AtlasMarkdownHeading]) {
        requestID = request.id; width = request.width; self.rows = rows; self.headings = headings
        var text = "", ranges: [NSRange] = [], location = 0
        for row in rows {
            let length = row.text.utf16.count
            ranges.append(NSRange(location: location, length: length))
            text += row.text + "\n"
            location += length + 1
        }
        displayText = text; displayRanges = ranges
        // Heading offsets are upstream rendered bytes; they are NOT native
        // string indexes. Binary-search the validated ordered flow intervals.
        headingRows = headings.map { heading in
            let offset = heading.rendered_utf8_offset.value
            var lo = 0, hi = rows.count
            while lo < hi {
                let mid = lo + (hi - lo) / 2
                if rows[mid].rendered_utf8_range.upper <= offset { lo = mid + 1 }
                else { hi = mid }
            }
            return lo < rows.count && rows[lo].rendered_utf8_range.lower <= offset ? lo : nil
        }
    }

    /// Caret/selection -> one enclosing source region. A cross-row selection
    /// with different regions is deliberately not turned into a fake exact map.
    func sourceRange(for selection: NSRange) -> AtlasMarkdownRange? {
        guard selection.location != NSNotFound, selection.location >= 0, selection.length >= 0,
              selection.location <= displayText.utf16.count,
              selection.length <= displayText.utf16.count - selection.location else { return nil }
        var result: AtlasMarkdownRange?
        var found = false
        for (row, range) in zip(rows, displayRanges) {
            let intersects = selection.length == 0
                ? selection.location >= range.location && selection.location < range.location + range.length
                : NSIntersectionRange(selection, range).length > 0
            if !intersects { continue }
            guard let original = row.enclosing_original_range else { return nil }
            if found && result != original { return nil }
            found = true; result = original
        }
        return result
    }

    /// Source-to-preview correspondence is explicitly enclosing-region level.
    func row(atSourceOffset offset: UInt64) -> Int? {
        rows.firstIndex { row in
            row.enclosing_original_range.map { $0.lower <= offset && offset < $0.upper } ?? false
        }
    }
}

/// Validates the published wire contract, not Markdown syntax. There is no
/// parsing/normalization/rendering policy duplicated from FrankenMarkdown.
struct AtlasMarkdownAssembly {
    static let maxResponseBytes = 1_048_576
    static let maxTransferredBytes = 16 * 1_048_576
    static let maxTextBytes = 2 * 1_048_576
    let request: AtlasMarkdownRequest
    let capture: AtlasMarkdownEnvelope
    let summary: AtlasMarkdownSummary
    private(set) var rows: [AtlasMarkdownRow] = []
    private(set) var headings: [AtlasMarkdownHeading] = []
    private var textBytes = 0
    private var slugs = Set<[UInt8]>()
    private let sourceBytes: [UInt8]

    init(request: AtlasMarkdownRequest, capture: AtlasMarkdownEnvelope, summary: AtlasMarkdownSummary) throws {
        guard summary.document_generation.value == 1, summary.document_ready, summary.layout_complete,
              !summary.native_shaped, summary.rendering == "logical-frankenmarkdown-flow",
              summary.source_mapping == "enclosing-regions-not-glyph-exact",
              summary.width_columns.value == UInt64(request.width),
              summary.total_flow_lines.value <= 8192, summary.total_headings.value <= 4096,
              summary.rendered_utf8_bytes.value <= UInt64(Self.maxTextBytes),
              summary.parser_source_base.value == (request.source.utf8.starts(with: [239, 187, 191]) ? 3 : 0)
        else { throw AtlasMarkdownError.invalidResponse }
        self.request = request; self.capture = capture; self.summary = summary
        sourceBytes = Array(request.source.utf8)
    }

    mutating func append(_ page: AtlasMarkdownPage) throws {
        let total = Int(summary.total_flow_lines.value)
        guard page.first_flow_line.value == UInt64(rows.count), page.flow_lines.count <= 128,
              page.flow_lines.count <= total - rows.count else { throw AtlasMarkdownError.invalidResponse }
        let end = rows.count + page.flow_lines.count
        guard (end == total ? page.next_flow_line == nil : page.next_flow_line?.value == UInt64(end)),
              end == total || !page.flow_lines.isEmpty,
              page.whole_document_visible == (rows.isEmpty && end == total) else {
            throw AtlasMarkdownError.invalidResponse
        }
        for row in page.flow_lines {
            let range = row.rendered_utf8_range
            guard row.flow_line.value == UInt64(rows.count), range.lower <= range.upper,
                  range.upper <= summary.rendered_utf8_bytes.value,
                  range.lower >= (rows.last?.rendered_utf8_range.upper ?? 0),
                  UInt64(row.text.utf8.count) <= range.upper - range.lower else {
                throw AtlasMarkdownError.invalidResponse
            }
            if let original = row.enclosing_original_range { try validateSource(original) }
            try charge(row.text.utf8.count + 1)
            rows.append(row)
        }
    }

    mutating func append(_ page: AtlasMarkdownHeadingPage) throws {
        let total = Int(summary.total_headings.value)
        guard page.headings.count <= 128, page.headings.count <= total - headings.count else {
            throw AtlasMarkdownError.invalidResponse
        }
        let end = headings.count + page.headings.count
        guard (end == total ? page.next_heading == nil : page.next_heading?.value == UInt64(end)),
              end == total || !page.headings.isEmpty else { throw AtlasMarkdownError.invalidResponse }
        for heading in page.headings {
            guard !heading.slug.isEmpty, heading.slug.utf8.count <= 4096,
                  !heading.slug.utf8.contains(0), slugs.insert(Array(heading.slug.utf8)).inserted,
                  heading.rendered_utf8_offset.value <= summary.rendered_utf8_bytes.value,
                  heading.rendered_utf8_offset.value >= (headings.last?.rendered_utf8_offset.value ?? 0) else {
                throw AtlasMarkdownError.invalidResponse
            }
            try validateSource(heading.original_range)
            try charge(heading.title.utf8.count + heading.slug.utf8.count)
            headings.append(heading)
        }
    }

    func finish() throws -> AtlasMarkdownDocument {
        guard rows.count == Int(summary.total_flow_lines.value), headings.count == Int(summary.total_headings.value)
        else { throw AtlasMarkdownError.invalidResponse }
        return AtlasMarkdownDocument(request: request, rows: rows, headings: headings)
    }

    private func validateSource(_ range: AtlasMarkdownRange) throws {
        guard range.lower < range.upper, range.lower >= summary.parser_source_base.value,
              range.upper <= UInt64(sourceBytes.count),
              boundary(Int(range.lower)), boundary(Int(range.upper)) else {
            throw AtlasMarkdownError.invalidResponse
        }
    }
    private func boundary(_ offset: Int) -> Bool {
        offset == sourceBytes.count || sourceBytes[offset] & 0xC0 != 0x80
    }
    private mutating func charge(_ count: Int) throws {
        guard count <= Self.maxTextBytes - textBytes else { throw AtlasMarkdownError.invalidResponse }
        textBytes += count
    }
}
