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
        do {
            if let failure = AtlasSearchExpression.queryFailure(in: data) { throw failure }
            if let failure = AtlasSearchOccurrencePolicy.queryFailure(in: data) { throw failure }
            return try JSONDecoder().decode(Self.self, from: data)
        }
        catch let error as AtlasSearchError { throw error }
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

/// The scope admitted before source capture spends its file/byte allowance.
/// Query scope is separate: later requests may search a narrower or broader
/// predicate without changing this immutable source population.
struct AtlasIndexedCaptureScope: Sendable, Equatable {
    let fileScope: AtlasSearchFileScope
    let pathScope: AtlasSearchPathScope
    let eligibleFiles, excludedFiles, visibleMissingFiles: Int
    let hasEcho: Bool

    init(_ wire: AtlasIndexWire, fileScope: AtlasSearchFileScope, pathScope: AtlasSearchPathScope,
         files: Int, requiresEcho: Bool) throws {
        self.fileScope = fileScope; self.pathScope = pathScope
        hasEcho = wire.fields.allKeys.contains { key in
            key.stringValue == "capture_search_scope" || key.stringValue.hasPrefix("capture_scope_")
                || key.stringValue == "capture_eligible_files" || key.stringValue == "capture_excluded_files"
                || key.stringValue == "capture_case_sensitive"
        }
        if !hasEcho {
            guard !requiresEcho, fileScope.isAll, pathScope.isWorkspace else { throw AtlasIndexWire.invalid() }
            eligibleFiles = files; excludedFiles = 0; visibleMissingFiles = 0
            return
        }
        guard try wire.flag("capture_case_sensitive"),
              try wire.number("catalogued_files") == UInt64(files) else { throw AtlasIndexWire.invalid() }
        // Read required fields, even for all/workspace. Optional legacy query
        // echoes must not make a partially missing capture receipt acceptable.
        eligibleFiles = try fileScope.eligibleFiles(kind: wire.text("capture_search_scope"),
            extensions: wire.text("capture_scope_extensions"), filenames: wire.text("capture_scope_filenames"),
            eligible: wire.text("capture_eligible_files"), excluded: wire.text("capture_excluded_files"),
            catalogued: files, pathScoped: !pathScope.isWorkspace)
        excludedFiles = files - eligibleFiles
        try pathScope.validateEcho(kind: wire.text("capture_scope_path_kind"),
            path: wire.text("capture_scope_path"), hex: wire.text("capture_scope_path_hex"),
            regionSHA256: wire.optionalText("capture_scope_region_sha256"),
            regionFiles: wire.optionalText("capture_scope_region_files"),
            regionPathBytes: wire.optionalText("capture_scope_region_path_bytes"))
        visibleMissingFiles = try pathScope.validateRegionCoverage(
            catalogued: wire.optionalText("capture_scope_region_catalogued_files"),
            missing: wire.optionalText("capture_scope_region_missing_files"), eligible: eligibleFiles,
            searchCatalogFiles: files, allFileTypes: fileScope.isAll, complete: false)
    }

    func validate(_ wire: AtlasIndexWire, files: Int) throws {
        let echoed = try Self(wire, fileScope: fileScope, pathScope: pathScope,
            files: files, requiresEcho: hasEcho)
        guard echoed == self else { throw AtlasIndexWire.invalid() }
    }
}

struct AtlasIndexedBasis: Sendable, Equatable {
    let owner, manifest, layout, index, captureManifest: UInt64
    let files, capturedFiles, unavailable, pending: Int
    let sourceBytes: UInt64
    let discoveryComplete: Bool
    let captureScope: AtlasIndexedCaptureScope

    func validate(_ wire: AtlasIndexWire, command: String) throws {
        try wire.header(schema: "fcb.atlas-search/1", command: command, owner: owner)
        guard try wire.number("source_manifest") == manifest, try wire.number("layout_revision") == layout,
              try wire.number("index_generation") == index, try wire.number("capture_manifest") == captureManifest,
              try wire.text("scope") == "all", try wire.number("workspace_files") == UInt64(files) else {
            throw AtlasIndexWire.invalid()
        }
        try captureScope.validate(wire, files: files)
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
        let structure = try AtlasIndexedStructure.decode(wire)
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
            structure: structure, actualText: actualText, excerpt: excerpt)
    }
    static func == (a: Self, b: Self) -> Bool {
        a.sameWitness(as: b) && a.hit.excerpt == b.hit.excerpt
    }
    private func sameWitness(as other: Self) -> Bool {
        file == other.file && revision == other.revision && pathHex == other.pathHex && hit.id == other.hit.id
            && hit.start == other.hit.start && hit.end == other.hit.end
            && hit.captureSHA256 == other.hit.captureSHA256 && hit.captureByteLength == other.hit.captureByteLength
            && hit.path.utf8.elementsEqual(other.hit.path.utf8)
            && hit.structure == other.hit.structure
            && AtlasSearchText.sameActualText(hit.actualText, other.hit.actualText)
    }
    func repeats(_ old: Self) -> Bool {
        sameWitness(as: old) && (hit.excerpt == nil || old.hit.excerpt == nil || hit.excerpt == old.hit.excerpt)
    }
    func matches(_ other: SearchHit) -> Bool {
        hit.id == other.id && hit.start == other.start && hit.end == other.end
            && hit.captureSHA256 == other.captureSHA256 && hit.captureByteLength == other.captureByteLength
            && hit.sourcePath?.utf8.elementsEqual(other.sourcePath?.utf8 ?? "".utf8) == true
            && hit.structure == other.structure
            && AtlasSearchText.sameActualText(hit.actualText, other.actualText)
    }
}

/// Only validates shared-engine evidence. Extraction, name matching and token
/// boundaries stay in Rust; native code cannot upgrade candidates to semantics.
enum AtlasIndexedStructure {
    static func decode(_ row: AtlasIndexWire) throws -> AtlasSearchStructure? {
        guard try !row.absent(["structure"]) else { return nil }
        let value = try row.object("structure"), range = try value.object("evidence_range")
        return try AtlasSearchStructure(name: value.text("name"), kind: value.text("kind"),
            language: value.text("language"), evidence: value.text("evidence_level"),
            candidate: value.number("candidate_id"), headingSlug: value.optionalText("heading_slug"),
            evidenceStart: range.number("start"), evidenceEnd: range.number("end"))
    }

    static func accepts(_ hit: SearchHit, mode: AtlasSearchMode, needle: String) -> Bool {
        guard !mode.isTextual, hit.actualText == nil, let value = hit.structure,
              value.mode == mode, value.evidence == mode.evidence,
              !value.name.isEmpty, value.name.utf8.count <= 4096,
              !value.kind.isEmpty, value.kind.utf8.count <= 64, !value.kind.utf8.contains(0),
              !value.language.isEmpty, value.language.utf8.count <= 64,
              value.candidate > 0, value.candidate <= 4096,
              let bytes = hit.captureByteLength, bytes <= mode.structureFileByteLimit,
              value.evidenceStart <= hit.start, value.evidenceEnd >= hit.end,
              value.evidenceEnd <= bytes else { return false }
        if let slug = value.headingSlug, slug.utf8.count > 4096 || slug.utf8.contains(0) { return false }
        switch mode {
        case .headings:
            return value.headingSlug?.isEmpty == false && value.kind == "heading" && value.language == "markdown"
                && value.evidenceStart == hit.start && value.evidenceEnd == hit.end
        case .occurrences:
            return value.name.utf8.elementsEqual(needle.utf8)
                && value.kind == "identifier-occurrence" && value.language == "text" && value.headingSlug == nil
                && value.evidenceStart == hit.start && value.evidenceEnd == hit.end
                && (hit.end - hit.start == UInt64(needle.utf8.count)
                    || hit.end - hit.start == UInt64(needle.utf16.count) * 2)
                && hit.excerpt?.matchesSourceText(needle) != false
        case .symbols:
            return value.headingSlug == nil
                && ["rust", "python", "javascript", "typescript", "go", "cpp"].contains(value.language)
        case .text, .expression: return false
        }
    }

    static func coverage(_ w: AtlasIndexWire, mode: AtlasSearchMode, nameMode: AtlasSearchNameMode,
                         scanned: Int, examined: Int, unavailable: Int, retained: Int,
                         matches: UInt64) throws -> AtlasSearchStructureCoverage? {
        if mode == .occurrences {
            guard nameMode == .exact, try w.flag("includes_comments_and_literals"),
                  try !w.flag("compiler_resolved"),
                  try w.text("token_policy") == AtlasSearchOccurrencePolicy.tokenPolicy,
                  try w.text("name_syntax") == AtlasSearchOccurrencePolicy.nameSyntax,
                  try w.text("unicode_normalization") == "none" else { throw AtlasIndexWire.invalid() }
        } else {
            guard try w.absent(["includes_comments_and_literals", "compiler_resolved", "token_policy",
                "name_syntax", "unicode_normalization"]) else { throw AtlasIndexWire.invalid() }
        }
        if mode.isTextual {
            guard try w.absent(["requested_mode", "realized_mode", "name_mode", "name_case", "evidence_level",
                "semantic_complete", "completeness_basis", "structure_file_byte_limit", "unsupported_structure_files",
                "extraction_limited_files", "structure_metadata_omitted", "structure_metadata_bytes"]) else {
                throw AtlasIndexWire.invalid()
            }
            return nil
        }
        guard try w.text("requested_mode") == mode.rawValue, try w.text("realized_mode") == mode.evidence,
              try w.text("name_mode") == nameMode.rawValue, try w.text("name_case") == "sensitive",
              try w.text("evidence_level") == mode.evidence, try !w.flag("semantic_complete"),
              try w.text("completeness_basis") == "admitted-extraction-processing" else { throw AtlasIndexWire.invalid() }
        let coverage = try AtlasSearchStructureCoverage(fileByteLimit: w.number("structure_file_byte_limit"),
            unsupportedFiles: w.count("unsupported_structure_files", upTo: unavailable),
            // Heading source qualification can exhaust its reservation before
            // any extraction is accepted into scanned_files.
            extractionLimitedFiles: w.count("extraction_limited_files", upTo: unavailable),
            metadataOmitted: w.count("structure_metadata_omitted", upTo: 4096 * 4096),
            metadataBytes: w.count("structure_metadata_bytes", upTo: 8 * 1024 * 1024))
        guard coverage.fileByteLimit == mode.structureFileByteLimit,
              coverage.unsupportedFiles + coverage.extractionLimitedFiles <= unavailable,
              coverage.unsupportedFiles + scanned <= examined,
              coverage.metadataOmitted == 0 || unavailable > 0,
              matches <= 4096 * 4096,
              matches <= UInt64(retained + coverage.metadataOmitted + 1) else { throw AtlasIndexWire.invalid() }
        return coverage
    }
}

struct AtlasIndexedQueryState: Equatable {
    let eligible, captured, captureUnavailable, visibleMissing: Int
    let examined, scanned, unavailable, pending, retained, step, skipped, fallback: Int
    let matches, verificationBytes: UInt64
    let running, complete, truncated, countAllMatches, matchCountComplete: Bool
    let stop: String?
    let expression: AtlasSearchExpression?
    let structureCoverage: AtlasSearchStructureCoverage?
}
struct AtlasIndexedQueryPage {
    let state: AtlasIndexedQueryState
    let hits: [AtlasIndexedHit]
    let next: UInt64?
    var expression: AtlasSearchExpression? { state.expression }

    init(_ json: String?, basis: AtlasIndexedBasis, generation: UInt64, needle: String,
         scope: AtlasSearchFileScope = .all, pathScope: AtlasSearchPathScope = .workspace,
         caseSensitive: Bool = true, mode: AtlasSearchMode = .text, nameMode: AtlasSearchNameMode = .contains,
         countAllMatches: Bool = false,
         command: String) throws {
        let w = try AtlasIndexWire.decode(json)
        try basis.validate(w, command: command)
        guard mode.isTextual || (caseSensitive && !countAllMatches) else { throw AtlasIndexWire.invalid() }
        let kind = try w.optionalText("search_scope")
        try pathScope.validateEcho(kind: w.optionalText("scope_path_kind"),
            path: w.optionalText("scope_path"), hex: w.optionalText("scope_path_hex"),
            regionSHA256: w.optionalText("scope_region_sha256"), regionFiles: w.optionalText("scope_region_files"),
            regionPathBytes: w.optionalText("scope_region_path_bytes"))
        let eligible = try scope.eligibleFiles(kind: kind, extensions: w.optionalText("scope_extensions"),
            filenames: w.optionalText("scope_filenames"), eligible: w.optionalText("eligible_files"),
            excluded: w.optionalText("excluded_files"), catalogued: basis.files, pathScoped: !pathScope.isWorkspace)
        let primaryNeedle = try w.text("needle")
        let expression = try AtlasSearchExpression.Wire(index: w).admit(mode: mode,
            query: needle, needle: primaryNeedle, eligibleFiles: eligible)
        let expressionExcluded = expression?.excludedFiles ?? 0
        if expression != nil {
            guard let explicitCase = try w.optionalFlag("case_sensitive"), explicitCase == caseSensitive else {
                throw AtlasIndexWire.invalid()
            }
        }
        let queriesCaptureScope = scope == basis.captureScope.fileScope && pathScope == basis.captureScope.pathScope
        if queriesCaptureScope {
            guard eligible == basis.captureScope.eligibleFiles else { throw AtlasIndexWire.invalid() }
        }
        let captured, captureUnavailable: Int
        if kind == nil && expression == nil {
            captured = basis.capturedFiles; captureUnavailable = basis.unavailable
        } else {
            captured = try w.count("eligible_captured_files", upTo: min(basis.capturedFiles, eligible))
            captureUnavailable = try w.count("scoped_capture_unavailable_files", upTo: min(basis.unavailable, eligible))
        }
        guard captured + captureUnavailable <= eligible - expressionExcluded else {
            throw AtlasIndexWire.invalid()
        }
        if expression == nil && (queriesCaptureScope || (scope.isAll && pathScope.isWorkspace)) {
            guard captured == basis.capturedFiles, captureUnavailable == basis.unavailable else { throw AtlasIndexWire.invalid() }
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
        let hasCountReceipt = w.fields.contains(AtlasIndexWire.Key("count_all_matches"))
            || w.fields.contains(AtlasIndexWire.Key("match_count_complete"))
        let matchCountComplete: Bool
        if hasCountReceipt || countAllMatches {
            // An opted-in count must carry both typed receipts on every page.
            // Missing/null fields must not turn a partial count into a total.
            guard try w.flag("count_all_matches") == countAllMatches else { throw AtlasIndexWire.invalid() }
            matchCountComplete = try w.flag("match_count_complete")
            guard matchCountComplete == complete else { throw AtlasIndexWire.invalid() }
        } else {
            matchCountComplete = complete
        }
        let visibleMissing = try pathScope.validateRegionCoverage(catalogued: w.optionalText("scope_region_catalogued_files"),
            missing: w.optionalText("scope_region_missing_files"), eligible: eligible, searchCatalogFiles: basis.files,
            allFileTypes: scope.isAll, complete: complete)
        guard try w.number("query_generation") == generation, generation > basis.index,
              expression != nil || primaryNeedle.utf8.elementsEqual(needle.utf8),
              try w.text("mode") == (mode == .text ? AtlasSearchText.wireMode(caseSensitive: caseSensitive) : mode.wireMode),
              try (w.optionalFlag("case_sensitive") ?? true) == caseSensitive,
              try w.text("search_strategy") == (mode.isTextual ? "retained-ephemeral-index" : "retained-capture-structure"),
              try w.flag("discovery_complete") == basis.discoveryComplete,
              try w.number("catalogued_files") == UInt64(basis.files),
              try w.number("displayed_hits") == UInt64(retained), try w.number("verified_files") == UInt64(scanned),
              try w.number("source_bytes_read") == 0, try w.number("read_calls") == 0,
              scanned + skipped <= captured,
              pending == eligible - examined, unavailable >= captureUnavailable, unavailable <= examined,
              verification <= (expression?.maxScanBytes ?? basis.sourceBytes),
              expression == nil || expression?.scannedInputBytes == verification,
              matches >= UInt64(retained), running == (stop == nil) else {
            throw AtlasIndexWire.invalid()
        }
        if mode.isTextual {
            guard examined == expressionExcluded + scanned + skipped + captureUnavailable,
                  unavailable <= captureUnavailable + scanned else { throw AtlasIndexWire.invalid() }
        } else {
            // Unsupported or oversized captures are examined without running
            // an extractor. They remain unavailable, never proven negatives.
            guard skipped == 0, fallback == 0, examined >= scanned + captureUnavailable,
                  examined <= captured + captureUnavailable,
                  examined - scanned <= unavailable else { throw AtlasIndexWire.invalid() }
        }
        let structureCoverage = try AtlasIndexedStructure.coverage(w, mode: mode,
            nameMode: nameMode, scanned: scanned, examined: examined - captureUnavailable,
            unavailable: unavailable - captureUnavailable, retained: retained, matches: matches)
        if let stop, !["all-files-examined", "file-limit", "source-byte-limit", "capture-scope", "match-limit", "verification-byte-limit"].contains(stop) {
            throw AtlasIndexWire.invalid()
        }
        if stop == "capture-scope" {
            guard pending > 0, basis.captureScope.excludedFiles > 0 else { throw AtlasIndexWire.invalid() }
        }
        if countAllMatches {
            guard mode.isTextual,
                  matches <= basis.sourceBytes, truncated == (matches > UInt64(retained)),
                  stop != "match-limit" else { throw AtlasIndexWire.invalid() }
        }
        if complete {
            guard !running, basis.discoveryComplete, pending == 0, unavailable == 0,
                  visibleMissing == 0 else { throw AtlasIndexWire.invalid() }
            if countAllMatches {
                guard matchCountComplete, stop == "all-files-examined" else { throw AtlasIndexWire.invalid() }
            } else {
                guard !truncated, matches == UInt64(retained) else { throw AtlasIndexWire.invalid() }
            }
            if let coverage = structureCoverage {
                guard stop == "all-files-examined", coverage.unsupportedFiles == 0,
                      coverage.extractionLimitedFiles == 0, coverage.metadataOmitted == 0 else {
                    throw AtlasIndexWire.invalid()
                }
            }
        }
        hits = try w.rows("hits", limit: command == "page" ? 128 : 64)
        guard hits.allSatisfy({ scope.admitsRawPath($0.rawPathBytes) && pathScope.admitsRawPath($0.rawPathBytes)
            && basis.captureScope.fileScope.admitsRawPath($0.rawPathBytes)
            && basis.captureScope.pathScope.admitsRawPath($0.rawPathBytes)
            && (mode.isTextual
                ? ($0.hit.structure == nil && (expression == nil || $0.hit.actualText != nil)
                    && AtlasSearchText.accepts(actualText: $0.hit.actualText, needle: primaryNeedle, caseSensitive: caseSensitive)
                    && ($0.hit.excerpt?.matchesSourceText($0.hit.actualText ?? primaryNeedle) != false))
                : AtlasIndexedStructure.accepts($0.hit, mode: mode, needle: primaryNeedle)) }) else {
            throw AtlasIndexWire.invalid()
        }
        next = try w.optionalNumber("next_offset")
        state = AtlasIndexedQueryState(eligible: eligible, captured: captured, captureUnavailable: captureUnavailable,
            visibleMissing: visibleMissing,
            examined: examined, scanned: scanned, unavailable: unavailable,
            pending: pending, retained: retained, step: step, skipped: skipped, fallback: fallback,
            matches: matches, verificationBytes: verification, running: running, complete: complete,
            truncated: truncated, countAllMatches: countAllMatches, matchCountComplete: matchCountComplete,
            stop: stop, expression: expression, structureCoverage: structureCoverage)
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
                  n.visibleMissing == p.visibleMissing, n.countAllMatches == p.countAllMatches,
                  n.step == p.step + 1, n.examined >= p.examined, n.examined <= p.examined + 1,
                  n.scanned >= p.scanned, n.skipped >= p.skipped, n.fallback >= p.fallback,
                  n.unavailable >= p.unavailable, n.retained >= p.retained, n.matches >= p.matches,
                  n.verificationBytes >= p.verificationBytes else { throw AtlasIndexWire.invalid() }
            if let expression = n.expression, let previous = p.expression {
                guard expression.advances(previous) else { throw AtlasIndexWire.invalid() }
            } else if n.expression != nil || p.expression != nil { throw AtlasIndexWire.invalid() }
            if let coverage = n.structureCoverage, let previous = p.structureCoverage {
                guard coverage.advances(previous) else { throw AtlasIndexWire.invalid() }
            } else if n.structureCoverage != nil || p.structureCoverage != nil { throw AtlasIndexWire.invalid() }
        } else {
            guard n.running, n.step == 0, n.retained == 0, n.matches == 0,
                  n.examined == n.captureUnavailable + (n.expression?.excludedFiles ?? 0), n.scanned == 0, n.skipped == 0,
                  n.verificationBytes == 0 else { throw AtlasIndexWire.invalid() }
            if let coverage = n.structureCoverage {
                guard coverage.unsupportedFiles == 0, coverage.extractionLimitedFiles == 0,
                      coverage.metadataOmitted == 0, coverage.metadataBytes == 0 else { throw AtlasIndexWire.invalid() }
            }
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
                    + 2 * (row.hit.actualText?.utf8.count ?? 0)
                    + 2 * (row.hit.structure?.name.utf8.count ?? 0)
                    + 2 * (row.hit.structure?.headingSlug?.utf8.count ?? 0) + 512
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

extension AtlasSearchExpression.Wire {
    init(index wire: AtlasIndexWire) throws {
        try self.init(expression: wire.optionalText("expression"), primary_needle: wire.optionalText("primary_needle"),
            query_syntax: wire.optionalText("query_syntax"), predicate_scope: wire.optionalText("predicate_scope"),
            path_filter_domain: wire.optionalText("path_filter_domain"), max_scan_bytes: wire.optionalText("max_scan_bytes"),
            scanned_input_bytes: wire.optionalText("scanned_input_bytes"),
            expression_excluded_files: wire.optionalText("expression_excluded_files"),
            expression_scope_files: wire.optionalText("expression_scope_files"))
    }
}
