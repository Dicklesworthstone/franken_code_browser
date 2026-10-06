import Foundation

/// The native host consumes FrankenMarkdown's logical flow. It never parses
/// Markdown, invents heading slugs, fetches assets, or follows document links.
struct AtlasReaderDocumentTransport: Sendable {
    let prepare: @Sendable (UInt64, UInt64, UInt64) -> String?
    let window: @Sendable (UInt64, UInt64, UInt64, UInt64) -> String?
    let headings: @Sendable (UInt64, UInt64, UInt64, UInt64) -> String?
    let heading: @Sendable (UInt64, UInt64, String, UInt64) -> String?
    let fromSource: @Sendable (UInt64, UInt64, UInt64, UInt64) -> String?
    let source: @Sendable (UInt64, UInt64, UInt64, UInt64, UInt64) -> String?
    let copy: @Sendable (UInt64, UInt64, UInt64, UInt64, UInt8) -> String?
}

enum AtlasReaderDocumentLimits {
    // Match DocumentReadOptions::default(), not the larger whole-source reader.
    static let sourceBytes: UInt64 = 64 * 1024
    static let flowLines: UInt64 = 8192
    static let flowItems: UInt64 = 8192
    static let blocks: UInt64 = 4096
    static let pageLines: UInt64 = 64
    static let context: UInt64 = 2048
    static let pageTextBytes: UInt64 = 256 * 1024
}

struct AtlasDocumentByteRange: Equatable, Sendable, Decodable {
    let start: UInt64
    let end: UInt64
    var length: UInt64 { end - start }
    init(start: UInt64, end: UInt64) { self.start = start; self.end = end }
    init(from decoder: Decoder) throws {
        let value = try AtlasReaderWire.Range(from: decoder).values()
        start = value.start; end = value.end
    }
    func valid(upTo limit: UInt64, nonempty: Bool = false) -> Bool {
        start <= end && end <= limit && (!nonempty || start < end)
    }
}

struct AtlasReaderDocumentInventory: Sendable {
    let identity: AtlasReaderIdentity
    let generation: UInt64
    let width: UInt64
    let totalLines: UInt64
    let totalHeadings: UInt64
    let renderedBytes: UInt64
    let sourceBase: UInt64
    func matches(_ other: Self) -> Bool {
        identity.matches(other.identity) && generation == other.generation && width == other.width
            && totalLines == other.totalLines && totalHeadings == other.totalHeadings
            && renderedBytes == other.renderedBytes && sourceBase == other.sourceBase
    }
}

struct AtlasReaderDocumentLine: Identifiable, Sendable, Equatable {
    let id: UInt64
    let text: String
    let rendered: AtlasDocumentByteRange
    let enclosingOriginal: AtlasDocumentByteRange?
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.id == rhs.id && lhs.text.utf8.elementsEqual(rhs.text.utf8)
            && lhs.rendered == rhs.rendered && lhs.enclosingOriginal == rhs.enclosingOriginal
    }
}

/// A flow row is not a physical source line. A target carries the original
/// page token so a delayed click cannot bind the same row number after reflow.
struct AtlasReaderDocumentTarget: Sendable, Equatable {
    let inventory: AtlasReaderDocumentInventory
    let pageID: UUID
    let line: AtlasReaderDocumentLine
    // Upstream row text trims layout trailing whitespace. Select only the
    // displayed UTF-8 prefix, not those padding bytes or the following newline.
    var rendered: AtlasDocumentByteRange {
        .init(start: line.rendered.start, end: line.rendered.start + UInt64(line.text.utf8.count))
    }
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.inventory.matches(rhs.inventory) && lhs.pageID == rhs.pageID && lhs.line == rhs.line
    }
}

struct AtlasReaderDocumentPage: Sendable {
    let id: UUID
    let inventory: AtlasReaderDocumentInventory
    let first: UInt64
    let lines: [AtlasReaderDocumentLine]
    let next: UInt64?
    let wholeDocumentVisible: Bool
    func target(at index: UInt64) -> AtlasReaderDocumentTarget? {
        guard let line = lines.first(where: { $0.id == index }), !line.text.isEmpty,
              line.enclosingOriginal != nil else { return nil }
        return .init(inventory: inventory, pageID: id, line: line)
    }
}

struct AtlasReaderDocumentHeading: Identifiable, Sendable, Equatable {
    let slug: String
    let title: String
    let renderedOffset: UInt64
    let original: AtlasDocumentByteRange
    // Swift String equality merges canonically equivalent spellings. Canonical
    // upstream slug bytes, not that equivalence or a displayed title, are IDs.
    var id: Data { Data(slug.utf8) }
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.slug.utf8.elementsEqual(rhs.slug.utf8) && lhs.title.utf8.elementsEqual(rhs.title.utf8)
            && lhs.renderedOffset == rhs.renderedOffset && lhs.original == rhs.original
    }
}
struct AtlasReaderDocumentHeadingTarget: Sendable, Equatable {
    let inventory: AtlasReaderDocumentInventory
    let pageID: UUID
    let heading: AtlasReaderDocumentHeading
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.inventory.matches(rhs.inventory) && lhs.pageID == rhs.pageID && lhs.heading == rhs.heading
    }
}
struct AtlasReaderDocumentHeadings: Sendable {
    let id: UUID
    let inventory: AtlasReaderDocumentInventory
    let first: UInt64
    let rows: [AtlasReaderDocumentHeading]
    let next: UInt64?
    func target(slug: String) -> AtlasReaderDocumentHeadingTarget? {
        rows.first(where: { $0.slug.utf8.elementsEqual(slug.utf8) }).map {
            .init(inventory: inventory, pageID: id, heading: $0)
        }
    }
}

enum AtlasReaderDocumentCopyMode: UInt8, Sendable { case renderedText = 0, enclosingMarkdown = 1 }
struct AtlasReaderDocumentCopy: Sendable {
    let target: AtlasReaderDocumentTarget
    let enclosingOriginal: AtlasDocumentByteRange
    let mode: AtlasReaderDocumentCopyMode
    let value: String
}
struct AtlasReaderDocumentSource: Sendable {
    let target: AtlasReaderDocumentTarget
    let visible: AtlasDocumentByteRange
    let text: String
    let originalHex: String
    let enclosingOriginal: AtlasDocumentByteRange
    let selectionUTF8: AtlasDocumentByteRange
    let selectedHex: String
}

enum AtlasReaderDocumentRequest: Sendable {
    case prepare(width: UInt64)
    case window(first: UInt64)
    case headings(first: UInt64)
    case heading(AtlasReaderDocumentHeadingTarget)
    case fromSource(offset: UInt64)
    case source(AtlasReaderDocumentTarget)
    case copy(AtlasReaderDocumentTarget, AtlasReaderDocumentCopyMode)
    var command: String {
        switch self {
        case .prepare: "document-prepare"
        case .window: "document-window"
        case .headings: "document-headings"
        case .heading: "document-heading"
        case .fromSource: "document-from-source"
        case .source: "document-source"
        case .copy: "document-copy"
        }
    }
}

enum AtlasReaderDocumentResult: Sendable {
    case prepared(AtlasReaderDocumentPage, AtlasReaderDocumentHeadings)
    case page(AtlasReaderDocumentPage)
    case headings(AtlasReaderDocumentHeadings)
    case source(AtlasReaderDocumentSource)
    case copy(AtlasReaderDocumentCopy)
}

/// Immutable worker descriptor. Only native marshaling and validation happen
/// here; the Rust reader owns all source/flow transformations and their budgets.
struct AtlasReaderDocumentJob: Sendable {
    let id: UUID
    let info: AtlasReaderInfo
    let generation: UInt64
    let inventory: AtlasReaderDocumentInventory?
    let request: AtlasReaderDocumentRequest

    func execute(_ transport: AtlasReaderDocumentTransport) throws -> AtlasReaderDocumentResult {
        let handle = info.identity.owner, count = AtlasReaderDocumentLimits.pageLines
        let json: String?
        switch request {
        case .prepare(let width): json = transport.prepare(handle, generation, width)
        case .window(let first): json = transport.window(handle, generation, first, count)
        case .headings(let first): json = transport.headings(handle, generation, first, count)
        case .heading(let target): json = transport.heading(handle, generation, target.heading.slug, count)
        case .fromSource(let offset): json = transport.fromSource(handle, generation, offset, count)
        case .source(let target):
            json = transport.source(handle, generation, target.rendered.start, target.rendered.end, AtlasReaderDocumentLimits.context)
        case .copy(let target, let mode):
            json = transport.copy(handle, generation, target.rendered.start, target.rendered.end, mode.rawValue)
        }
        return try decode(json)
    }

    func decode(_ json: String?) throws -> AtlasReaderDocumentResult {
        let data = try AtlasReaderWire.admit(json, handle: info.identity.owner)
        do {
            let wire = try JSONDecoder().decode(AtlasReaderDocumentWire.self, from: data)
            guard let header = wire.header else { throw AtlasReaderError.invalidResponse }
            let identity = try header.identity(handle: info.identity.owner)
            guard identity.matches(info.identity), header.command == request.command else {
                throw AtlasReaderError.invalidResponse
            }
            switch request {
            case .copy(let target, let mode): return .copy(try wire.copy(target, mode: mode))
            case .source(let target): return .source(try wire.source(target))
            default: break
            }
            let basis = try wire.inventory(identity: identity, generation: generation)
            if let inventory, !inventory.matches(basis) { throw AtlasReaderError.invalidResponse }
            switch request {
            case .prepare(let width):
                guard basis.width == width else { throw AtlasReaderError.invalidResponse }
                return .prepared(try wire.page(basis, expectedFirst: 0), try wire.headings(basis, first: 0))
            case .window(let first): return .page(try wire.page(basis, expectedFirst: first))
            case .headings(let first): return .headings(try wire.headings(basis, first: first))
            case .heading(let target):
                guard try wire.text("heading_slug").utf8.elementsEqual(target.heading.slug.utf8),
                      try wire.range("heading_original_range") == target.heading.original else {
                    throw AtlasReaderError.invalidResponse
                }
                let page = try wire.page(basis)
                // The upstream heading selects the first row whose rendered end
                // is after its offset. Empty EOF is possible for synthetic flow.
                if let first = page.lines.first {
                    guard first.rendered.end > target.heading.renderedOffset else { throw AtlasReaderError.invalidResponse }
                }
                return .page(page)
            case .fromSource(let offset):
                guard try wire.number("original_anchor") == offset else { throw AtlasReaderError.invalidResponse }
                return .page(try wire.page(basis))
            case .source, .copy: throw AtlasReaderError.invalidResponse
            }
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }
}

/// Admission belongs to the main actor, foreign work to the existing reader
/// lane. Search/outline counters and captures are never borrowed or reset here.
@MainActor final class AtlasReaderDocumentState {
    private var generation: UInt64 = 0
    private var active: UUID?
    private(set) var inventory: AtlasReaderDocumentInventory?
    private(set) var page: AtlasReaderDocumentPage?
    private(set) var headings: AtlasReaderDocumentHeadings?

    func admit(_ request: AtlasReaderDocumentRequest, info: AtlasReaderInfo) throws -> AtlasReaderDocumentJob {
        let next: UInt64
        switch request {
        case .prepare(let width):
            guard (4...512).contains(width) else { throw AtlasReaderError.invalidInput }
            let (value, overflow) = generation.addingReportingOverflow(1)
            guard !overflow else { throw AtlasReaderError.unavailable }
            next = value
        default:
            guard let inventory, inventory.identity.matches(info.identity) else { throw AtlasReaderError.unavailable }
            next = inventory.generation
            switch request {
            case .window(let first):
                guard first <= inventory.totalLines else { throw AtlasReaderError.invalidInput }
            case .headings(let first):
                guard first <= inventory.totalHeadings else { throw AtlasReaderError.invalidInput }
            case .heading(let target):
                guard headings?.target(slug: target.heading.slug) == target else { throw AtlasReaderError.invalidInput }
            case .fromSource(let offset):
                guard offset >= inventory.sourceBase, offset <= info.identity.capturedBytes else { throw AtlasReaderError.invalidInput }
            case .source(let target), .copy(let target, _):
                guard page?.target(at: target.line.id) == target else { throw AtlasReaderError.invalidInput }
            case .prepare: break
            }
        }
        if case .prepare = request {
            generation = next
            // A foreign reflow may succeed before a canceled/invalid response.
            // Retire native authority now; an old visible preview is read-only.
            revoke()
        }
        let job = AtlasReaderDocumentJob(id: UUID(), info: info, generation: next, inventory: inventory, request: request)
        active = job.id
        return job
    }

    func accept(_ result: AtlasReaderDocumentResult, job: AtlasReaderDocumentJob) throws {
        guard active == job.id else { throw AtlasReaderError.canceled }
        if let expected = job.inventory, inventory?.matches(expected) != true { throw AtlasReaderError.canceled }
        switch result {
        case .prepared(let page, let headings): inventory = page.inventory; self.page = page; self.headings = headings
        case .page(let page): self.page = page
        case .headings(let headings): self.headings = headings
        case .source(let source):
            guard page?.target(at: source.target.line.id) == source.target else { throw AtlasReaderError.canceled }
        case .copy(let copy):
            guard page?.target(at: copy.target.line.id) == copy.target else { throw AtlasReaderError.canceled }
        }
        active = nil
    }
    func abandon(_ job: AtlasReaderDocumentJob) { if active == job.id { active = nil } }
    func revoke() { active = nil; inventory = nil; page = nil; headings = nil }
    func close() { revoke(); generation = 0 }
}

/// Keep arrays bounded during decoding, not only after JSON materialization.
/// This wrapper never leaves the worker or becomes presentation state.
private struct AtlasReaderDocumentWire: Decodable {
    struct Key: CodingKey {
        let stringValue: String
        var intValue: Int? { nil }
        init(_ value: String) { stringValue = value }
        init?(stringValue: String) { self.init(stringValue) }
        init?(intValue: Int) { return nil }
    }
    let header: AtlasReaderWire.Header?
    let fields: KeyedDecodingContainer<Key>
    init(from decoder: Decoder) throws {
        header = try .init(from: decoder)
        fields = try decoder.container(keyedBy: Key.self)
    }
    func text(_ key: String) throws -> String { try fields.decode(String.self, forKey: Key(key)) }
    func number(_ key: String) throws -> UInt64 {
        guard let value = AtlasReaderWire.integer(try text(key)) else { throw AtlasReaderError.invalidResponse }
        return value
    }
    func optionalNumber(_ key: String) throws -> UInt64? {
        guard let value = try fields.decodeIfPresent(String.self, forKey: Key(key)) else { return nil }
        guard let number = AtlasReaderWire.integer(value) else { throw AtlasReaderError.invalidResponse }
        return number
    }
    func flag(_ key: String) throws -> Bool { try fields.decode(Bool.self, forKey: Key(key)) }
    func range(_ key: String) throws -> AtlasDocumentByteRange { try fields.decode(AtlasDocumentByteRange.self, forKey: Key(key)) }
    func absent(_ keys: [String]) throws -> Bool {
        for key in keys where fields.contains(Key(key)) {
            if try !fields.decodeNil(forKey: Key(key)) { return false }
        }
        return true
    }
    func array<T: Decodable>(_ key: String) throws -> [T] {
        var container = try fields.nestedUnkeyedContainer(forKey: Key(key))
        if let count = container.count, count > Int(AtlasReaderDocumentLimits.pageLines) { throw AtlasReaderError.invalidResponse }
        var values: [T] = []
        while !container.isAtEnd {
            guard values.count < Int(AtlasReaderDocumentLimits.pageLines) else { throw AtlasReaderError.invalidResponse }
            values.append(try container.decode(T.self))
        }
        return values
    }
    func inventory(identity: AtlasReaderIdentity, generation: UInt64) throws -> AtlasReaderDocumentInventory {
        guard generation > 0, try number("document_generation") == generation,
              identity.encoding == "utf8", identity.capturedBytes <= AtlasReaderDocumentLimits.sourceBytes,
              try flag("document_ready"), try flag("layout_complete"), try !flag("native_shaped"),
              try text("rendering") == "logical-frankenmarkdown-flow",
              try text("source_mapping") == "enclosing-regions-not-glyph-exact" else { throw AtlasReaderError.invalidResponse }
        let width = try number("width_columns"), lines = try number("total_flow_lines")
        let headings = try number("total_headings"), bytes = try number("rendered_utf8_bytes")
        let base = try number("parser_source_base")
        guard (4...512).contains(width), lines <= AtlasReaderDocumentLimits.flowLines,
              headings <= AtlasReaderDocumentLimits.blocks, bytes <= 64 * 1024 * 1024,
              (base == 0 || base == 3), base <= identity.capturedBytes else { throw AtlasReaderError.invalidResponse }
        return .init(identity: identity, generation: generation, width: width, totalLines: lines,
            totalHeadings: headings, renderedBytes: bytes, sourceBase: base)
    }
    func next(_ key: String, first: UInt64, count: Int, total: UInt64) throws -> UInt64? {
        guard first <= total, UInt64(count) == min(AtlasReaderDocumentLimits.pageLines, total - first) else {
            throw AtlasReaderError.invalidResponse
        }
        let next = try optionalNumber(key), end = first + UInt64(count)
        guard next == (end < total ? end : nil) else { throw AtlasReaderError.invalidResponse }
        return next
    }
    func page(_ inventory: AtlasReaderDocumentInventory, expectedFirst: UInt64? = nil) throws -> AtlasReaderDocumentPage {
        struct Row: Decodable {
            let flow_line, text: String
            let rendered_utf8_range: AtlasDocumentByteRange
            let enclosing_original_range: AtlasDocumentByteRange?
        }
        let first = try number("first_flow_line")
        if let expectedFirst, first != expectedFirst { throw AtlasReaderError.invalidResponse }
        let rows: [Row] = try array("flow_lines")
        let next = try next("next_flow_line", first: first, count: rows.count, total: inventory.totalLines)
        let whole = try flag("whole_document_visible")
        guard whole == (first == 0 && first + UInt64(rows.count) == inventory.totalLines) else { throw AtlasReaderError.invalidResponse }
        var previousEnd: UInt64 = 0, textBytes: UInt64 = 0
        let lines = try rows.enumerated().map { index, row -> AtlasReaderDocumentLine in
            let range = row.rendered_utf8_range, count = UInt64(row.text.utf8.count)
            textBytes += count
            guard AtlasReaderWire.integer(row.flow_line) == first + UInt64(index),
                  range.valid(upTo: inventory.renderedBytes), range.start >= previousEnd,
                  count <= range.length, textBytes <= AtlasReaderDocumentLimits.pageTextBytes else {
                throw AtlasReaderError.invalidResponse
            }
            previousEnd = range.end
            if let original = row.enclosing_original_range {
                guard original.valid(upTo: inventory.identity.capturedBytes, nonempty: true), original.start >= inventory.sourceBase else {
                    throw AtlasReaderError.invalidResponse
                }
            }
            return .init(id: first + UInt64(index), text: row.text, rendered: range, enclosingOriginal: row.enclosing_original_range)
        }
        return .init(id: UUID(), inventory: inventory, first: first, lines: lines, next: next, wholeDocumentVisible: whole)
    }
    func headings(_ inventory: AtlasReaderDocumentInventory, first: UInt64) throws -> AtlasReaderDocumentHeadings {
        struct Row: Decodable {
            let slug, title, rendered_utf8_offset: String
            let original_range: AtlasDocumentByteRange
        }
        let rows: [Row] = try array("headings")
        let next = try next("next_heading", first: first, count: rows.count, total: inventory.totalHeadings)
        var slugs = Set<Data>()
        let headings = try rows.map { row -> AtlasReaderDocumentHeading in
            guard !row.slug.isEmpty, row.slug.utf8.count <= 4096, !row.slug.utf8.contains(0),
                  row.title.utf8.count <= Int(AtlasReaderDocumentLimits.sourceBytes),
                  slugs.insert(Data(row.slug.utf8)).inserted,
                  let offset = AtlasReaderWire.integer(row.rendered_utf8_offset), offset <= inventory.renderedBytes,
                  row.original_range.valid(upTo: inventory.identity.capturedBytes, nonempty: true),
                  row.original_range.start >= inventory.sourceBase else { throw AtlasReaderError.invalidResponse }
            return .init(slug: row.slug, title: row.title, renderedOffset: offset, original: row.original_range)
        }
        return .init(id: UUID(), inventory: inventory, first: first, rows: headings, next: next)
    }
    func selection(_ target: AtlasReaderDocumentTarget) throws {
        guard try number("document_generation") == target.inventory.generation,
              try text("selection_namespace") == "document",
              try text("source_mapping") == "enclosing-regions-not-glyph-exact",
              try range("rendered_utf8_range") == target.rendered,
              try absent(["query_generation", "outline_generation", "symbol_id", "declaration_line", "semantic_resolution"]) else {
            throw AtlasReaderError.invalidResponse
        }
    }
    func copy(_ target: AtlasReaderDocumentTarget, mode: AtlasReaderDocumentCopyMode) throws -> AtlasReaderDocumentCopy {
        try selection(target)
        let original = try range("enclosing_original_range")
        guard original.valid(upTo: target.inventory.identity.capturedBytes, nonempty: true),
              original.start >= target.inventory.sourceBase, original.length <= AtlasReaderLimits.maximumPageBytes else {
            throw AtlasReaderError.invalidResponse
        }
        let value: String
        switch mode {
        case .renderedText:
            value = try text("text")
            guard try text("copy_domain") == "rendered-text-utf8", try absent(["original_hex"]),
                  value.utf8.elementsEqual(target.line.text.utf8) else { throw AtlasReaderError.invalidResponse }
        case .enclosingMarkdown:
            value = try text("original_hex")
            guard try text("copy_domain") == "enclosing-original-markdown", try absent(["text"]),
                  AtlasReaderWire.validHex(value, bytes: original.length) else { throw AtlasReaderError.invalidResponse }
        }
        return .init(target: target, enclosingOriginal: original, mode: mode, value: value)
    }
    func source(_ target: AtlasReaderDocumentTarget) throws -> AtlasReaderDocumentSource {
        // This nested value is a document selection, not a standalone response.
        let selected = try fields.decode(Selection.self, forKey: Key("selection"))
        try selected.fields.selection(target)
        let raw = try selected.fields.range("original_range"), utf8 = try selected.fields.range("window_utf8_range")
        let hex = try selected.fields.text("original_hex")
        let visible = try range("visible_range"), requested = try range("requested_original_range")
        let sourceLength = target.inventory.identity.capturedBytes
        let text = try text("text"), pageHex = try self.text("original_hex")
        let padding = AtlasReaderDocumentLimits.context + 4
        guard raw.valid(upTo: sourceLength, nonempty: true), raw.start >= target.inventory.sourceBase,
              visible.valid(upTo: sourceLength), requested.valid(upTo: sourceLength),
              requested.start == raw.start - min(padding, raw.start),
              requested.end == raw.end + min(padding, sourceLength - raw.end),
              visible.start <= raw.start, visible.end >= raw.end,
              visible.start >= requested.start - min(8, requested.start),
              visible.start <= min(sourceLength, requested.start + 8),
              visible.end >= requested.end - min(8, requested.end),
              visible.end <= min(sourceLength, requested.end + 8),
              try !flag("range_limited"), try flag("boundaries_adjusted") == (visible != requested),
              try !flag("has_replacements"), try absent(["first_physical_line"]),
              try self.text("text_kind") == "logical-captured-text-not-shaped",
              UInt64(text.utf8.count) <= AtlasReaderDocumentLimits.pageTextBytes,
              utf8.valid(upTo: UInt64(text.utf8.count), nonempty: true),
              AtlasReaderWire.validHex(pageHex, bytes: visible.length),
              AtlasReaderWire.validHex(hex, bytes: raw.length) else { throw AtlasReaderError.invalidResponse }
        if let next = try optionalNumber("next_offset"), next != visible.end { throw AtlasReaderError.invalidResponse }
        let pageBytes = Array(pageHex.utf8), decoded = Array(text.utf8)
        guard pageBytes[Int((raw.start - visible.start) * 2)..<Int((raw.end - visible.start) * 2)].elementsEqual(hex.utf8),
              String(bytes: decoded[Int(utf8.start)..<Int(utf8.end)], encoding: .utf8) != nil,
              decoded[Int(utf8.start)..<Int(utf8.end)].elementsEqual(Self.hexBytes(hex)) else {
            throw AtlasReaderError.invalidResponse
        }
        return .init(target: target, visible: visible, text: text, originalHex: pageHex,
            enclosingOriginal: raw, selectionUTF8: utf8, selectedHex: hex)
    }
    private static func hexBytes(_ validatedHex: String) -> [UInt8] {
        let bytes = Array(validatedHex.utf8)
        func nibble(_ b: UInt8) -> UInt8 { b <= 57 ? b - 48 : b - 87 }
        return stride(from: 0, to: bytes.count, by: 2).map { nibble(bytes[$0]) * 16 + nibble(bytes[$0 + 1]) }
    }
    private struct Selection: Decodable {
        let fields: AtlasReaderDocumentWire
        init(from decoder: Decoder) throws {
            // The nested object has no source header. Reuse only keyed field
            // validation, with the outer response's already-verified header.
            let c = try decoder.container(keyedBy: Key.self)
            fields = AtlasReaderDocumentWire(fields: c)
        }
    }
    private init(fields: KeyedDecodingContainer<Key>) {
        // Nested selections have no identity header. Full responses require
        // that header in init(from:); only selection fields use this initializer.
        self.fields = fields
        header = nil
    }
}
