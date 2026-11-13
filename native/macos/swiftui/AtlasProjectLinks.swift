import Foundation

enum AtlasProjectLinksError: Error, Sendable {
    case unavailable, invalidInput, invalidResponse, canceled, engine(String)
    var message: String {
        switch self {
        case .unavailable: return "Project relationships are unavailable. Capture the project again."
        case .invalidInput: return "Choose a current relationship or an exact relative project path."
        case .invalidResponse: return "The relationship response could not be verified. It is not an empty result."
        case .canceled: return "Relationship work was canceled."
        case .engine(let code):
            if ["ATLAS_LINKS_PATH_NOT_FOUND", "ATLAS_IMPORTS_PATH_NOT_FOUND"].contains(code) {
                return "That exact path is outside this captured atlas catalog. The previous results remain available; refresh to capture the current project."
            }
            return "Relationship work was refused (\(code))."
        }
    }
}


/// These are separate retained inventories with independent generations.
/// A lexical import candidate never acquires Markdown or compiler evidence.
enum AtlasProjectRelationshipProjection: String, CaseIterable, Identifiable, Sendable {
    case documentation, imports
    var id: String { rawValue }
    var label: String { self == .documentation ? "Documentation" : "Imports" }
    var noun: String { self == .documentation ? "documentation links" : "import candidates" }
    var schema: String { self == .documentation ? "fcb.atlas-links/1" : "fcb.atlas-imports/1" }
    func command(_ operation: String) -> String { "\(self == .documentation ? "links" : "imports")-\(operation)" }
    func directionLabel(_ direction: AtlasProjectLinkDirection) -> String {
        direction == .incoming && self == .imports ? "Imported by" : direction.label
    }
    var explanation: String {
        switch self {
        case .documentation:
            "Parsed Markdown destinations identify the whole source document. Link-token positions and compiler relationships are not established."
        case .imports:
            "Lexical import statements carry exact captured evidence. Catalog targets are navigation candidates; compiler resolution, build configuration and runtime dependencies are not established."
        }
    }
}

struct AtlasProjectLinkOperations: Sendable {
    let begin: @Sendable (UInt64, UInt64, UInt64, AtlasSearchCancellation) throws -> String?
    let step: @Sendable (UInt64, UInt64, AtlasSearchCancellation) throws -> String?
    let page: @Sendable (UInt64, UInt64, AtlasProjectLinkDirection, String?, UInt64, UInt64) throws -> String?
    let openReader: @Sendable (UInt64, UInt64, UInt64, UInt64, AtlasProjectLinkSide) throws -> String?
}

enum AtlasProjectLinkDirection: String, CaseIterable, Identifiable, Sendable {
    case all, outgoing, incoming
    var id: String { rawValue }
    var label: String { switch self { case .all: "All"; case .outgoing: "Outgoing"; case .incoming: "Backlinks" } }
    var code: UInt64 { switch self { case .all: 0; case .outgoing: 1; case .incoming: 2 } }
}
enum AtlasProjectLinkSide: String, Sendable {
    case evidence, target
    var code: UInt64 { self == .evidence ? 0 : 1 }
}

/// These functions operate only on the private prepared-index owner. The
/// existing index builder owns discovery and bounded original-source capture.
struct AtlasProjectLinksTransport: Sendable {
    let index: AtlasIndexedSearchTransport
    let readerDomain: UUID?
    let begin: @Sendable (UInt64, UInt64, UInt64, AtlasSearchCancellation) throws -> String?
    let step: @Sendable (UInt64, UInt64, AtlasSearchCancellation) throws -> String?
    let page: @Sendable (UInt64, UInt64, AtlasProjectLinkDirection, String?, UInt64, UInt64) throws -> String?
    let openReader: @Sendable (UInt64, UInt64, UInt64, UInt64, AtlasProjectLinkSide) throws -> String?
    var imports: AtlasProjectLinkOperations? = nil

    func operations(for projection: AtlasProjectRelationshipProjection) throws -> AtlasProjectLinkOperations {
        if projection == .imports {
            guard let imports else { throw AtlasProjectLinksError.unavailable }
            return imports
        }
        return AtlasProjectLinkOperations(begin: begin, step: step, page: page, openReader: openReader)
    }
}

struct AtlasProjectLinkPath: Sendable, Equatable {
    let bytes: [UInt8]
    let display: String
    var hex: String { bytes.map { String(format: "%02x", $0) }.joined() }
    init(bytes: [UInt8], display: String? = nil) throws {
        guard Self.valid(bytes) else { throw AtlasProjectLinksError.invalidInput }
        self.bytes = bytes
        self.display = display ?? String(bytes: bytes, encoding: .utf8) ?? bytes.map { String(format: "\\x%02x", $0) }.joined()
    }
    init(_ wire: AtlasIndexWire) throws {
        let hex = try wire.text("hex"), display = try wire.text("display")
        guard try wire.text("encoding") == "unix-bytes", display.utf8.count <= 16_384, !hex.isEmpty, hex.utf8.count <= 8192,
              hex.utf8.count.isMultiple(of: 2) else { throw AtlasProjectLinksError.invalidResponse }
        let characters = Array(hex.utf8)
        func nibble(_ value: UInt8) throws -> UInt8 {
            switch value { case 48...57: value - 48; case 97...102: value - 87; default: throw AtlasProjectLinksError.invalidResponse }
        }
        var bytes: [UInt8] = []; bytes.reserveCapacity(characters.count / 2)
        for i in stride(from: 0, to: characters.count, by: 2) { bytes.append(try nibble(characters[i]) * 16 + nibble(characters[i + 1])) }
        guard Self.valid(bytes) else { throw AtlasProjectLinksError.invalidResponse }
        self.bytes = bytes; self.display = display
    }
    static func valid(_ bytes: [UInt8]) -> Bool {
        !bytes.isEmpty && bytes.count <= 4096 && bytes.first != 47 && !bytes.contains(0)
            && !bytes.split(separator: 47, omittingEmptySubsequences: false).contains {
                $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
            }
    }
    static func == (a: Self, b: Self) -> Bool { a.bytes == b.bytes && a.display.utf8.elementsEqual(b.display.utf8) }
}

struct AtlasProjectLinkFile: Identifiable, Sendable, Equatable {
    let id: UInt64
    var file: UInt64 { id }
    let revision: UInt64
    let path: AtlasProjectLinkPath
    let capturedBytes: UInt64
    let digest: String
}

struct AtlasProjectLinkRange: Equatable, Sendable {
    let start, end: UInt64
    init(_ w: AtlasIndexWire) throws {
        start = try w.number("start"); end = try w.number("end")
        guard start <= end else { throw AtlasProjectLinksError.invalidResponse }
    }
}

/// Native validation of engine evidence, never native source parsing or resolution.
struct AtlasProjectImportEvidence: Equatable, Sendable {
    let language, kind, resolutionBasis: String

    static func validateRoot(_ w: AtlasIndexWire) throws {
        guard try w.text("projection") == "lexical-source-imports",
              try w.text("relationship_kind") == "lexical-import",
              try w.text("evidence_precision") == "exact-import-source-range",
              try w.text("extractor") == "franken-markdown-d6fbd1d5/import-statements-v2",
              try w.text("resolver") == "fcb-closed-catalog-imports-v2",
              try w.text("extraction_policy") == "rust-mod-use-js-static-and-python-import-v2",
              try !w.flag("compiler_resolved"), try !w.flag("semantic_complete") else {
            throw AtlasProjectLinksError.invalidResponse
        }
    }
    init(_ w: AtlasIndexWire) throws {
        language = try w.text("language"); kind = try w.text("lexical_import_kind")
        resolutionBasis = try w.text("resolution_basis")
        let kinds: [String]
        switch language {
        case "rust": kinds = ["rust-module", "rust-use"]
        case "python": kinds = ["python-import", "python-from"]
        case "javascript", "typescript", "jsx", "tsx": kinds = ["javascript-import", "javascript-export"]
        default: throw AtlasProjectLinksError.invalidResponse
        }
        guard kinds.contains(kind),
              ["exact-relative-path", "unique-relative-module-candidate", "rust-module-file-candidate",
               "python-relative-module-candidate", "unresolved-rust-use", "external-specifier",
               "unsupported-specifier", "no-catalog-target", "ambiguous-catalog-target"].contains(resolutionBasis) else {
            throw AtlasProjectLinksError.invalidResponse
        }
        if resolutionBasis == "python-relative-module-candidate" && (language != "python" || kind != "python-from") {
            throw AtlasProjectLinksError.invalidResponse
        }
        if language == "python" && !["python-relative-module-candidate", "external-specifier",
            "unsupported-specifier", "no-catalog-target", "ambiguous-catalog-target"].contains(resolutionBasis) {
            throw AtlasProjectLinksError.invalidResponse
        }
    }
    var label: String {
        let statement: String
        switch kind {
        case "rust-module": statement = "Rust module declaration"
        case "rust-use": statement = "Rust use statement"
        case "python-import": statement = "Python import statement"
        case "python-from": statement = "Python from-import statement"
        case "javascript-export": statement = "Re-export statement"
        default: statement = "Import statement"
        }
        return "\(statement) · \(language) · lexical evidence"
    }
}

struct AtlasProjectLink: Identifiable, Sendable, Equatable, Decodable {
    let id: UInt64
    let projection: AtlasProjectRelationshipProjection
    let source: AtlasProjectLinkFile
    let target: AtlasProjectLinkFile?
    let targetPath: AtlasProjectLinkPath?
    let targetFile: UInt64?
    let sourceLink: UInt64
    let block: UInt64?
    let evidenceRange: AtlasProjectLinkRange
    let importEvidence: AtlasProjectImportEvidence?
    let destination, resolution, destinationSuffix, fragmentResolution: String
    var label: String {
        let status: String
        switch resolution {
        case "captured": status = projection == .imports ? "Captured catalog target candidate" : "Exact target capture available"
        case "unavailable": status = "Target capture unavailable"
        case "uncaptured": status = "Target is catalogued but was not captured"
        case "outside-capture-scope": status = "Target is outside the retained capture scope"
        case "missing": status = "Target is absent from this captured catalog"
        case "ambiguous": status = "Several catalog targets are possible; no target was selected"
        case "external": status = projection == .imports ? "External specifier; no package resolution or network content" : "External destination; no network content was fetched"
        default: status = projection == .imports ? "No supported catalog target was established" : "Destination could not be resolved within the project"
        }
        return status + (projection == .documentation && destinationSuffix.contains("#")
            ? " · fragment is checked when the target opens" : "")
    }
    var canOpenTarget: Bool { resolution == "captured" && target != nil }
    var evidenceLabel: String {
        guard let importEvidence else {
            return "Parsed Markdown link · whole captured document evidence; no exact link span or target navigation"
        }
        return "\(importEvidence.label) · original bytes \(evidenceRange.start)..<\(evidenceRange.end) · not compiler resolved"
    }

    init(from decoder: Decoder) throws {
        try self.init(AtlasIndexWire(from: decoder), projection: .documentation)
    }

    init(_ w: AtlasIndexWire, projection: AtlasProjectRelationshipProjection) throws {
        self.projection = projection
        id = try w.number("link_id")
        let file = try w.number("source_file_id"), revision = try w.number("source_revision")
        let bytes = try w.number("source_bytes"), digest = try w.text("source_sha256")
        let path = try AtlasProjectLinkPath(w.object("source_path"))
        evidenceRange = try AtlasProjectLinkRange(w.object("evidence_range"))
        sourceLink = try w.number("source_link_id")
        destination = try w.text("destination"); resolution = try w.text("resolution")
        if projection == .documentation {
            block = try w.number("block_index")
            destinationSuffix = try w.text("destination_suffix"); fragmentResolution = try w.text("fragment_resolution")
            importEvidence = nil
            guard bytes <= 262_144, evidenceRange.start == 0, evidenceRange.end == bytes,
                  destinationSuffix.utf8.count <= 4096,
                  fragmentResolution == (destinationSuffix.isEmpty ? "none" : "unqualified"),
                  try w.absent(["language", "lexical_import_kind", "resolution_basis"]),
                  resolution != "ambiguous" else { throw AtlasProjectLinksError.invalidResponse }
        } else {
            block = nil; destinationSuffix = ""; fragmentResolution = "none"
            importEvidence = try AtlasProjectImportEvidence(w)
            guard bytes <= 131_072, evidenceRange.start < evidenceRange.end, evidenceRange.end <= bytes,
                  !destination.isEmpty,
                  try w.absent(["block_index", "destination_suffix", "fragment_resolution"]) else {
                throw AtlasProjectLinksError.invalidResponse
            }
        }
        guard id > 0, id <= 4096, file > 0, revision > 0, sourceLink > 0,
              AtlasProjectLinksWire.digest(digest), destination.utf8.count <= 4096,
              ["captured", "unavailable", "uncaptured", "outside-capture-scope", "missing", "ambiguous", "external", "unresolved"].contains(resolution) else {
            throw AtlasProjectLinksError.invalidResponse
        }
        source = AtlasProjectLinkFile(id: file, revision: revision, path: path, capturedBytes: bytes, digest: digest)
        targetFile = try w.optionalNumber("target_file_id")
        if let targetFile {
            guard targetFile > 0 else { throw AtlasProjectLinksError.invalidResponse }
            targetPath = try AtlasProjectLinkPath(w.object("target_path"))
        } else {
            guard try w.absent(["target_path"]) else { throw AtlasProjectLinksError.invalidResponse }
            targetPath = nil
        }
        if resolution == "captured" {
            guard let targetFile, let targetPath,
                  let revision = try w.optionalNumber("target_revision"), revision > 0,
                  let digest = try w.optionalText("target_sha256"), AtlasProjectLinksWire.digest(digest),
                  let bytes = try w.optionalNumber("target_bytes"), bytes <= 1024 * 1024 else {
                throw AtlasProjectLinksError.invalidResponse
            }
            target = AtlasProjectLinkFile(id: targetFile, revision: revision, path: targetPath, capturedBytes: bytes, digest: digest)
        } else {
            guard try w.absent(["target_revision", "target_sha256", "target_bytes"]),
                  ["unavailable", "uncaptured", "outside-capture-scope"].contains(resolution) == (targetFile != nil) else {
                throw AtlasProjectLinksError.invalidResponse
            }
            target = nil
        }
        if let importEvidence {
            let allowed: [String]
            switch resolution {
            case "captured", "unavailable", "uncaptured", "outside-capture-scope":
                allowed = ["exact-relative-path", "unique-relative-module-candidate", "rust-module-file-candidate",
                    "python-relative-module-candidate"]
            case "missing": allowed = ["no-catalog-target"]
            case "ambiguous": allowed = ["ambiguous-catalog-target"]
            case "external": allowed = ["external-specifier"]
            default: allowed = ["unresolved-rust-use", "unsupported-specifier"]
            }
            guard allowed.contains(importEvidence.resolutionBasis) else { throw AtlasProjectLinksError.invalidResponse }
        }
    }
    static func == (a: Self, b: Self) -> Bool {
        a.id == b.id && a.projection == b.projection && a.source == b.source && a.target == b.target
            && a.targetPath == b.targetPath && a.targetFile == b.targetFile && a.sourceLink == b.sourceLink
            && a.block == b.block && a.evidenceRange == b.evidenceRange && a.importEvidence == b.importEvidence
            && a.destination.utf8.elementsEqual(b.destination.utf8) && a.resolution == b.resolution
            && a.destinationSuffix.utf8.elementsEqual(b.destinationSuffix.utf8) && a.fragmentResolution == b.fragmentResolution
    }
}

private struct AtlasProjectImportRow: Decodable {
    let link: AtlasProjectLink
    init(from decoder: Decoder) throws {
        link = try AtlasProjectLink(AtlasIndexWire(from: decoder), projection: .imports)
    }
}

struct AtlasProjectLinksCoverage: Equatable, Sendable {
    let projection: AtlasProjectRelationshipProjection
    let files, captured, captureUnavailable, outsideScope, uncaptured: Int
    let examined, pending, scanned, unsupported, sourceLimited: Int
    let diagnostics, linksSeen, retained, steps, lastStepFiles: UInt64
    let parseBytes, metadataBytes, hashedBytes: UInt64
    let building, complete, discoveryComplete, captureRegionComplete, truncated: Bool
    let stop: String?

    init(_ w: AtlasIndexWire, basis: AtlasIndexedBasis,
         projection: AtlasProjectRelationshipProjection = .documentation) throws {
        self.projection = projection
        let population = projection == .documentation ? "markdown" : "source"
        let terminal = projection == .documentation ? "all-markdown-examined" : "all-source-files-examined"
        files = try w.count("\(population)_files", upTo: basis.files)
        captured = try w.count("captured_\(population)_files", upTo: basis.capturedFiles)
        captureUnavailable = try w.count("capture_unavailable_\(population)_files", upTo: basis.unavailable)
        outsideScope = try w.count("outside_capture_scope_\(population)_files", upTo: basis.files)
        uncaptured = try w.count("uncaptured_\(population)_files", upTo: basis.files)
        examined = try w.count("examined_\(population)_files", upTo: captured)
        pending = try w.count("pending_\(population)_files", upTo: captured)
        scanned = try w.count("scanned_\(population)_files", upTo: examined)
        unsupported = try w.count("unsupported_\(population)_files", upTo: examined)
        sourceLimited = try w.count("source_limited_\(population)_files", upTo: examined)
        diagnostics = try w.number("parser_diagnostics"); linksSeen = try w.number("links_seen")
        retained = try w.number("retained_links"); steps = try w.number("steps")
        lastStepFiles = try w.number("last_step_files"); parseBytes = try w.number("parsed_source_bytes")
        metadataBytes = try w.number("metadata_bytes"); hashedBytes = try w.number("hashed_source_bytes")
        building = try w.flag("relationships_in_progress"); complete = try w.flag("relationships_complete")
        captureRegionComplete = try w.flag("capture_region_complete")
        discoveryComplete = try w.flag("discovery_complete"); truncated = try w.flag("truncated")
        stop = try w.optionalText("stop_reason")
        if projection == .documentation {
            guard try w.text("evidence_precision") == "whole-captured-markdown-document",
                  try w.text("relationship_kind") == "parsed-markdown-link",
                  try w.text("resolution_domain") == "exact-catalog-relative-path",
                  try w.number("max_markdown_source_bytes") == 262_144 else { throw AtlasProjectLinksError.invalidResponse }
        } else {
            try AtlasProjectImportEvidence.validateRoot(w)
            guard try w.number("max_source_bytes") == 131_072 else { throw AtlasProjectLinksError.invalidResponse }
        }
        guard try w.text("source_observation") == "retained-index-captures",
              try w.number("max_links") == 4096, try w.number("max_parse_bytes") == 32 * 1024 * 1024,
              try w.number("max_metadata_bytes") == 8 * 1024 * 1024, metadataBytes <= 8 * 1024 * 1024,
              try w.number("source_bytes_read") == 0, try w.number("read_calls") == 0,
              files == captured + captureUnavailable + outsideScope + uncaptured,
              examined == scanned + unsupported + sourceLimited, captured == examined + pending,
              retained <= 4096, retained <= linksSeen, steps <= 4097, lastStepFiles <= 1,
              parseBytes <= 32 * 1024 * 1024, parseBytes <= basis.sourceBytes, hashedBytes <= basis.sourceBytes,
              captureRegionComplete == (basis.captureScope.visibleMissingFiles == 0),
              discoveryComplete == basis.discoveryComplete, building == (stop == nil),
              stop == nil || [terminal, "parse-byte-limit"].contains(stop!) else { throw AtlasProjectLinksError.invalidResponse }
        if complete {
            guard !building, stop == terminal, pending == 0, discoveryComplete, captureRegionComplete,
                  basis.captureScope.visibleMissingFiles == 0, captureUnavailable == 0, outsideScope == 0,
                  uncaptured == 0, unsupported == 0, sourceLimited == 0, diagnostics == 0,
                  !truncated, retained == linksSeen else { throw AtlasProjectLinksError.invalidResponse }
        }
    }
    func validateSuccessor(_ next: Self) throws {
        guard building, next.projection == projection, next.steps == steps + 1,
              next.files == files, next.captured == captured, next.captureUnavailable == captureUnavailable,
              next.outsideScope == outsideScope, next.uncaptured == uncaptured,
              next.examined >= examined, next.examined <= examined + 1,
              next.lastStepFiles == UInt64(next.examined - examined),
              next.scanned >= scanned, next.unsupported >= unsupported, next.sourceLimited >= sourceLimited,
              next.diagnostics >= diagnostics, next.linksSeen >= linksSeen, next.retained >= retained,
              next.parseBytes >= parseBytes, next.metadataBytes >= metadataBytes,
              next.hashedBytes >= hashedBytes, next.captureRegionComplete == captureRegionComplete,
              !truncated || next.truncated else { throw AtlasProjectLinksError.invalidResponse }
    }
    var summary: String {
        let sourceName = projection == .documentation ? "Markdown" : "eligible source"
        if building { return "Building \(projection.noun): \(examined) of \(captured) captured \(sourceName) files examined; \(retained) retained." }
        let status = complete ? "Complete within the captured \(sourceName) inventory" : "Partial \(projection.label.lowercased()) coverage"
        var parts = ["\(status): \(retained) \(projection.noun) from \(scanned) files."]
        if captureUnavailable + uncaptured + outsideScope > 0 {
            parts.append("\(captureUnavailable) unavailable, \(uncaptured) uncaptured and \(outsideScope) outside the capture scope.")
        }
        if unsupported + sourceLimited + pending > 0 {
            parts.append("\(unsupported) unsupported, \(sourceLimited) beyond the source limit and \(pending) pending.")
        }
        if diagnostics > 0 { parts.append("\(diagnostics) extraction diagnostics.") }
        if truncated { parts.append("Relationship metadata limits were reached; additional records were omitted.") }
        if !discoveryComplete { parts.append("Project discovery was incomplete.") }
        if projection == .imports { parts.append("Lexical coverage does not establish compiler or semantic completeness.") }
        return parts.joined(separator: " ")
    }
}

struct AtlasProjectLinksPage: Identifiable, Sendable {
    let id = UUID()
    let basis: AtlasIndexedBasis
    let generation: UInt64
    let coverage: AtlasProjectLinksCoverage
    let direction: AtlasProjectLinkDirection
    let path: AtlasProjectLinkPath?
    let filterFile: UInt64?
    let start, limit, total: UInt64
    let links: [AtlasProjectLink]
    let next: UInt64?
    var summary: String { coverage.summary }
    var complete: Bool { coverage.complete }
    var building: Bool { coverage.building }
    var projection: AtlasProjectRelationshipProjection { coverage.projection }

    static func decode(_ json: String?, index: AtlasPreparedIndex, generation: UInt64,
        command: String, direction: AtlasProjectLinkDirection = .all, path: [UInt8]? = nil,
        start: UInt64 = 0, limit: UInt64 = 64,
        projection: AtlasProjectRelationshipProjection = .documentation) throws -> Self {
        guard generation > 0, limit > 0, limit <= 128, direction == .all || path != nil,
              path == nil || AtlasProjectLinkPath.valid(path!) else { throw AtlasProjectLinksError.invalidInput }
        let w = try AtlasProjectLinksWire.decode(json, projection: projection)
        try AtlasProjectLinksWire.identity(w, basis: index.basis, generation: generation, command: command, projection: projection)
        let coverage = try AtlasProjectLinksCoverage(w, basis: index.basis, projection: projection)
        guard try w.text("direction") == direction.rawValue, try w.number("offset") == start,
              try w.number("limit") == limit else { throw AtlasProjectLinksError.invalidResponse }
        let filterFile = try w.optionalNumber("filter_file_id")
        let selected: AtlasProjectLinkPath?
        if let path {
            selected = try AtlasProjectLinkPath(w.object("filter_path"))
            guard selected?.bytes == path, let filterFile, filterFile > 0, filterFile <= UInt64(index.basis.files) else {
                throw AtlasProjectLinksError.invalidResponse
            }
        } else {
            guard filterFile == nil, try w.absent(["filter_path"]) else { throw AtlasProjectLinksError.invalidResponse }
            selected = nil
        }
        let total = try w.number("matching_links"), next = try w.optionalNumber("next_offset")
        guard total <= coverage.retained, start <= total,
              path != nil || total == coverage.retained else { throw AtlasProjectLinksError.invalidResponse }
        let links: [AtlasProjectLink]
        if projection == .documentation {
            links = try w.rows("links", limit: Int(limit))
        } else {
            let rows: [AtlasProjectImportRow] = try w.rows("links", limit: Int(limit))
            links = rows.map(\.link)
        }
        let end = start + UInt64(links.count)
        guard UInt64(links.count) <= min(limit, total - start),
              start == total ? links.isEmpty : !links.isEmpty,
              next == (end < total ? end : nil) else { throw AtlasProjectLinksError.invalidResponse }
        var previous: UInt64 = 0
        var files: [UInt64: AtlasProjectLinkFile] = [:]
        var paths: [UInt64: [UInt8]] = [:]
        var resolutions: [UInt64: String] = [:]
        for (offset, link) in links.enumerated() {
            guard link.projection == projection, link.id > previous, link.id <= coverage.retained,
                  path != nil || link.id == start + UInt64(offset) + 1,
                  link.source.id <= UInt64(index.basis.files), link.source.revision == index.basis.index,
                  link.targetFile.map({ $0 <= UInt64(index.basis.files) }) != false,
                  link.target.map({ $0.revision == index.basis.index }) != false else { throw AtlasProjectLinksError.invalidResponse }
            previous = link.id
            for (file, rawPath, status) in [(link.source.id, link.source.path.bytes, "captured")]
                + (link.targetFile.map { [($0, link.targetPath!.bytes, link.resolution)] } ?? []) {
                if let old = paths[file], old != rawPath { throw AtlasProjectLinksError.invalidResponse }
                if let old = resolutions[file], old != status { throw AtlasProjectLinksError.invalidResponse }
                paths[file] = rawPath; resolutions[file] = status
            }
            if let path {
                let source = link.source.path.bytes == path, target = link.targetPath?.bytes == path
                guard direction == .outgoing ? source : (direction == .incoming ? target : source || target) else {
                    throw AtlasProjectLinksError.invalidResponse
                }
                if source, link.source.id != filterFile { throw AtlasProjectLinksError.invalidResponse }
                if target, link.targetFile != filterFile { throw AtlasProjectLinksError.invalidResponse }
            }
            for file in [link.source, link.target].compactMap({ $0 }) {
                if let old = files[file.id], old != file { throw AtlasProjectLinksError.invalidResponse }
                files[file.id] = file
            }
        }
        return Self(basis: index.basis, generation: generation, coverage: coverage, direction: direction,
            path: selected, filterFile: filterFile, start: start, limit: limit, total: total, links: links, next: next)
    }
}

enum AtlasProjectLinksWire {
    static func digest(_ text: String) -> Bool {
        text.utf8.count == 64 && text.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
    static func decode(_ json: String?, projection: AtlasProjectRelationshipProjection = .documentation) throws -> AtlasIndexWire {
        guard let json else { throw AtlasProjectLinksError.unavailable }
        do {
            let w = try AtlasIndexWire.decode(json)
            if try w.text("status") == "error" {
                let schema = try w.text("schema"), code = try w.object("error").text("code")
                guard [projection.schema, "fcb.atlas-session/1"].contains(schema), !code.isEmpty,
                      code.utf8.count <= 128, code.utf8.allSatisfy({ (65...90).contains($0) || (48...57).contains($0) || $0 == 95 }) else {
                    throw AtlasProjectLinksError.invalidResponse
                }
                throw AtlasProjectLinksError.engine(code)
            }
            return w
        } catch let error as AtlasProjectLinksError { throw error }
        catch { throw AtlasProjectLinksError.invalidResponse }
    }
    static func identity(_ w: AtlasIndexWire, basis: AtlasIndexedBasis, generation: UInt64, command: String,
        projection: AtlasProjectRelationshipProjection = .documentation) throws {
        try w.header(schema: projection.schema, command: command, owner: basis.owner)
        guard try w.number("source_manifest") == basis.manifest, try w.number("layout_revision") == basis.layout,
              try w.number("index_generation") == basis.index, try w.number("capture_manifest") == basis.captureManifest,
              try w.number("relationship_generation") == generation else { throw AtlasProjectLinksError.invalidResponse }
    }
}
