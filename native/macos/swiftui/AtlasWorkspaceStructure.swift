import Foundation

enum AtlasWorkspaceStructureMode: String, CaseIterable, Identifiable, Sendable {
    case symbols, headings
    var id: String { rawValue }
    var label: String { self == .symbols ? "Symbols" : "Headings" }
    var code: UInt64 { self == .symbols ? 0 : 1 }
    var evidence: String { self == .symbols ? "heuristic-outline-candidate" : "fmd-heading" }
    var maximumFileBytes: UInt64 { self == .symbols ? 65_536 : 262_144 }
}

enum AtlasWorkspaceStructureLimits {
    static let responseBytes = 8 * 1024 * 1024
    static let maximumMatches: UInt64 = 256
    static let page: UInt64 = 32
    static let defaultScanBytes: UInt64 = 256 * 1024 * 1024
    static let maximumScanBytes: UInt64 = 4 * 1024 * 1024 * 1024
    static let maximumReadCalls: UInt64 = 1_048_576
    static let retainedSourceBytes: UInt64 = 32 * 1024 * 1024
}

enum AtlasWorkspaceStructureError: Error, Sendable {
    case invalidInput, invalidResponse, unavailable, canceled
    case engine(String)
    var message: String {
        switch self {
        case .invalidInput: return "Enter a name or heading of 1–1,024 UTF-8 bytes without NUL and choose a supported scan limit."
        case .invalidResponse: return "The structural search response could not be verified. Previously opened source is unchanged."
        case .unavailable: return "Project structure search is unavailable. Close another search or reopen this sheet."
        case .canceled: return "Search canceled. Entered work may still be draining."
        case .engine(let code): return "The structural search operation was refused (\(code))."
        }
    }
}

struct AtlasWorkspaceStructureRequest: Equatable, Sendable {
    let needle: String
    let mode: AtlasWorkspaceStructureMode
    let nameMode: AtlasSearchNameMode
    let fileScope: AtlasSearchFileScope
    let pathScope: AtlasSearchPathScope
    let discoveryPolicy: AtlasDiscoveryPolicy
    let maximumMatches, maximumScanBytes, maximumReadCalls, startAfter: UInt64

    init(needle: String, mode: AtlasWorkspaceStructureMode, nameMode: AtlasSearchNameMode,
        fileScope: AtlasSearchFileScope = .all, pathScope: AtlasSearchPathScope = .workspace,
        discoveryPolicy: AtlasDiscoveryPolicy = .repositoryRules,
        maximumMatches: UInt64 = AtlasWorkspaceStructureLimits.maximumMatches,
        maximumScanBytes: UInt64 = AtlasWorkspaceStructureLimits.defaultScanBytes,
        maximumReadCalls: UInt64 = AtlasWorkspaceStructureLimits.maximumReadCalls,
        startAfter: UInt64 = 0) throws {
        let end = startAfter.addingReportingOverflow(maximumMatches)
        guard !needle.isEmpty, needle.utf8.count <= 1024, !needle.utf8.contains(0),
              (1...AtlasWorkspaceStructureLimits.maximumMatches).contains(maximumMatches),
              (1...AtlasWorkspaceStructureLimits.maximumScanBytes).contains(maximumScanBytes),
              (1...AtlasWorkspaceStructureLimits.maximumReadCalls).contains(maximumReadCalls),
              !end.overflow, end.partialValue < UInt64.max else { throw AtlasWorkspaceStructureError.invalidInput }
        self.needle = needle; self.mode = mode; self.nameMode = nameMode
        self.fileScope = fileScope; self.pathScope = pathScope; self.discoveryPolicy = discoveryPolicy
        self.maximumMatches = maximumMatches; self.maximumScanBytes = maximumScanBytes
        self.maximumReadCalls = maximumReadCalls; self.startAfter = startAfter
    }

    static func == (a: Self, b: Self) -> Bool {
        a.needle.utf8.elementsEqual(b.needle.utf8) && a.mode == b.mode && a.nameMode == b.nameMode
            && a.fileScope == b.fileScope && a.pathScope == b.pathScope && a.discoveryPolicy == b.discoveryPolicy
            && a.maximumMatches == b.maximumMatches && a.maximumScanBytes == b.maximumScanBytes
            && a.maximumReadCalls == b.maximumReadCalls && a.startAfter == b.startAfter
    }

    /// Validate the requested literal name predicate without Unicode folding or
    /// parsing source. Rust remains the only owner of structural extraction.
    func admitsName(_ name: String) -> Bool {
        switch nameMode {
        case .exact: return name.utf8.elementsEqual(needle.utf8)
        case .prefix: return name.utf8.starts(with: needle.utf8)
        case .contains: return Data(name.utf8).range(of: Data(needle.utf8)) != nil
        }
    }
}

struct AtlasWorkspaceStructureRange: Equatable, Sendable {
    let start, end: UInt64
    init(_ wire: AtlasWorkspaceStructureWire, sourceBytes: UInt64) throws {
        start = try wire.number("start"); end = try wire.number("end")
        guard start < end, end <= sourceBytes else { throw AtlasWorkspaceStructureError.invalidResponse }
    }
}

struct AtlasWorkspaceStructureHit: Equatable, Sendable, Identifiable {
    let index, occurrence, candidate, file, sourceRevision, sourceBytes: UInt64
    let path: [UInt8]
    let displayPath, name, kind, language, digest, encoding: String
    let headingSlug: String?
    let range, evidenceRange: AtlasWorkspaceStructureRange
    var id: UInt64 { index }

    init(_ wire: AtlasWorkspaceStructureWire, summary: AtlasWorkspaceStructureSummary) throws {
        index = try wire.number("index"); occurrence = try wire.number("occurrence_id")
        candidate = try wire.number("candidate_id"); file = try wire.number("file_id")
        sourceRevision = try wire.number("source_revision"); sourceBytes = try wire.number("source_length")
        let nativePath = try wire.object("path")
        path = try nativePath.path(maximumBytes: 2048)
        displayPath = try nativePath.text("display")
        name = try wire.text("name"); kind = try wire.text("kind"); language = try wire.text("language")
        headingSlug = try wire.optionalText("heading_slug", required: true)
        digest = try wire.text("capture_sha256"); encoding = try wire.text("encoding")
        range = try .init(wire.object("original_range"), sourceBytes: sourceBytes)
        evidenceRange = try .init(wire.object("evidence_range"), sourceBytes: sourceBytes)
        guard index < summary.retainedHits, occurrence > summary.request.startAfter,
              occurrence <= summary.matchesSeen, candidate > 0, file > 0, sourceRevision > 0,
              sourceBytes <= summary.request.mode.maximumFileBytes,
              AtlasDirectoryFocus.project.containsFile(path),
              summary.request.fileScope.admitsRawPath(path), summary.request.pathScope.admitsRawPath(path),
              !name.isEmpty, name.utf8.count <= 4096, summary.request.admitsName(name),
              !kind.isEmpty, kind.utf8.count <= 128, !language.isEmpty, language.utf8.count <= 128,
              headingSlug.map({ $0.utf8.count <= 4096 }) ?? true,
              AtlasWorkspaceStructureWire.hex(digest, bytes: 32),
              ["utf8", "utf16le", "utf16be"].contains(encoding),
              try wire.flag("capture_complete"), try wire.text("consistency") == "unchanged-metadata-not-atomic",
              evidenceRange.start <= range.start, range.end <= evidenceRange.end else {
            throw AtlasWorkspaceStructureError.invalidResponse
        }
        if summary.request.mode == .headings {
            guard headingSlug != nil, kind == "heading", language == "markdown" else {
                throw AtlasWorkspaceStructureError.invalidResponse
            }
        } else if headingSlug != nil { throw AtlasWorkspaceStructureError.invalidResponse }
    }

    static func == (a: Self, b: Self) -> Bool {
        a.index == b.index && a.occurrence == b.occurrence && a.candidate == b.candidate
            && a.file == b.file && a.sourceRevision == b.sourceRevision && a.sourceBytes == b.sourceBytes
            && a.path == b.path && a.range == b.range && a.evidenceRange == b.evidenceRange
            && a.name.utf8.elementsEqual(b.name.utf8) && a.kind.utf8.elementsEqual(b.kind.utf8)
            && a.language.utf8.elementsEqual(b.language.utf8) && a.digest == b.digest && a.encoding == b.encoding
            && a.headingSlug.map { Array($0.utf8) } == b.headingSlug.map { Array($0.utf8) }
    }
}

struct AtlasWorkspaceStructureSummary: Equatable, Sendable {
    let owner, generation, revision: UInt64
    let root: [UInt8]
    let request: AtlasWorkspaceStructureRequest
    let more, complete, discoveryComplete, discoveryMore, hasMoreMatches: Bool
    let state: String
    let stopReason: String?
    let missingScopeFiles: UInt64
    let counts: [String: UInt64]
    var retainedHits: UInt64 { counts["retained_hits"]! }
    var matchesSeen: UInt64 { counts["matches_seen"]! }

    private static let counterKeys = [
        "catalogued_files", "eligible_files", "excluded_files", "pending_catalogued_files",
        "files_examined", "files_unexamined", "files_scanned", "incomplete_files", "unsupported_files",
        "oversized_files", "unavailable_files", "analysis_limited_files", "matching_files", "matches_seen",
        "retained_hits", "retained_matching_files", "retained_source_bytes", "structure_metadata_bytes",
        "structure_metadata_omitted", "source_retention_omitted_files", "page_start_after", "skipped_matches",
        "payload_bytes_read", "read_calls", "retained_catalog_pages", "retained_catalog_files"
    ]

    init(_ wire: AtlasWorkspaceStructureWire, owner: UInt64, generation: UInt64, root: [UInt8],
        request: AtlasWorkspaceStructureRequest, acceptingCanonicalRoot: Bool = false) throws {
        guard owner > 0, generation > 0, try wire.number("owner") == owner,
              try wire.number("query_generation") == generation else { throw AtlasWorkspaceStructureError.invalidResponse }
        self.owner = owner; self.generation = generation; self.request = request
        revision = try wire.number("revision"); self.root = try wire.object("root_path").path(maximumBytes: 16_384)
        // Only the owned, zero-I/O open receipt may establish the canonical
        // root of an ancestor alias. Every later response must keep these raw
        // bytes; no native filesystem lookup or UTF-8 round trip is needed.
        guard revision > 0, acceptingCanonicalRoot || self.root == root,
              root.first == 47, !root.contains(0), root.count <= 16_384,
              self.root.first == 47, !self.root.contains(0),
              self.root == [47] || !self.root.dropFirst().split(separator: 47,
                omittingEmptySubsequences: false).contains(where: {
                    $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
                }),
              try wire.text("scope") == "progressively-discovered-whole-files",
              try wire.text("strategy") == "bounded-complete-capture-extraction",
              try wire.text("capture_kind") == "complete-retained-source",
              try wire.text("source_observation") == "per-file-captures-not-atomic-workspace",
              try wire.text("ordering") == "provisional-discovery-order",
              try wire.text("membership") == "progressive-observation",
              try wire.text("requested_mode") == request.mode.rawValue,
              try wire.text("realized_mode") == request.mode.evidence,
              try wire.text("evidence_level") == request.mode.evidence,
              try !wire.flag("semantic_complete"), try !wire.flag("compiler_resolved"),
              try wire.text("name_case") == "sensitive", try wire.text("name_mode") == request.nameMode.rawValue,
              try wire.text("needle").utf8.elementsEqual(request.needle.utf8),
              try wire.text("completeness_basis") == "admitted-extraction-processing",
              request.discoveryPolicy.admits(try wire.text("discovery_policy")),
              try wire.flag("rule_files_enabled") == !request.discoveryPolicy.includesExcluded else {
            throw AtlasWorkspaceStructureError.invalidResponse
        }
        if request.discoveryPolicy.includesExcluded { try wire.requireNull("rule_policy") }
        let limits = try wire.object("limits")
        guard try limits.number("max_matches") == request.maximumMatches,
              try limits.number("max_scan_bytes") == request.maximumScanBytes,
              try limits.number("max_read_calls") == request.maximumReadCalls,
              try limits.number("max_file_bytes") == request.mode.maximumFileBytes,
              try limits.number("max_retained_source_bytes") == AtlasWorkspaceStructureLimits.retainedSourceBytes,
              try limits.number("max_steps_per_advance") == 16 else { throw AtlasWorkspaceStructureError.invalidResponse }
        var counters: [String: UInt64] = [:]
        for key in Self.counterKeys { counters[key] = try wire.number(key) }
        counts = counters
        more = try wire.flag("more"); complete = try wire.flag("search_complete")
        discoveryComplete = try wire.flag("discovery_complete"); discoveryMore = try wire.flag("discovery_more")
        hasMoreMatches = try wire.flag("page_has_more_matches")
        state = try wire.text("state"); stopReason = try wire.optionalText("stop_reason", required: true)
        let catalogued = counters["catalogued_files"]!, eligible = counters["eligible_files"]!
        let pending = counters["pending_catalogued_files"]!, examined = counters["files_examined"]!
        let seen = counters["matches_seen"]!, retained = counters["retained_hits"]!
        guard catalogued <= UInt64(Int.max), pending <= catalogued, eligible <= catalogued - pending,
              counters["excluded_files"]! == catalogued - pending - eligible,
              examined <= eligible, counters["files_unexamined"]! == eligible - examined,
              counters["files_scanned"]! <= examined, counters["incomplete_files"]! <= examined,
              counters["matching_files"]! <= counters["files_scanned"]!,
              counters["retained_matching_files"]! <= counters["matching_files"]!,
              counters["retained_matching_files"]! <= retained,
              retained <= request.maximumMatches, retained <= seen - min(request.startAfter, seen),
              counters["page_start_after"]! == request.startAfter,
              counters["skipped_matches"]! == min(request.startAfter, seen),
              counters["retained_source_bytes"]! <= AtlasWorkspaceStructureLimits.retainedSourceBytes,
              counters["structure_metadata_bytes"]! <= 2 * 1024 * 1024,
              counters["structure_metadata_omitted"]! <= seen,
              counters["source_retention_omitted_files"]! <= counters["matching_files"]!,
              counters["payload_bytes_read"]! <= request.maximumScanBytes,
              counters["read_calls"]! <= request.maximumReadCalls,
              counters["retained_catalog_pages"]! <= 4, counters["retained_catalog_files"]! <= 1024,
              try wire.flag("terminal") == !more, try wire.flag("selection_available") == (retained > 0),
              try wire.flag("count_all_matches"), try wire.flag("match_count_complete") == complete,
              try wire.flag("truncated") == (seen > retained),
              hasMoreMatches == (seen - min(request.startAfter, seen) > retained),
              ["discovering", "complete", "partial", "canceled"].contains(state),
              more == (state == "discovering"), complete == (state == "complete"),
              !discoveryComplete || !discoveryMore,
              stopReason == nil || ["source-byte-limit", "read-call-limit", "user-stopped",
                "discovery-unavailable", "work-failed"].contains(stopReason!),
              !more || stopReason == nil else { throw AtlasWorkspaceStructureError.invalidResponse }
        for key in ["unsupported_files", "oversized_files", "unavailable_files", "analysis_limited_files"] {
            guard counters[key]! <= counters["incomplete_files"]! else { throw AtlasWorkspaceStructureError.invalidResponse }
        }
        if complete {
            guard discoveryComplete, !more, stopReason == nil, pending == 0,
                  counters["incomplete_files"] == 0, examined == eligible else { throw AtlasWorkspaceStructureError.invalidResponse }
        }
        func optional(_ key: String) throws -> String? { try wire.optionalText(key) }
        try request.pathScope.validateEcho(kind: wire.text("scope_path_kind"), path: wire.text("scope_path"),
            hex: wire.text("scope_path_hex"), regionSHA256: optional("scope_region_sha256"),
            regionFiles: optional("scope_region_files"), regionPathBytes: optional("scope_region_path_bytes"))
        let admitted = try request.fileScope.eligibleFiles(kind: wire.text("search_scope"),
            extensions: wire.text("scope_extensions"), filenames: wire.text("scope_filenames"),
            eligible: wire.text("eligible_files"), excluded: wire.text("excluded_files"),
            catalogued: Int(catalogued - pending), pathScoped: !request.pathScope.isWorkspace)
        missingScopeFiles = UInt64(try request.pathScope.validateRegionCoverage(
            catalogued: optional("scope_region_catalogued_files"), missing: optional("scope_region_missing_files"),
            eligible: admitted, searchCatalogFiles: Int(catalogued), allFileTypes: request.fileScope.isAll,
            complete: complete))
    }

    func follows(_ previous: Self) -> Bool {
        guard owner == previous.owner, generation == previous.generation, root == previous.root,
              request == previous.request, previous.revision < UInt64.max, revision == previous.revision + 1,
              previous.more, !previous.discoveryComplete || discoveryComplete else { return false }
        let variable = Set(["pending_catalogued_files", "files_unexamined", "retained_catalog_pages", "retained_catalog_files"])
        return Self.counterKeys.filter { !variable.contains($0) }.allSatisfy { counts[$0]! >= previous.counts[$0]! }
    }

    var coverage: String {
        let label = complete ? "Extraction processing complete" : (more ? "Discovery and extraction in progress" : "Partial extraction coverage")
        return "\(label) · \(matchesSeen) matching candidates in \(counts["matching_files"]!) files · "
            + "\(retainedHits) retained results · \(counts["files_examined"]!) of \(counts["eligible_files"]!) admitted files examined."
    }
    var limitations: String {
        var parts = ["Symbols are heuristic candidates; headings use the shared Markdown extractor. No compiler-complete claim is made."]
        if counts["incomplete_files"]! > 0 {
            parts.append("\(counts["incomplete_files"]!) incomplete files: \(counts["unsupported_files"]!) unsupported, "
                + "\(counts["oversized_files"]!) oversized, \(counts["unavailable_files"]!) unavailable, "
                + "\(counts["analysis_limited_files"]!) extraction-limited.")
        }
        if missingScopeFiles > 0 { parts.append("\(missingScopeFiles) requested visible files were not observed.") }
        if counts["structure_metadata_omitted"]! > 0 || counts["source_retention_omitted_files"]! > 0 {
            parts.append("Retention omitted \(counts["structure_metadata_omitted"]!) candidate labels and "
                + "\(counts["source_retention_omitted_files"]!) matching source captures; counts can exceed available rows.")
        }
        if let stopReason { parts.append("Stopped: \(stopReason).") }
        return parts.joined(separator: " ")
    }
}

struct AtlasWorkspaceStructurePage: Sendable {
    let id = UUID()
    let summary: AtlasWorkspaceStructureSummary
    let offset: UInt64
    let rows: [AtlasWorkspaceStructureHit]
    let nextOffset: UInt64?
    let diagnostics: [String]
    let diagnosticsTruncated: Bool

    static func decode(_ json: String?, command: String, owner: UInt64, generation: UInt64,
        root: [UInt8], request: AtlasWorkspaceStructureRequest, offset: UInt64 = 0,
        count: UInt64 = AtlasWorkspaceStructureLimits.page, acceptingCanonicalRoot: Bool = false) throws -> Self {
        do {
            guard count > 0, count <= AtlasWorkspaceStructureLimits.page else { throw AtlasWorkspaceStructureError.invalidInput }
            let wire = try AtlasWorkspaceStructureWire.read(json, owner: owner, command: command)
            let summary = try AtlasWorkspaceStructureSummary(wire, owner: owner, generation: generation, root: root,
                request: request, acceptingCanonicalRoot: acceptingCanonicalRoot)
            if acceptingCanonicalRoot {
                guard command == "info", offset == 0, summary.revision == 1, summary.retainedHits == 0,
                      summary.counts["payload_bytes_read"] == 0, summary.counts["read_calls"] == 0 else {
                    throw AtlasWorkspaceStructureError.invalidResponse
                }
            }
            guard try wire.number("page_offset") == offset, offset <= summary.retainedHits else {
                throw AtlasWorkspaceStructureError.invalidResponse
            }
            let rawRows = try wire.rows("hits", maximum: Int(count))
            let rows = try rawRows.map { try AtlasWorkspaceStructureHit($0, summary: summary) }
            guard try wire.number("page_count") == UInt64(rows.count),
                  UInt64(rows.count) == min(count, summary.retainedHits - offset) else {
                throw AtlasWorkspaceStructureError.invalidResponse
            }
            var lastOccurrence = request.startAfter
            for (position, row) in rows.enumerated() {
                guard row.index == offset + UInt64(position), row.occurrence > lastOccurrence else {
                    throw AtlasWorkspaceStructureError.invalidResponse
                }
                lastOccurrence = row.occurrence
            }
            let next = try wire.optionalNumber("next_offset", required: true)
            let end = offset + UInt64(rows.count)
            guard next == (end < summary.retainedHits ? end : nil) else { throw AtlasWorkspaceStructureError.invalidResponse }
            let diagnostics = try wire.rows("diagnostics", maximum: 32).map { row -> String in
                let file = try row.number("file_id"), code = try row.text("code")
                guard file > 0, !code.isEmpty, code.utf8.count <= 128 else { throw AtlasWorkspaceStructureError.invalidResponse }
                return "File \(file): \(String(reflecting: code))"
            }
            return Self(summary: summary, offset: offset, rows: rows, nextOffset: next, diagnostics: diagnostics,
                diagnosticsTruncated: try wire.flag("diagnostics_truncated"))
        } catch let error as AtlasWorkspaceStructureError { throw error }
        catch { throw AtlasWorkspaceStructureError.invalidResponse }
    }

    func target(_ row: AtlasWorkspaceStructureHit) -> AtlasWorkspaceStructureTarget? {
        guard rows.contains(row) else { return nil }
        return .init(page: id, owner: summary.owner, generation: summary.generation, revision: summary.revision, hit: row)
    }
    func admits(_ target: AtlasWorkspaceStructureTarget) -> Bool {
        target.page == id && target.owner == summary.owner && target.generation == summary.generation
            && target.revision == summary.revision && rows.contains(target.hit)
    }
}

struct AtlasWorkspaceStructureTarget: Sendable {
    let page: UUID
    let owner, generation, revision: UInt64
    let hit: AtlasWorkspaceStructureHit
}

/// Bounded JSON envelope validation only. No filesystem, structural parser or
/// alternate source decoder is implemented by this native presentation.
struct AtlasWorkspaceStructureWire: Decodable {
    struct Key: CodingKey {
        let stringValue: String
        var intValue: Int? { nil }
        init(_ value: String) { stringValue = value }
        init?(stringValue: String) { self.init(stringValue) }
        init?(intValue: Int) { return nil }
    }
    let fields: KeyedDecodingContainer<Key>
    init(from decoder: Decoder) throws { fields = try decoder.container(keyedBy: Key.self) }
    static func read(_ json: String?, owner: UInt64, command: String) throws -> Self {
        guard owner > 0, let json, !json.isEmpty, json.utf8.count <= AtlasWorkspaceStructureLimits.responseBytes else {
            throw AtlasWorkspaceStructureError.invalidResponse
        }
        do {
            let wire = try JSONDecoder().decode(Self.self, from: Data(json.utf8))
            guard try wire.text("schema") == "fcb.catalog-structure/1", try wire.number("owner") == owner else {
                throw AtlasWorkspaceStructureError.invalidResponse
            }
            if try wire.text("status") == "error" {
                let code = try wire.object("error").text("code")
                guard !code.isEmpty, code.utf8.count <= 512,
                      code.unicodeScalars.allSatisfy({ $0.value >= 32 && $0.value != 127 }) else {
                    throw AtlasWorkspaceStructureError.invalidResponse
                }
                throw AtlasWorkspaceStructureError.engine(code)
            }
            guard try wire.text("status") == "ok", try wire.text("command") == command else {
                throw AtlasWorkspaceStructureError.invalidResponse
            }
            return wire
        } catch let error as AtlasWorkspaceStructureError { throw error }
        catch { throw AtlasWorkspaceStructureError.invalidResponse }
    }
    func text(_ key: String) throws -> String { try fields.decode(String.self, forKey: Key(key)) }
    func flag(_ key: String) throws -> Bool { try fields.decode(Bool.self, forKey: Key(key)) }
    func number(_ key: String) throws -> UInt64 {
        let raw = try text(key)
        guard let number = UInt64(raw), String(number) == raw else { throw AtlasWorkspaceStructureError.invalidResponse }
        return number
    }
    func optionalText(_ key: String, required: Bool = false) throws -> String? {
        guard !required || fields.contains(Key(key)) else { throw AtlasWorkspaceStructureError.invalidResponse }
        return try fields.decodeIfPresent(String.self, forKey: Key(key))
    }
    func optionalNumber(_ key: String, required: Bool = false) throws -> UInt64? {
        guard let raw = try optionalText(key, required: required) else { return nil }
        guard let value = UInt64(raw), String(value) == raw else { throw AtlasWorkspaceStructureError.invalidResponse }
        return value
    }
    func requireNull(_ key: String) throws {
        guard fields.contains(Key(key)), try fields.decodeNil(forKey: Key(key)) else { throw AtlasWorkspaceStructureError.invalidResponse }
    }
    func object(_ key: String) throws -> Self { try fields.decode(Self.self, forKey: Key(key)) }
    func rows(_ key: String, maximum: Int) throws -> [Self] {
        var input = try fields.nestedUnkeyedContainer(forKey: Key(key))
        guard input.count.map({ $0 <= maximum }) ?? true else { throw AtlasWorkspaceStructureError.invalidResponse }
        var rows: [Self] = []
        while !input.isAtEnd {
            guard rows.count < maximum else { throw AtlasWorkspaceStructureError.invalidResponse }
            rows.append(try input.decode(Self.self))
        }
        return rows
    }
    static func hex(_ value: String, bytes: Int) -> Bool {
        value.utf8.count == bytes * 2 && value.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
    func path(maximumBytes: Int) throws -> [UInt8] {
        let encoded = try text("hex"), display = try text("display")
        guard try text("encoding") == "unix-bytes", !encoded.isEmpty, encoded.utf8.count % 2 == 0,
              encoded.utf8.count <= 2 * maximumBytes, display.utf8.count <= 65_536,
              Self.hex(encoded, bytes: encoded.utf8.count / 2) else { throw AtlasWorkspaceStructureError.invalidResponse }
        let bytes = Array(encoded.utf8)
        func nibble(_ byte: UInt8) -> UInt8 { byte <= 57 ? byte - 48 : byte - 87 }
        return stride(from: 0, to: bytes.count, by: 2).map { nibble(bytes[$0]) * 16 + nibble(bytes[$0 + 1]) }
    }
}

