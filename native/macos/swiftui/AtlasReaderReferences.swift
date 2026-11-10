import Foundation

/// Optional worker-side reader capability. The shared Rust engine owns token
/// boundaries and original-byte mapping; Swift never extracts identifiers.
struct AtlasReaderReferencesTransport: Sendable {
    let prepare: @Sendable (UInt64, UInt64, String, UInt64) -> String?
    let page: @Sendable (UInt64, UInt64, UInt64, UInt64) -> String?
    let reference: @Sendable (UInt64, UInt64, UInt64, UInt64) -> String?
}

struct AtlasReferencePageRequest: Equatable, Sendable {
    let start: UInt64
    let limit: UInt64
    static let initial = Self(start: 0, limit: 64)

    func validate() throws {
        guard start <= AtlasReferencePage.maxItems, (1...128).contains(limit) else {
            throw AtlasReaderError.invalidInput
        }
    }
}

struct AtlasReferenceOccurrence: Equatable, Identifiable, Sendable {
    let id: UInt64
    let range: AtlasOutlineRange
    /// The engine counts LF boundaries, including CRLF. This is not a native
    /// wrapped row or a claim that CR-only lines use the same numbering.
    let line: UInt64
}

/// IDs are local to one accepted source/name inventory. A page UUID prevents a
/// delayed row event from acquiring authority through a later page with equal
/// numeric occurrence IDs. Exact UTF-8 spelling is preserved independently of
/// Swift's canonical-equivalence String comparison.
struct AtlasReferenceTarget: Equatable, Sendable {
    let identity: AtlasReaderIdentity
    let generation: UInt64
    let pageID: UUID
    let name: String
    let occurrence: AtlasReferenceOccurrence
    static let contextBytes: UInt64 = 2048

    static func == (a: Self, b: Self) -> Bool {
        a.identity.matches(b.identity) && a.generation == b.generation
            && a.pageID == b.pageID && a.name.utf8.elementsEqual(b.name.utf8)
            && a.occurrence == b.occurrence
    }
}

struct AtlasReaderReferenceSelection: Sendable {
    let target: AtlasReferenceTarget
    let utf8Start: UInt64
    let utf8End: UInt64
    let originalHex: String
}

struct AtlasReferenceInventory: Sendable {
    let identity: AtlasReaderIdentity
    let generation: UInt64
    let name: String
    let maxItems: UInt64
    let retained: UInt64
    let matchesCounted: UInt64
    let limited: Bool

    func matches(_ other: Self) -> Bool {
        identity.matches(other.identity) && generation == other.generation
            && name.utf8.elementsEqual(other.name.utf8) && maxItems == other.maxItems
            && retained == other.retained && matchesCounted == other.matchesCounted
            && limited == other.limited
    }
}

/// A bounded page of whole-token text occurrences, not compiler-resolved
/// references. Completeness covers only that declared policy in one capture.
struct AtlasReferencePage: Sendable {
    static let maxItems: UInt64 = 4096
    static let sourceLimit: UInt64 = 512 * 1024
    static let nameBytes = 1024
    let id: UUID
    let inventory: AtlasReferenceInventory
    let request: AtlasReferencePageRequest
    let rows: [AtlasReferenceOccurrence]
    let nextOffset: UInt64?

    /// Only marshaling bounds are checked here. Identifier policy belongs to
    /// the shared engine, including its deliberately conservative Unicode rule.
    static func validateName(_ name: String) throws {
        guard !name.isEmpty, name.utf8.count <= nameBytes, !name.utf8.contains(0) else {
            throw AtlasReaderError.invalidInput
        }
    }

    func target(id: UInt64) -> AtlasReferenceTarget? {
        rows.first(where: { $0.id == id }).map {
            AtlasReferenceTarget(identity: inventory.identity, generation: inventory.generation,
                pageID: self.id, name: inventory.name, occurrence: $0)
        }
    }

    var summary: String {
        let counts = inventory.limited
            ? "At least \(inventory.matchesCounted) whole-token occurrences; \(inventory.retained) retained. More may exist."
            : "\(inventory.retained) whole-token occurrences in this retained source."
        return counts + " Includes comments and string literals; not compiler references."
    }

    static func decode(_ json: String?, info: AtlasReaderInfo, generation: UInt64,
        name: String, request: AtlasReferencePageRequest, preparing: Bool = false,
        inventory: AtlasReferenceInventory? = nil, maxItems: UInt64 = AtlasReferencePage.maxItems) throws -> Self {
        try validateName(name)
        try request.validate()
        guard generation > 0, maxItems <= Self.maxItems else { throw AtlasReaderError.invalidInput }
        let data = try AtlasReaderWire.admit(json, handle: info.identity.owner)
        do {
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            let identity = try wire.header.identity(handle: info.identity.owner)
            guard identity.matches(info.identity), identity.capturedBytes <= sourceLimit,
                  ["utf8", "utf16le", "utf16be"].contains(identity.encoding),
                  wire.header.command == (preparing ? "references" : "reference-page"),
                  !preparing || request == .initial,
                  AtlasReaderWire.integer(wire.reference_generation) == generation,
                  wire.name.utf8.elementsEqual(name.utf8), wire.name_case == "sensitive",
                  wire.selection_namespace == "reference", wire.evidence_level == "whole-token-text-candidate",
                  wire.includes_comments_and_literals, !wire.compiler_resolved, !wire.semantic_complete,
                  wire.line_semantics == "LF-counted-source-lines",
                  AtlasReaderWire.integer(wire.max_source_bytes) == sourceLimit,
                  AtlasReaderWire.integer(wire.max_items) == maxItems,
                  let retained = AtlasReaderWire.integer(wire.retained_references), retained <= maxItems,
                  let counted = AtlasReaderWire.integer(wire.matches_counted),
                  wire.search_complete == !wire.output_limited, wire.count_complete == wire.search_complete,
                  request.start <= retained else { throw AtlasReaderError.invalidResponse }
            if wire.output_limited {
                guard retained == maxItems, counted == retained + 1 else { throw AtlasReaderError.invalidResponse }
            } else {
                guard counted == retained else { throw AtlasReaderError.invalidResponse }
            }
            if identity.encoding != "utf8" {
                guard identity.capturedBytes >= 2, identity.capturedBytes % 2 == 0 else {
                    throw AtlasReaderError.invalidResponse
                }
            }
            let expectedCount = min(request.limit, retained - request.start)
            guard UInt64(wire.references.count) == expectedCount else { throw AtlasReaderError.invalidResponse }
            let end = request.start + expectedCount
            let next: UInt64?
            if let text = wire.next_offset {
                guard let value = AtlasReaderWire.integer(text), value == end, end < retained else {
                    throw AtlasReaderError.invalidResponse
                }
                next = value
            } else {
                guard end == retained else { throw AtlasReaderError.invalidResponse }
                next = nil
            }
            let basis = AtlasReferenceInventory(identity: identity, generation: generation, name: name,
                maxItems: maxItems, retained: retained, matchesCounted: counted, limited: wire.output_limited)
            if let inventory, !inventory.matches(basis) { throw AtlasReaderError.invalidResponse }
            // Every exact occurrence has the same encoded byte width. Standard
            // String views provide that width without duplicating token policy
            // or the engine's original-to-decoded offset mapping.
            let tokenBytes = identity.encoding == "utf8"
                ? UInt64(name.utf8.count) : UInt64(name.utf16.count) * 2
            var previousEnd: UInt64 = 0
            var previousLine: UInt64 = 1
            let rows = try wire.references.enumerated().map { index, row -> AtlasReferenceOccurrence in
                guard let id = AtlasReaderWire.integer(row.reference_id), id == request.start + UInt64(index) + 1,
                      let line = AtlasReaderWire.integer(row.line), line >= previousLine,
                      line <= identity.capturedBytes + 1 else { throw AtlasReaderError.invalidResponse }
                let range = try row.original_range.admit(length: identity.capturedBytes)
                guard range.start >= previousEnd, range.length == tokenBytes else {
                    throw AtlasReaderError.invalidResponse
                }
                if identity.encoding != "utf8" {
                    guard range.start >= 2, range.start % 2 == 0, range.end % 2 == 0 else {
                        throw AtlasReaderError.invalidResponse
                    }
                }
                previousEnd = range.end; previousLine = line
                return AtlasReferenceOccurrence(id: id, range: range, line: line)
            }
            return Self(id: UUID(), inventory: basis, request: request, rows: rows, nextOffset: next)
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }

    private struct Wire: Decodable {
        let header: AtlasReaderWire.Header
        let reference_generation, name, name_case, selection_namespace, evidence_level: String
        let line_semantics, max_source_bytes, max_items, retained_references, matches_counted: String
        let includes_comments_and_literals, compiler_resolved, semantic_complete: Bool
        let search_complete, count_complete, output_limited: Bool
        let references: [Row]
        let next_offset: String?

        struct Row: Decodable {
            let reference_id, line: String
            let original_range: AtlasReaderWire.Range
        }
        enum CodingKeys: CodingKey {
            case reference_generation, name, name_case, selection_namespace, evidence_level,
                 line_semantics, max_source_bytes, max_items, retained_references, matches_counted,
                 includes_comments_and_literals, compiler_resolved, semantic_complete,
                 search_complete, count_complete, output_limited, references, next_offset
        }
        init(from decoder: Decoder) throws {
            header = try .init(from: decoder)
            let c = try decoder.container(keyedBy: CodingKeys.self)
            reference_generation = try c.decode(String.self, forKey: .reference_generation)
            name = try c.decode(String.self, forKey: .name)
            name_case = try c.decode(String.self, forKey: .name_case)
            selection_namespace = try c.decode(String.self, forKey: .selection_namespace)
            evidence_level = try c.decode(String.self, forKey: .evidence_level)
            line_semantics = try c.decode(String.self, forKey: .line_semantics)
            max_source_bytes = try c.decode(String.self, forKey: .max_source_bytes)
            max_items = try c.decode(String.self, forKey: .max_items)
            retained_references = try c.decode(String.self, forKey: .retained_references)
            matches_counted = try c.decode(String.self, forKey: .matches_counted)
            includes_comments_and_literals = try c.decode(Bool.self, forKey: .includes_comments_and_literals)
            compiler_resolved = try c.decode(Bool.self, forKey: .compiler_resolved)
            semantic_complete = try c.decode(Bool.self, forKey: .semantic_complete)
            search_complete = try c.decode(Bool.self, forKey: .search_complete)
            count_complete = try c.decode(Bool.self, forKey: .count_complete)
            output_limited = try c.decode(Bool.self, forKey: .output_limited)
            guard c.contains(.next_offset) else { throw AtlasReaderError.invalidResponse }
            next_offset = try c.decodeIfPresent(String.self, forKey: .next_offset)
            var input = try c.nestedUnkeyedContainer(forKey: .references)
            if let count = input.count, count > 128 { throw AtlasReaderError.invalidResponse }
            var rows: [Row] = []
            while !input.isAtEnd {
                guard rows.count < 128 else { throw AtlasReaderError.invalidResponse }
                rows.append(try input.decode(Row.self))
            }
            references = rows
        }
    }
}
