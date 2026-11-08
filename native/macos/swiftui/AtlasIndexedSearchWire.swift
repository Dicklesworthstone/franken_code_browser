import Foundation

/// Bounded typed marshaling, not an index or source decoder. Keep the decoder
/// container worker-local; only validated values become report/capture state.
struct AtlasIndexWire: Decodable {
    struct Key: CodingKey {
        let stringValue: String
        var intValue: Int? { nil }
        init(_ text: String) { stringValue = text }
        init?(stringValue: String) { self.init(stringValue) }
        init?(intValue: Int) { return nil }
    }
    let fields: KeyedDecodingContainer<Key>
    init(from decoder: Decoder) throws { fields = try decoder.container(keyedBy: Key.self) }
    static func decode(_ json: String?) throws -> Self {
        guard let json else { throw AtlasSearchError.unavailable }
        guard json.utf8.count <= 8 * 1024 * 1024, let data = json.data(using: .utf8) else { throw invalid() }
        do { return try JSONDecoder().decode(Self.self, from: data) }
        catch { throw invalid() }
    }
    static func invalid() -> AtlasSearchError { .invalidResponse }
    static func integer(_ text: String) throws -> UInt64 {
        guard let n = UInt64(text), String(n) == text else { throw invalid() }
        return n
    }
    func text(_ key: String) throws -> String { try fields.decode(String.self, forKey: Key(key)) }
    func flag(_ key: String) throws -> Bool { try fields.decode(Bool.self, forKey: Key(key)) }
    func number(_ key: String) throws -> UInt64 { try Self.integer(text(key)) }
    func count(_ key: String, upTo maximum: Int) throws -> Int {
        let n = try number(key)
        guard n <= UInt64(maximum) else { throw Self.invalid() }
        return Int(n)
    }
    func optionalNumber(_ key: String) throws -> UInt64? {
        try fields.decodeIfPresent(String.self, forKey: Key(key)).map(Self.integer)
    }
    func optionalText(_ key: String) throws -> String? { try fields.decodeIfPresent(String.self, forKey: Key(key)) }
    func optionalFlag(_ key: String) throws -> Bool? { try fields.decodeIfPresent(Bool.self, forKey: Key(key)) }
    func object(_ key: String) throws -> Self { try fields.decode(Self.self, forKey: Key(key)) }
    func absent(_ keys: [String]) throws -> Bool {
        for key in keys where fields.contains(Key(key)) {
            if try !fields.decodeNil(forKey: Key(key)) { return false }
        }
        return true
    }
    func rows<T: Decodable>(_ key: String, limit: Int) throws -> [T] {
        var rows = try fields.nestedUnkeyedContainer(forKey: Key(key))
        if let count = rows.count, count > limit { throw Self.invalid() }
        var result: [T] = []
        while !rows.isAtEnd {
            guard result.count < limit else { throw Self.invalid() }
            result.append(try rows.decode(T.self))
        }
        return result
    }
    func header(schema: String, command: String, owner: UInt64) throws {
        guard try text("schema") == schema, try text("status") == "ok", try text("command") == command,
              try number("owner") == owner, owner != 0 else { throw Self.invalid() }
    }
}

struct AtlasIndexedBasis: Sendable, Equatable {
    let owner, manifest, layout, index, captureManifest: UInt64
    let files, capturedFiles, unavailable, pending: Int
    let sourceBytes: UInt64
    let discoveryComplete: Bool

    func validate(_ wire: AtlasIndexWire, command: String) throws {
        try wire.header(schema: "fcb.atlas-search/1", command: command, owner: owner)
        guard try wire.number("source_manifest") == manifest, try wire.number("layout_revision") == layout,
              try wire.number("index_generation") == index, try wire.number("capture_manifest") == captureManifest,
              try wire.text("scope") == "all", try wire.number("workspace_files") == UInt64(files) else {
            throw AtlasIndexWire.invalid()
        }
    }
}

struct AtlasIndexedHit: Decodable, Sendable, Equatable {
    var hit: SearchHit
    let file, revision: UInt64
    let pathHex: String
    let rawPathBytes: [UInt8]

    init(from decoder: Decoder) throws {
        let wire = try AtlasIndexWire(from: decoder)
        let ordinal = try wire.number("hit_id")
        file = try wire.number("file_id"); revision = try wire.number("source_revision")
        let range = try wire.object("original_range"), path = try wire.object("path")
        let start = try range.number("start"), end = try range.number("end")
        let bytes = try wire.number("capture_byte_length"), digest = try wire.text("capture_sha256")
        let display = try path.text("display")
        let actualText = try wire.optionalText("matched_text")
        pathHex = try path.text("hex")
        guard ordinal > 0, ordinal <= 1000, file > 0, revision > 0, start < end, end <= bytes,
              bytes <= 1024 * 1024, digest.utf8.count == 64,
              digest.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
              try path.text("encoding") == "unix-bytes", display.utf8.count <= 65_536,
              !pathHex.isEmpty, pathHex.utf8.count <= 32_768, pathHex.utf8.count.isMultiple(of: 2) else {
            throw AtlasIndexWire.invalid()
        }
        if let actualText, !AtlasSearchText.validActualText(actualText) { throw AtlasIndexWire.invalid() }
        let hex = Array(pathHex.utf8)
        func nibble(_ b: UInt8) throws -> UInt8 {
            switch b { case 48...57: return b - 48; case 97...102: return b - 87; default: throw AtlasIndexWire.invalid() }
        }
        var raw: [UInt8] = []; raw.reserveCapacity(hex.count / 2)
        for i in stride(from: 0, to: hex.count, by: 2) { raw.append(try nibble(hex[i]) * 16 + nibble(hex[i + 1])) }
        guard !raw.contains(0), raw.first != 47,
              !raw.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                  $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
              }) else { throw AtlasIndexWire.invalid() }
        rawPathBytes = raw
        let excerpt = try wire.fields.decodeIfPresent(AtlasSearchExcerptWire.self,
            forKey: AtlasIndexWire.Key("excerpt"))?.validated(hitStart: start, hitEnd: end,
                capturedBytes: bytes, actualText: actualText)
        hit = SearchHit(id: Int(ordinal - 1), path: display, sourcePath: String(bytes: raw, encoding: .utf8),
            start: start, end: end, captureSHA256: digest, captureByteLength: bytes,
            actualText: actualText, excerpt: excerpt)
    }
    static func == (a: Self, b: Self) -> Bool {
        a.sameWitness(as: b) && a.hit.excerpt == b.hit.excerpt
    }
    private func sameWitness(as other: Self) -> Bool {
        file == other.file && revision == other.revision && pathHex == other.pathHex && hit.id == other.hit.id
            && hit.start == other.hit.start && hit.end == other.hit.end
            && hit.captureSHA256 == other.hit.captureSHA256 && hit.captureByteLength == other.hit.captureByteLength
            && hit.path.utf8.elementsEqual(other.hit.path.utf8)
            && AtlasSearchText.sameActualText(hit.actualText, other.hit.actualText)
    }
    func repeats(_ old: Self) -> Bool {
        sameWitness(as: old) && (hit.excerpt == nil || old.hit.excerpt == nil || hit.excerpt == old.hit.excerpt)
    }
    func matches(_ other: SearchHit) -> Bool {
        hit.id == other.id && hit.start == other.start && hit.end == other.end
            && hit.captureSHA256 == other.captureSHA256 && hit.captureByteLength == other.captureByteLength
            && hit.sourcePath?.utf8.elementsEqual(other.sourcePath?.utf8 ?? "".utf8) == true
            && AtlasSearchText.sameActualText(hit.actualText, other.actualText)
    }
}

struct AtlasIndexedQueryState: Equatable {
    let eligible, captured, captureUnavailable, visibleMissing: Int
    let examined, scanned, unavailable, pending, retained, step, skipped, fallback: Int
    let matches, verificationBytes: UInt64
    let running, complete, truncated: Bool
    let stop: String?
}
struct AtlasIndexedQueryPage {
    let state: AtlasIndexedQueryState
    let hits: [AtlasIndexedHit]
    let next: UInt64?

    init(_ json: String?, basis: AtlasIndexedBasis, generation: UInt64, needle: String,
         scope: AtlasSearchFileScope = .all, pathScope: AtlasSearchPathScope = .workspace,
         caseSensitive: Bool = true, command: String) throws {
        let w = try AtlasIndexWire.decode(json)
        try basis.validate(w, command: command)
        let kind = try w.optionalText("search_scope")
        try pathScope.validateEcho(kind: w.optionalText("scope_path_kind"),
            path: w.optionalText("scope_path"), hex: w.optionalText("scope_path_hex"),
            regionSHA256: w.optionalText("scope_region_sha256"), regionFiles: w.optionalText("scope_region_files"),
            regionPathBytes: w.optionalText("scope_region_path_bytes"))
        let eligible = try scope.eligibleFiles(kind: kind, extensions: w.optionalText("scope_extensions"),
            filenames: w.optionalText("scope_filenames"), eligible: w.optionalText("eligible_files"),
            excluded: w.optionalText("excluded_files"), catalogued: basis.files, pathScoped: !pathScope.isWorkspace)
        let captured, captureUnavailable: Int
        if kind == nil {
            captured = basis.capturedFiles; captureUnavailable = basis.unavailable
        } else {
            captured = try w.count("eligible_captured_files", upTo: min(basis.capturedFiles, eligible))
            captureUnavailable = try w.count("scoped_capture_unavailable_files", upTo: min(basis.unavailable, eligible))
        }
        guard captured + captureUnavailable <= eligible,
              !scope.isAll || !pathScope.isWorkspace || (captured == basis.capturedFiles && captureUnavailable == basis.unavailable) else {
            throw AtlasIndexWire.invalid()
        }
        let examined = try w.count("examined_files", upTo: eligible)
        let scanned = try w.count("scanned_files", upTo: captured)
        let unavailable = try w.count("unavailable_files", upTo: eligible)
        let pending = try w.count("pending_files", upTo: eligible)
        let retained = try w.count("retained_hits", upTo: 1000)
        let step = try w.count("step_count", upTo: 4097)
        let skipped = try w.count("skipped_by_index", upTo: captured)
        let fallback = try w.count("fallback_files", upTo: scanned)
        let matches = try w.number("matches_seen"), verification = try w.number("verification_source_bytes")
        let running = try w.flag("search_in_progress"), complete = try w.flag("search_complete")
        let truncated = try w.flag("truncated"), stop = try w.optionalText("stop_reason")
        let visibleMissing = try pathScope.validateRegionCoverage(catalogued: w.optionalText("scope_region_catalogued_files"),
            missing: w.optionalText("scope_region_missing_files"), eligible: eligible, searchCatalogFiles: basis.files,
            allFileTypes: scope.isAll, complete: complete)
        guard try w.number("query_generation") == generation, generation > basis.index,
              try w.text("needle").utf8.elementsEqual(needle.utf8),
              try w.text("mode") == AtlasSearchText.wireMode(caseSensitive: caseSensitive),
              try (w.optionalFlag("case_sensitive") ?? true) == caseSensitive,
              try w.text("search_strategy") == "retained-ephemeral-index",
              try w.flag("discovery_complete") == basis.discoveryComplete,
              try w.number("catalogued_files") == UInt64(basis.files),
              try w.number("displayed_hits") == UInt64(retained), try w.number("verified_files") == UInt64(scanned),
              try w.number("source_bytes_read") == 0, try w.number("read_calls") == 0,
              scanned + skipped <= captured,
              examined == scanned + skipped + captureUnavailable, pending == eligible - examined,
              unavailable >= captureUnavailable, unavailable <= captureUnavailable + scanned,
              verification <= basis.sourceBytes, matches >= UInt64(retained), running == (stop == nil) else {
            throw AtlasIndexWire.invalid()
        }
        if let stop, !["all-files-examined", "file-limit", "source-byte-limit", "match-limit", "verification-byte-limit"].contains(stop) {
            throw AtlasIndexWire.invalid()
        }
        if complete {
            guard !running, !truncated, basis.discoveryComplete, pending == 0, unavailable == 0,
                  matches == UInt64(retained) else { throw AtlasIndexWire.invalid() }
        }
        hits = try w.rows("hits", limit: command == "page" ? 128 : 64)
        guard hits.allSatisfy({ scope.admitsRawPath($0.rawPathBytes) && pathScope.admitsRawPath($0.rawPathBytes)
            && AtlasSearchText.accepts(actualText: $0.hit.actualText, needle: needle, caseSensitive: caseSensitive)
            && ($0.hit.excerpt?.matchesSourceText($0.hit.actualText ?? needle) != false) }) else {
            throw AtlasIndexWire.invalid()
        }
        next = try w.optionalNumber("next_offset")
        state = AtlasIndexedQueryState(eligible: eligible, captured: captured, captureUnavailable: captureUnavailable,
            visibleMissing: visibleMissing,
            examined: examined, scanned: scanned, unavailable: unavailable,
            pending: pending, retained: retained, step: step, skipped: skipped, fallback: fallback,
            matches: matches, verificationBytes: verification, running: running, complete: complete,
            truncated: truncated, stop: stop)
    }
}

/// Only metadata is appended here; Rust owns candidate filtering and exact
/// verification. Every repeated row must still carry its original witness.
struct AtlasIndexedAccumulator {
    private(set) var state: AtlasIndexedQueryState?
    private(set) var hits: [AtlasIndexedHit] = []
    private var byFile: [UInt64: AtlasIndexedHit] = [:]
    private var textBytes = 0
    private var excerptBytes = 0
    let id = UUID()
    mutating func head(_ page: AtlasIndexedQueryPage, basis: AtlasIndexedBasis) throws {
        let n = page.state
        if let p = state {
            guard p.running, n.eligible == p.eligible, n.captured == p.captured, n.captureUnavailable == p.captureUnavailable,
                  n.visibleMissing == p.visibleMissing,
                  n.step == p.step + 1, n.examined >= p.examined, n.examined <= p.examined + 1,
                  n.scanned >= p.scanned, n.skipped >= p.skipped, n.fallback >= p.fallback,
                  n.unavailable >= p.unavailable, n.retained >= p.retained, n.matches >= p.matches,
                  n.verificationBytes >= p.verificationBytes else { throw AtlasIndexWire.invalid() }
        } else {
            guard n.running, n.step == 0, n.retained == 0, n.matches == 0,
                  n.examined == n.captureUnavailable, n.scanned == 0, n.skipped == 0,
                  n.verificationBytes == 0 else { throw AtlasIndexWire.invalid() }
        }
        state = n
        try append(page, start: 0, limit: 64)
    }
    mutating func append(_ page: AtlasIndexedQueryPage, start: Int, limit: Int) throws {
        guard let state, page.state == state, start >= 0, start <= hits.count, start <= state.retained else { throw AtlasIndexWire.invalid() }
        let end = min(state.retained, start + limit)
        guard page.hits.count == end - start, page.next == (end < state.retained ? UInt64(end) : nil) else { throw AtlasIndexWire.invalid() }
        for (offset, row) in page.hits.enumerated() {
            let at = start + offset
            guard row.hit.id == at else { throw AtlasIndexWire.invalid() }
            if at < hits.count {
                guard row.repeats(hits[at]) else { throw AtlasIndexWire.invalid() }
            } else {
                guard at == hits.count, hits.count < 1000 else { throw AtlasIndexWire.invalid() }
                if let previous = byFile[row.file] {
                    guard previous.revision == row.revision, previous.pathHex == row.pathHex,
                          previous.hit.captureSHA256 == row.hit.captureSHA256,
                          previous.hit.captureByteLength == row.hit.captureByteLength else { throw AtlasIndexWire.invalid() }
                }
                let charge = row.pathHex.utf8.count + row.rawPathBytes.count
                    + 2 * row.hit.path.utf8.count + (row.hit.sourcePath?.utf8.count ?? 0)
                    + 2 * (row.hit.actualText?.utf8.count ?? 0) + 512
                guard charge <= AtlasSearchRetention.witnessBytes - textBytes else { throw AtlasIndexWire.invalid() }
                var admitted = row
                if let excerpt = row.hit.excerpt {
                    let charge = excerpt.retainedByteCount + 256
                    if charge <= AtlasSearchRetention.excerptBytes - excerptBytes { excerptBytes += charge }
                    else { admitted.hit.excerpt = nil }
                }
                textBytes += charge; byFile[row.file] = admitted; hits.append(admitted)
            }
        }
    }
}
