import Foundation

/// Optional C boundary. The shared Rust extractor owns language recognition,
/// filtering and source mapping; this host owns only bounded presentation pages.
struct AtlasReaderOutlineTransport: Sendable {
    let prepare: @Sendable (UInt64, UInt64, String?, UInt64) -> String?
    let symbols: @Sendable (UInt64, UInt64, String, UInt8, UInt64, UInt64) -> String?
    let symbol: @Sendable (UInt64, UInt64, UInt64, UInt64) -> String?
}

enum AtlasOutlineLanguage: String, CaseIterable, Identifiable, Sendable {
    case automatic = "", rust, python, javascript, typescript, go, cpp
    var id: String { rawValue }
    var label: String { self == .automatic ? "Infer from filename" : rawValue }
    var engineName: String? { self == .automatic ? nil : rawValue }
}

enum AtlasOutlineNameMode: String, CaseIterable, Identifiable, Sendable {
    case exact, prefix, contains
    var id: String { rawValue }
    var code: UInt8 { switch self { case .exact: 0; case .prefix: 1; case .contains: 2 } }
}

struct AtlasOutlinePageRequest: Equatable, Sendable {
    let needle: String
    let mode: AtlasOutlineNameMode
    let start: UInt64
    let limit: UInt64
    static let initial = Self(needle: "", mode: .exact, start: 0, limit: 64)

    func validate() throws {
        guard needle.utf8.count <= 256, !needle.utf8.contains(0),
              start <= AtlasOutlinePage.maxItems, (1...128).contains(limit) else {
            throw AtlasReaderError.invalidInput
        }
    }
    static func == (a: Self, b: Self) -> Bool {
        a.needle.utf8.elementsEqual(b.needle.utf8) && a.mode == b.mode
            && a.start == b.start && a.limit == b.limit
    }
}

struct AtlasOutlineRange: Equatable, Sendable {
    let start: UInt64
    let end: UInt64
    var length: UInt64 { end - start }
    func contains(_ other: Self) -> Bool { start <= other.start && other.end <= end }
}

struct AtlasOutlineSymbol: Equatable, Identifiable, Sendable {
    let id: UInt64
    let parent: UInt64?
    let depth: UInt64
    let name: String
    let kind: String
    let line: UInt64
    let evidence: AtlasOutlineRange
    let nameRange: AtlasOutlineRange?
    var selectedRange: AtlasOutlineRange { nameRange ?? evidence }
    static func == (a: Self, b: Self) -> Bool {
        a.id == b.id && a.parent == b.parent && a.depth == b.depth && a.line == b.line
            && a.name.utf8.elementsEqual(b.name.utf8) && a.kind == b.kind
            && a.evidence == b.evidence && a.nameRange == b.nameRange
    }
}

/// A filtered position is never a symbol ID. A page-local token also prevents
/// stale UI rows from lending authority to a later filtered presentation.
struct AtlasOutlineTarget: Equatable, Sendable {
    let identity: AtlasReaderIdentity
    let generation: UInt64
    let pageID: UUID
    let symbol: AtlasOutlineSymbol
    static let contextBytes: UInt64 = 2048
    static func == (a: Self, b: Self) -> Bool {
        a.identity.matches(b.identity) && a.generation == b.generation
            && a.pageID == b.pageID && a.symbol == b.symbol
    }
}

struct AtlasReaderSymbolSelection: Sendable {
    let target: AtlasOutlineTarget
    let utf8Start: UInt64
    let utf8End: UInt64
    let originalHex: String
}

struct AtlasOutlineInventory: Sendable {
    let identity: AtlasReaderIdentity
    let generation: UInt64
    let language: String
    let retained: UInt64
    let limited: Bool
    let noDeclarations: Bool
    func matches(_ other: Self) -> Bool {
        identity.matches(other.identity) && generation == other.generation
            && language == other.language && retained == other.retained
            && limited == other.limited && noDeclarations == other.noDeclarations
    }
}

struct AtlasOutlinePage: Sendable {
    static let maxItems: UInt64 = 4096
    static let sourceLimit: UInt64 = 64 * 1024
    let id: UUID
    let inventory: AtlasOutlineInventory
    let request: AtlasOutlinePageRequest
    let matched: UInt64
    let rows: [AtlasOutlineSymbol]
    let nextOffset: UInt64?

    func target(id: UInt64) -> AtlasOutlineTarget? {
        rows.first(where: { $0.id == id }).map {
            AtlasOutlineTarget(identity: inventory.identity, generation: inventory.generation,
                pageID: self.id, symbol: $0)
        }
    }
    var summary: String {
        if inventory.noDeclarations {
            return "No declarations recognized. This is not proof that the file has no symbols."
        }
        let coverage = inventory.limited ? "Partial outline; extraction reached its item limit." : "Heuristic outline, not compiler resolution."
        return "\(matched) matching retained candidates / \(inventory.retained). \(coverage)"
    }

    static func decode(_ json: String?, info: AtlasReaderInfo, generation: UInt64,
        request: AtlasOutlinePageRequest, preparing: Bool = false,
        language: AtlasOutlineLanguage = .automatic, inventory: AtlasOutlineInventory? = nil) throws -> Self {
        try request.validate()
        let data = try AtlasReaderWire.admit(json, handle: info.identity.owner)
        do {
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            let identity = try wire.header.identity(handle: info.identity.owner)
            guard identity.matches(info.identity), identity.capturedBytes <= sourceLimit,
                  generation > 0, AtlasReaderWire.integer(wire.outline_generation) == generation,
                  wire.header.command == (preparing ? "outline" : "symbols"),
                  !preparing || request == .initial,
                  AtlasOutlineLanguage(rawValue: wire.language)?.engineName != nil,
                  language == .automatic || wire.language == language.rawValue,
                  wire.evidence_level == "heuristic-outline-candidate", !wire.semantic_complete,
                  wire.count_basis == "retained-candidates", wire.name_case == "sensitive",
                  wire.needle.utf8.elementsEqual(request.needle.utf8), wire.name_mode == request.mode.rawValue,
                  let retained = AtlasReaderWire.integer(wire.retained_symbols), retained <= maxItems,
                  let matched = AtlasReaderWire.integer(wire.matched_symbols), matched <= retained,
                  request.start <= matched, !request.needle.isEmpty || matched == retained,
                  !wire.no_recognized_declarations || retained == 0 else { throw AtlasReaderError.invalidResponse }
            let expectedCount = min(request.limit, matched - request.start)
            guard UInt64(wire.symbols.count) == expectedCount else { throw AtlasReaderError.invalidResponse }
            let end = request.start + expectedCount
            let next: UInt64?
            if let text = wire.next_offset {
                guard let value = AtlasReaderWire.integer(text), value == end, end < matched else {
                    throw AtlasReaderError.invalidResponse
                }
                next = value
            } else {
                guard end == matched else { throw AtlasReaderError.invalidResponse }
                next = nil
            }
            let basis = AtlasOutlineInventory(identity: identity, generation: generation,
                language: wire.language, retained: retained, limited: wire.output_limited,
                noDeclarations: wire.no_recognized_declarations)
            if let inventory, !inventory.matches(basis) { throw AtlasReaderError.invalidResponse }
            var previous: UInt64 = 0
            let rows = try wire.symbols.map { row -> AtlasOutlineSymbol in
                guard let id = AtlasReaderWire.integer(row.symbol_id), id > previous, id <= retained,
                      let depth = AtlasReaderWire.integer(row.depth), depth <= maxItems,
                      let line = AtlasReaderWire.integer(row.declaration_line), (1...4096).contains(line),
                      !row.name.isEmpty, row.name.utf8.count <= Int(sourceLimit),
                      !row.kind.isEmpty, row.kind.utf8.count <= 128 else { throw AtlasReaderError.invalidResponse }
                previous = id
                let parent = try row.parent_id.map { value -> UInt64 in
                    guard let parent = AtlasReaderWire.integer(value), parent > 0, parent < id else {
                        throw AtlasReaderError.invalidResponse
                    }
                    return parent
                }
                guard (parent == nil) == (depth == 0) else { throw AtlasReaderError.invalidResponse }
                let evidence = try row.evidence_range.admit(length: identity.capturedBytes)
                let name = try row.name_range.map { try $0.admit(length: identity.capturedBytes) }
                if let name, !evidence.contains(name) { throw AtlasReaderError.invalidResponse }
                return AtlasOutlineSymbol(id: id, parent: parent, depth: depth, name: row.name,
                    kind: row.kind, line: line, evidence: evidence, nameRange: name)
            }
            return Self(id: UUID(), inventory: basis, request: request, matched: matched, rows: rows, nextOffset: next)
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }

    private struct Wire: Decodable {
        let header: AtlasReaderWire.Header
        let outline_generation, language, evidence_level, retained_symbols, matched_symbols: String
        let count_basis, name_case, needle, name_mode: String
        let semantic_complete, output_limited, no_recognized_declarations: Bool
        let symbols: [Row]
        let next_offset: String?
        struct Row: Decodable {
            let symbol_id, depth, name, kind, declaration_line: String
            let parent_id: String?
            let evidence_range: AtlasReaderWire.Range
            let name_range: AtlasReaderWire.Range?
        }
        enum CodingKeys: CodingKey {
            case outline_generation, language, evidence_level, retained_symbols, matched_symbols,
                 count_basis, name_case, needle, name_mode, semantic_complete, output_limited,
                 no_recognized_declarations, symbols, next_offset
        }
        init(from decoder: Decoder) throws {
            header = try .init(from: decoder)
            let c = try decoder.container(keyedBy: CodingKeys.self)
            outline_generation = try c.decode(String.self, forKey: .outline_generation)
            language = try c.decode(String.self, forKey: .language)
            evidence_level = try c.decode(String.self, forKey: .evidence_level)
            retained_symbols = try c.decode(String.self, forKey: .retained_symbols)
            matched_symbols = try c.decode(String.self, forKey: .matched_symbols)
            count_basis = try c.decode(String.self, forKey: .count_basis)
            name_case = try c.decode(String.self, forKey: .name_case)
            needle = try c.decode(String.self, forKey: .needle)
            name_mode = try c.decode(String.self, forKey: .name_mode)
            semantic_complete = try c.decode(Bool.self, forKey: .semantic_complete)
            output_limited = try c.decode(Bool.self, forKey: .output_limited)
            no_recognized_declarations = try c.decode(Bool.self, forKey: .no_recognized_declarations)
            next_offset = try c.decodeIfPresent(String.self, forKey: .next_offset)
            var input = try c.nestedUnkeyedContainer(forKey: .symbols)
            if let count = input.count, count > 128 { throw AtlasReaderError.invalidResponse }
            var rows: [Row] = []
            while !input.isAtEnd {
                guard rows.count < 128 else { throw AtlasReaderError.invalidResponse }
                rows.append(try input.decode(Row.self))
            }
            symbols = rows
        }
    }
}

extension AtlasReaderWire.Range {
    func admit(length: UInt64) throws -> AtlasOutlineRange {
        let range = try values()
        guard range.start < range.end, range.end <= length else { throw AtlasReaderError.invalidResponse }
        return AtlasOutlineRange(start: range.start, end: range.end)
    }
}
