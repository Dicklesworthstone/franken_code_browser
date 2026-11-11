import Foundation

private typealias SearchPoll = @convention(c) (UnsafeMutableRawPointer?) -> Int32
@_silgen_name("fcb_atlas_create") private func createSearchAtlas() -> UInt64
@_silgen_name("fcb_atlas_open_cancelable")
private func openSearchAtlas(_ handle: UInt64, _ root: UnsafePointer<CChar>?, _ maxFiles: UInt64,
    _ poll: SearchPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_search_begin")
private func beginSearch(_ handle: UInt64, _ generation: UInt64, _ query: UnsafePointer<CChar>?,
    _ maxMatches: UInt64, _ maxFiles: UInt64, _ maxFileBytes: UInt64, _ maxSourceBytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_search_begin_scoped")
private func beginScopedSearch(_ handle: UInt64, _ generation: UInt64, _ query: UnsafePointer<CChar>?,
    _ extensions: UnsafePointer<CChar>?, _ filenames: UnsafePointer<CChar>?,
    _ maxMatches: UInt64, _ maxFiles: UInt64, _ maxFileBytes: UInt64, _ maxSourceBytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_search_begin_path_scoped")
private func beginPathScopedSearch(_ handle: UInt64, _ generation: UInt64, _ query: UnsafePointer<CChar>?,
    _ extensions: UnsafePointer<CChar>?, _ filenames: UnsafePointer<CChar>?,
    _ pathKind: UInt64, _ path: UnsafePointer<CChar>?,
    _ maxMatches: UInt64, _ maxFiles: UInt64, _ maxFileBytes: UInt64, _ maxSourceBytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_search_begin_matching")
private func beginMatchingSearch(_ handle: UInt64, _ generation: UInt64, _ query: UnsafePointer<CChar>?,
    _ extensions: UnsafePointer<CChar>?, _ filenames: UnsafePointer<CChar>?,
    _ pathKind: UInt64, _ path: UnsafePointer<CChar>?, _ caseSensitive: UInt64,
    _ maxMatches: UInt64, _ maxFiles: UInt64, _ maxFileBytes: UInt64, _ maxSourceBytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_search_begin_counted_matching")
private func beginCountedMatchingSearch(_ handle: UInt64, _ generation: UInt64, _ query: UnsafePointer<CChar>?,
    _ extensions: UnsafePointer<CChar>?, _ filenames: UnsafePointer<CChar>?,
    _ pathKind: UInt64, _ path: UnsafePointer<CChar>?, _ caseSensitive: UInt64,
    _ maxMatches: UInt64, _ maxFiles: UInt64, _ maxFileBytes: UInt64, _ maxSourceBytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_search_begin_expression")
private func beginExpressionSearch(_ handle: UInt64, _ generation: UInt64, _ query: UnsafePointer<CChar>?,
    _ extensions: UnsafePointer<CChar>?, _ filenames: UnsafePointer<CChar>?,
    _ pathKind: UInt64, _ path: UnsafePointer<CChar>?, _ caseSensitive: UInt64,
    _ maxMatches: UInt64, _ maxFiles: UInt64, _ maxFileBytes: UInt64,
    _ maxSourceBytes: UInt64, _ maxScanBytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_structure_begin")
private func beginStructureSearch(_ handle: UInt64, _ generation: UInt64, _ mode: UInt64, _ nameMode: UInt64,
    _ query: UnsafePointer<CChar>?, _ extensions: UnsafePointer<CChar>?, _ filenames: UnsafePointer<CChar>?,
    _ pathKind: UInt64, _ path: UnsafePointer<CChar>?,
    _ maxMatches: UInt64, _ maxFiles: UInt64, _ maxFileBytes: UInt64, _ maxSourceBytes: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_search_step_cancelable")
private func stepSearch(_ handle: UInt64, _ generation: UInt64,
    _ poll: SearchPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_search_page")
private func pageSearch(_ handle: UInt64, _ generation: UInt64, _ start: UInt64, _ limit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_search_open_reader")
private func importSearchReader(_ atlas: UInt64, _ reader: UInt64, _ generation: UInt64, _ hit: UInt64) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_close") private func closeSearchAtlas(_ handle: UInt64) -> UInt8
@_silgen_name("fcb_free_string") private func releaseSearchString(_ pointer: UnsafeMutablePointer<CChar>?)

/// Scheduling/marshaling only. The existing Rust atlas engine discovers once
/// per query, captures/verifies at most one file per step, and retains matching
/// captures for all pages. No native directory walk, matcher or source decoder.
/// A fresh handle preserves explicit-search freshness; this is NOT a persistent
/// index or an atomic workspace observation. Close/destruction stays on worker.
enum AtlasNativeSearch {
    static func run(root: String, query: String,
                    cancellation: AtlasSearchCancellation) throws -> AtlasSearchReport {
        try runProgressive(root: root, query: query, cancellation: cancellation, progress: { _ in })
    }

    static func runProgressive(root: String, query: String, cancellation: AtlasSearchCancellation,
        progress: @Sendable (AtlasSearchReport) -> Void) throws -> AtlasSearchReport {
        try runWithAccess(root: root, query: query, cancellation: cancellation,
            access: AtlasSearchAccessLease(nil), progress: progress)
    }

    /// The request's grant is retained before any foreign work, including early
    /// failures. Finished/provisional reports pin the actual source session.
    static func runWithAccess(root: String, query: String, cancellation: AtlasSearchCancellation,
        access: AtlasSearchAccessLease, progress: @Sendable (AtlasSearchReport) -> Void) throws -> AtlasSearchReport {
        try runScoped(input: AtlasSearchInput(root: root, query: query), cancellation: cancellation,
            access: access, progress: progress)
    }

    static func runScoped(input: AtlasSearchInput, cancellation: AtlasSearchCancellation,
        access: AtlasSearchAccessLease, progress: @Sendable (AtlasSearchReport) -> Void) throws -> AtlasSearchReport {
        let root = input.root, query = input.query, scope = input.fileScope, pathScope = input.pathScope
        if cancellation.isCanceled { throw AtlasSearchError.canceled }
        let handle = createSearchAtlas()
        guard handle != 0 else { throw AtlasSearchError.unavailable }
        let owner = try AtlasSearchCaptureOwner(handle: handle,
            close: { closeSearchAtlas($0) != 0 }, retirementFailed: {
                FileHandle.standardError.write(Data("ATLAS_SEARCH_RETIREMENT_FAILED\n".utf8))
            })
        owner.retainAccess(access)
        let poll: SearchPoll = { context in
            guard let context else { return 1 }
            return Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().isCanceled ? 1 : 0
        }
        return try withExtendedLifetime(cancellation) {
            let context = Unmanaged.passUnretained(cancellation).toOpaque()
            let metadataLimit: UInt64 = pathScope.isVisible ? UInt64(AtlasSearchPathScope.maximumVisibleFiles) : 4096
            let opened: SearchOpen = try read(root.withCString {
                rootPointer in owner.call { openSearchAtlas($0, rootPointer, metadataLimit, poll, context) }
            }, cancellation)
            guard opened.status == "ok" else { throw AtlasSearchError.unavailable }
            var page: SearchPage = try read(query.withCString {
                queryPointer in scope.withCStrings { extensions, filenames in
                    pathScope.withCString { kind, path in
                        owner.call { handle in
                            if input.mode == .expression {
                                return beginExpressionSearch(handle, 1, queryPointer, extensions, filenames,
                                    kind, path, input.caseSensitive ? 1 : 0, 1000, 4096,
                                    1024 * 1024, 32 * 1024 * 1024, AtlasSearchExpression.scanLimit)
                            }
                            if !input.mode.isTextual {
                                return beginStructureSearch(handle, 1, input.mode.code, input.nameMode.code,
                                    queryPointer, extensions, filenames, kind, path,
                                    1000, 4096, 1024 * 1024, 32 * 1024 * 1024)
                            }
                            if !input.caseSensitive {
                                return beginMatchingSearch(handle, 1, queryPointer, extensions, filenames,
                                    kind, path, 0, 1000, 4096, 1024 * 1024, 32 * 1024 * 1024)
                            }
                            if input.mode == .text {
                                return beginCountedMatchingSearch(handle, 1, queryPointer, extensions, filenames,
                                    kind, path, 1, 1000, 4096, 1024 * 1024, 32 * 1024 * 1024)
                            }
                            if !pathScope.isWorkspace {
                                return beginPathScopedSearch(handle, 1, queryPointer, extensions, filenames,
                                    kind, path, 1000, 4096, 1024 * 1024, 32 * 1024 * 1024)
                            }
                            return scope.isAll
                                ? beginSearch(handle, 1, queryPointer, 1000, 4096, 1024 * 1024, 32 * 1024 * 1024)
                                : beginScopedSearch(handle, 1, queryPointer, extensions, filenames, 1000, 4096, 1024 * 1024, 32 * 1024 * 1024)
                        }
                    }
                }
            }, cancellation)
            var stream = SearchStream(query: query, scope: scope, pathScope: pathScope,
                mode: input.mode, nameMode: input.nameMode, caseSensitive: input.caseSensitive)
            var lastPublished: TimeInterval? = nil
            var publishedHits = 0
            while true {
                try stream.advance(page)
                try stream.append(page, start: 0, limit: 64)
                while stream.count < stream.retained {
                    let offset = stream.count
                    let tail: SearchPage = try read(owner.call { pageSearch($0, 1, UInt64(offset), 128) }, cancellation)
                    try stream.append(tail, start: offset, limit: 128)
                }
                let report = try stream.report(owner: owner, root: root)
                if cancellation.isCanceled { throw AtlasSearchError.canceled }
                if !report.isInProgress { return report }
                let now = ProcessInfo.processInfo.systemUptime
                // Deliver first hits promptly; otherwise bound overlay/UI work.
                // The coordinator additionally coalesces when the UI is busy.
                if lastPublished == nil || (publishedHits == 0 && stream.count > 0)
                    || now - (lastPublished ?? now) >= 0.1 {
                    progress(report)
                    lastPublished = now
                    publishedHits = stream.count
                }
                if cancellation.isCanceled { throw AtlasSearchError.canceled }
                page = try read(owner.call { stepSearch($0, 1, poll, context) }, cancellation)
            }
        }
    }

    fileprivate static func importReader(atlas: UInt64, reader: UInt64, generation: UInt64, hit: UInt64) throws -> String {
        let pointer = importSearchReader(atlas, reader, generation, hit)
        defer { releaseSearchString(pointer) }
        guard let pointer else { throw AtlasSearchError.unavailable }
        let length = strnlen(pointer, 4 * 1024 * 1024 + 1)
        guard length <= 4 * 1024 * 1024,
              let json = String(data: Data(bytes: pointer, count: length), encoding: .utf8) else {
            throw AtlasSearchError.invalidResponse
        }
        return json
    }

    private static func read<T: Decodable>(_ pointer: UnsafeMutablePointer<CChar>?,
        _ cancellation: AtlasSearchCancellation) throws -> T {
        defer { releaseSearchString(pointer) }
        if cancellation.isCanceled { throw AtlasSearchError.canceled }
        guard let pointer else { throw AtlasSearchError.unavailable }
        // The C ABI guarantees a valid NUL-terminated allocation. Bound copies
        // before Data/JSON decoding, including unknown fields in an envelope.
        let length = strnlen(pointer, 16 * 1024 * 1024 + 1)
        guard length <= 16 * 1024 * 1024 else { throw AtlasSearchError.invalidResponse }
        do {
            let data = Data(bytes: pointer, count: length)
            if let failure = AtlasSearchExpression.queryFailure(in: data)
                ?? AtlasSearchOccurrencePolicy.queryFailure(in: data) { throw failure }
            let result = try JSONDecoder().decode(T.self, from: data)
            if cancellation.isCanceled { throw AtlasSearchError.canceled }
            return result
        } catch let error as AtlasSearchError { throw error }
        catch { throw AtlasSearchError.invalidResponse }
    }
}

private struct SearchOpen: Decodable { let status: String }
private struct SearchPage: Decodable {
    let schema, status, command, owner, source_manifest, layout_revision, query_generation: String
    let needle, mode, search_strategy: String
    let case_sensitive: Bool?
    let requested_mode, realized_mode, name_mode, name_case, evidence_level, completeness_basis: String?
    let semantic_complete: Bool?
    let includes_comments_and_literals, compiler_resolved: Bool?
    let token_policy, name_syntax, unicode_normalization: String?
    let structure_file_byte_limit, unsupported_structure_files, extraction_limited_files: String?
    let structure_metadata_omitted, structure_metadata_bytes: String?
    let discovery_complete, search_in_progress, search_complete, truncated: Bool
    let count_all_matches, match_count_complete: Bool?
    let stop_reason: String?
    let catalogued_files, examined_files, scanned_files, unavailable_files, pending_files: String
    let search_scope, scope_extensions, scope_filenames, eligible_files, excluded_files: String?
    let scope_path_kind, scope_path, scope_path_hex: String?
    let scope_region_sha256, scope_region_files, scope_region_path_bytes: String?
    let scope_region_catalogued_files, scope_region_missing_files: String?
    let matches_seen, retained_hits, displayed_hits, step_count: String
    let hits: [SearchWireHit]
    let next_offset: String?
    let expression, primary_needle, query_syntax, predicate_scope, path_filter_domain: String?
    let max_scan_bytes, scanned_input_bytes, expression_excluded_files, expression_scope_files: String?

    var expressionWire: AtlasSearchExpression.Wire {
        .init(expression: expression, primary_needle: primary_needle, query_syntax: query_syntax,
            predicate_scope: predicate_scope, path_filter_domain: path_filter_domain,
            max_scan_bytes: max_scan_bytes, scanned_input_bytes: scanned_input_bytes,
            expression_excluded_files: expression_excluded_files, expression_scope_files: expression_scope_files)
    }
}
private struct SearchWireHit: Decodable, Equatable {
    let hit_id, file_id, source_revision, capture_sha256, capture_byte_length: String
    let original_range: SearchWireRange
    let path: SearchWirePath
    let structure: SearchWireStructure?
    let matched_text: String?
    var excerpt: AtlasSearchExcerptWire?
    func repeats(_ old: Self) -> Bool {
        hit_id == old.hit_id && file_id == old.file_id && source_revision == old.source_revision
            && capture_sha256 == old.capture_sha256 && capture_byte_length == old.capture_byte_length
            && original_range == old.original_range && path == old.path && structure == old.structure
            && AtlasSearchText.sameActualText(matched_text, old.matched_text)
            && (excerpt == nil || old.excerpt == nil || excerpt == old.excerpt)
    }
}
private struct SearchWireStructure: Decodable, Equatable {
    let name, kind, language, candidate_id, evidence_level: String
    let heading_slug: String?
    let evidence_range: SearchWireRange
    static func == (a: Self, b: Self) -> Bool {
        a.name.utf8.elementsEqual(b.name.utf8) && a.kind == b.kind && a.language == b.language
            && a.candidate_id == b.candidate_id && a.evidence_level == b.evidence_level
            && AtlasSearchText.sameActualText(a.heading_slug, b.heading_slug)
            && a.evidence_range == b.evidence_range
    }
}
private struct SearchWireRange: Decodable, Equatable { let start, end: String }
private struct SearchWirePath: Decodable, Equatable { let encoding, hex, display: String }

/// A single worker-confined accumulator. Identity/coverage are pinned before
/// appending; earlier hit witnesses must be byte-identical on repeated pages.
/// Exactly one canonical all-files row order is admitted. Raw paths are never
/// reconstructed from display labels, including non-UTF-8 Unix filenames.
private struct SearchStream {
    let query: String
    let scope: AtlasSearchFileScope
    let pathScope: AtlasSearchPathScope
    let mode: AtlasSearchMode
    let nameMode: AtlasSearchNameMode
    let caseSensitive: Bool
    let id = UUID()
    private var identity: [String]?
    private var state: SearchState?
    private var wireHits: [SearchWireHit] = []
    private var hits: [SearchHit] = []
    private var files: [String: SearchWireHit] = [:]
    private var retainedTextBytes = 0
    private var retainedExcerptBytes = 0
    init(query: String, scope: AtlasSearchFileScope, pathScope: AtlasSearchPathScope,
         mode: AtlasSearchMode, nameMode: AtlasSearchNameMode, caseSensitive: Bool = true) {
        self.query = query; self.scope = scope; self.pathScope = pathScope
        self.mode = mode; self.nameMode = nameMode
        self.caseSensitive = caseSensitive
    }
    var count: Int { hits.count }
    var retained: Int { state?.retained ?? 0 }
    private var beginCommand: String {
        mode == .expression ? "begin-expression" : (mode.isTextual ? "begin" : "begin-structure")
    }

    mutating func advance(_ page: SearchPage) throws {
        let next = try validated(page)
        if let previous = state {
            guard page.command == "step", previous.running,
                  next.steps == previous.steps + 1, next.steps <= 4097,
                  next.catalogued == previous.catalogued, next.eligible == previous.eligible,
                  next.visibleMissing == previous.visibleMissing,
                  next.discovery == previous.discovery,
                  next.examined >= previous.examined, next.examined <= previous.examined + 1,
                  next.scanned >= previous.scanned, next.unavailable >= previous.unavailable,
                  next.retained >= previous.retained, next.matches >= previous.matches else { throw invalid() }
            if let coverage = next.structureCoverage, let old = previous.structureCoverage {
                guard coverage.advances(old) else { throw invalid() }
            } else if next.structureCoverage != nil || previous.structureCoverage != nil { throw invalid() }
            if let expression = next.expression, let old = previous.expression {
                guard expression.advances(old) else { throw invalid() }
            } else if next.expression != nil || previous.expression != nil { throw invalid() }
        } else {
            guard page.command == beginCommand,
                  next.steps == 0, next.examined == (next.expression?.excludedFiles ?? 0),
                  next.scanned == 0, next.unavailable == 0,
                  next.retained == 0, next.matches == 0, next.running else { throw invalid() }
            if let expression = next.expression {
                guard expression.scannedInputBytes == 0 else { throw invalid() }
            }
            if let coverage = next.structureCoverage {
                guard coverage.unsupportedFiles == 0, coverage.extractionLimitedFiles == 0,
                      coverage.metadataOmitted == 0, coverage.metadataBytes == 0 else { throw invalid() }
            }
        }
        identity = [page.owner, page.source_manifest, page.layout_revision, page.query_generation]
        state = next
    }

    mutating func append(_ page: SearchPage, start: Int, limit: Int) throws {
        guard let state, try validated(page) == state,
              start == 0 || page.command == "page",
              start <= hits.count, start <= state.retained, page.hits.count <= limit else { throw invalid() }
        let end = min(start + limit, state.retained)
        guard page.hits.count == end - start else { throw invalid() }
        if end < state.retained {
            guard let offset = page.next_offset, try number(offset) == UInt64(end) else { throw invalid() }
        } else if page.next_offset != nil { throw invalid() }
        for (offset, wire) in page.hits.enumerated() {
            let position = start + offset
            guard try number(wire.hit_id) == UInt64(position + 1) else { throw invalid() }
            if position < hits.count {
                // Output admission may omit optional context on one response.
                // If both previews are present, their captured bytes must agree.
                guard wire.repeats(wireHits[position]) else { throw invalid() }
                continue
            }
            guard position == hits.count, hits.count < 1000 else { throw invalid() }
            var hit = try decode(wire, position: position)
            var retainedWire = wire
            if let previous = files[wire.file_id] {
                guard wire.source_revision == previous.source_revision, wire.path.hex == previous.path.hex,
                      wire.capture_sha256 == previous.capture_sha256,
                      wire.capture_byte_length == previous.capture_byte_length else { throw invalid() }
            }
            let charge = wire.path.hex.utf8.count + 2 * wire.path.display.utf8.count
                + (hit.sourcePath?.utf8.count ?? 0) + 2 * (hit.actualText?.utf8.count ?? 0) + 512
            guard charge <= AtlasSearchRetention.witnessBytes - retainedTextBytes else { throw invalid() }
            retainedTextBytes += charge
            if let excerpt = hit.excerpt {
                let previewCharge = excerpt.retainedByteCount + (wire.excerpt?.text.utf8.count ?? 0) + 256
                if previewCharge <= AtlasSearchRetention.excerptBytes - retainedExcerptBytes {
                    retainedExcerptBytes += previewCharge
                } else {
                    hit.excerpt = nil; retainedWire.excerpt = nil
                }
            }
            files[wire.file_id] = retainedWire
            wireHits.append(retainedWire)
            hits.append(hit)
        }
    }

    func report(owner: AtlasSearchCaptureOwner, root: String) throws -> AtlasSearchReport {
        guard let state, let identity, hits.count == state.retained, hits.count == wireHits.count else { throw invalid() }
        let source = try AtlasSearchCaptureIdentity(owner: number(identity[0]), manifest: number(identity[1]),
            layout: number(identity[2]), generation: number(identity[3]))
        let witnesses = try zip(hits, wireHits).map { hit, wire in
            AtlasSearchCaptureWitness(hit: hit, file: try number(wire.file_id),
                revision: try number(wire.source_revision), pathHex: wire.path.hex)
        }
        let capture = hits.isEmpty ? nil : AtlasSearchCaptureFactory.make(owner: owner,
            identity: source, root: root, needle: state.expression?.primaryNeedle ?? query, witnesses: witnesses,
            caseSensitive: caseSensitive, mode: mode, importReader: AtlasNativeSearch.importReader)
        return AtlasSearchReport(hits: hits, complete: state.complete, truncated: state.truncated,
            unavailableFiles: state.unavailable, unsupportedFiles: state.structureCoverage?.unsupportedFiles ?? 0, matchesSeen: state.matches,
            countAllMatches: mode == .text && caseSensitive,
            matchCountComplete: state.complete,
            progress: AtlasSearchProgress(isRunning: state.running, examinedFiles: state.examined,
                cataloguedFiles: state.eligible), streamID: id, capture: capture,
            fileScope: scope, excludedFiles: state.catalogued - state.eligible, pathScope: pathScope,
            mode: mode, nameMode: nameMode, structureCoverage: state.structureCoverage,
            caseSensitive: caseSensitive, visibleMissingFiles: state.visibleMissing, expression: state.expression)
    }

    private func validated(_ page: SearchPage) throws -> SearchState {
        guard page.schema == "fcb.atlas-search/1", page.status == "ok",
              [beginCommand, "step", "page"].contains(page.command),
              page.mode == (mode == .text ? AtlasSearchText.wireMode(caseSensitive: caseSensitive) : mode.wireMode),
              (page.case_sensitive ?? true) == caseSensitive,
              mode != .expression || page.case_sensitive != nil,
              page.search_strategy == (mode.isTextual ? "live-capture-scan" : "live-structure-scan"),
              mode == .expression || page.needle.utf8.elementsEqual(query.utf8),
              try number(page.query_generation) == 1 else { throw invalid() }
        for key in [page.owner, page.source_manifest, page.layout_revision] {
            guard try number(key) > 0 else { throw invalid() }
        }
        if let identity {
            guard identity == [page.owner, page.source_manifest, page.layout_revision, page.query_generation] else { throw invalid() }
        }
        let catalogued = try count(page.catalogued_files,
            max: pathScope.isVisible ? AtlasSearchPathScope.maximumVisibleFiles : 4096)
        try pathScope.validateEcho(kind: page.scope_path_kind, path: page.scope_path, hex: page.scope_path_hex,
            regionSHA256: page.scope_region_sha256, regionFiles: page.scope_region_files,
            regionPathBytes: page.scope_region_path_bytes)
        let eligible = try scope.eligibleFiles(kind: page.search_scope, extensions: page.scope_extensions,
            filenames: page.scope_filenames, eligible: page.eligible_files, excluded: page.excluded_files,
            catalogued: catalogued, pathScoped: !pathScope.isWorkspace)
        let expression = try page.expressionWire.admit(mode: mode, query: query,
            needle: page.needle, eligibleFiles: eligible)
        let expressionExcluded = expression?.excludedFiles ?? 0
        let visibleMissing = try pathScope.validateRegionCoverage(catalogued: page.scope_region_catalogued_files,
            missing: page.scope_region_missing_files, eligible: eligible, searchCatalogFiles: catalogued,
            allFileTypes: scope.isAll, complete: page.search_complete)
        let examined = try count(page.examined_files, max: min(eligible, 4096 + expressionExcluded))
        guard examined >= expressionExcluded else { throw invalid() }
        let scanned = try count(page.scanned_files, max: examined - expressionExcluded)
        let unavailable = try count(page.unavailable_files, max: examined - expressionExcluded)
        let pending = try count(page.pending_files, max: eligible)
        let retained = try count(page.retained_hits, max: 1000)
        let matches = try number(page.matches_seen)
        let steps = try count(page.step_count, max: 4097)
        let countedExact = mode == .text && caseSensitive
        guard (page.count_all_matches ?? false) == countedExact,
              (page.match_count_complete ?? page.search_complete) == page.search_complete,
              pending == eligible - examined, matches >= UInt64(retained),
              try number(page.displayed_hits) == UInt64(retained),
              page.search_in_progress == (page.stop_reason == nil) else { throw invalid() }
        let structureCoverage: AtlasSearchStructureCoverage?
        if mode == .occurrences {
            guard nameMode == .exact, caseSensitive,
                  page.includes_comments_and_literals == true, page.compiler_resolved == false,
                  page.token_policy == AtlasSearchOccurrencePolicy.tokenPolicy,
                  page.name_syntax == AtlasSearchOccurrencePolicy.nameSyntax,
                  page.unicode_normalization == "none" else { throw invalid() }
        } else {
            guard page.includes_comments_and_literals == nil, page.compiler_resolved == nil,
                  page.token_policy == nil, page.name_syntax == nil,
                  page.unicode_normalization == nil else { throw invalid() }
        }
        if mode.isTextual {
            guard page.requested_mode == nil, page.realized_mode == nil, page.name_mode == nil,
                  page.name_case == nil, page.evidence_level == nil, page.semantic_complete == nil,
                  page.completeness_basis == nil, page.structure_file_byte_limit == nil,
                  page.unsupported_structure_files == nil, page.extraction_limited_files == nil,
                  page.structure_metadata_omitted == nil, page.structure_metadata_bytes == nil,
                  matches <= (countedExact ? UInt64(32 * 1024 * 1024) : 1001) else { throw invalid() }
            structureCoverage = nil
        } else {
            guard page.requested_mode == mode.rawValue, page.realized_mode == mode.evidence,
                  page.name_mode == nameMode.rawValue, page.name_case == "sensitive",
                  page.evidence_level == mode.evidence, page.semantic_complete == false,
                  page.completeness_basis == "admitted-extraction-processing",
                  let cap = page.structure_file_byte_limit,
                  let unsupported = page.unsupported_structure_files,
                  let extraction = page.extraction_limited_files,
                  let omitted = page.structure_metadata_omitted,
                  let bytes = page.structure_metadata_bytes else { throw invalid() }
            let coverage = try AtlasSearchStructureCoverage(fileByteLimit: number(cap),
                unsupportedFiles: count(unsupported, max: unavailable),
                extractionLimitedFiles: count(extraction, max: min(scanned, unavailable)),
                metadataOmitted: count(omitted, max: 4096 * 4096),
                metadataBytes: count(bytes, max: 8 * 1024 * 1024))
            guard coverage.fileByteLimit == mode.structureFileByteLimit,
                  coverage.unsupportedFiles + coverage.extractionLimitedFiles <= unavailable,
                  coverage.unsupportedFiles + scanned <= examined,
                  coverage.metadataOmitted == 0 || unavailable > 0,
                  matches <= 4096 * 4096,
                  matches <= UInt64(retained + coverage.metadataOmitted + 1) else { throw invalid() }
            structureCoverage = coverage
        }
        if let stop = page.stop_reason {
            guard ["all-files-examined", "file-limit", "source-byte-limit", "match-limit", "verification-byte-limit"].contains(stop) else { throw invalid() }
        }
        if countedExact {
            guard page.stop_reason != "match-limit",
                  page.truncated == (matches > UInt64(retained)) else { throw invalid() }
        } else if mode.isTextual {
            guard page.truncated == (page.stop_reason == "match-limit"),
                  matches <= UInt64(retained + (page.truncated ? 1 : 0)) else { throw invalid() }
        }
        if page.search_complete {
            guard !page.search_in_progress, page.discovery_complete, pending == 0,
                  unavailable == 0,
                  countedExact ? (page.match_count_complete == true) : (!page.truncated && matches == UInt64(retained))
                  else { throw invalid() }
            if let coverage = structureCoverage {
                guard coverage.unsupportedFiles == 0, coverage.extractionLimitedFiles == 0,
                      coverage.metadataOmitted == 0 else { throw invalid() }
            }
        }
        return SearchState(catalogued: catalogued, eligible: eligible, visibleMissing: visibleMissing,
            examined: examined, scanned: scanned,
            unavailable: unavailable, retained: retained, matches: matches, steps: steps,
            running: page.search_in_progress, complete: page.search_complete,
            discovery: page.discovery_complete, truncated: page.truncated, stop: page.stop_reason,
            structureCoverage: structureCoverage, expression: expression)
    }

    private func decode(_ wire: SearchWireHit, position: Int) throws -> SearchHit {
        guard try number(wire.file_id) > 0, try number(wire.source_revision) > 0 else { throw invalid() }
        let start = try number(wire.original_range.start), end = try number(wire.original_range.end)
        let length = try number(wire.capture_byte_length)
        guard start < end, end <= length, length <= 1024 * 1024,
              wire.capture_sha256.utf8.count == 64,
              wire.capture_sha256.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
              wire.path.encoding == "unix-bytes", wire.path.hex.utf8.count <= 32_768,
              wire.path.display.utf8.count <= 65_536 else { throw invalid() }
        let input = Array(wire.path.hex.utf8)
        guard !input.isEmpty, input.count.isMultiple(of: 2) else { throw invalid() }
        func nibble(_ value: UInt8) throws -> UInt8 {
            switch value { case 48...57: return value - 48; case 97...102: return value - 87; default: throw invalid() }
        }
        var raw: [UInt8] = []
        raw.reserveCapacity(input.count / 2)
        for index in stride(from: 0, to: input.count, by: 2) {
            raw.append(try nibble(input[index]) * 16 + nibble(input[index + 1]))
        }
        guard !raw.contains(0), raw.first != 47,
              !raw.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                  $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
              }), scope.admitsRawPath(raw), pathScope.admitsRawPath(raw) else { throw invalid() }
        let structure: AtlasSearchStructure?
        if mode.isTextual {
            guard wire.structure == nil, mode != .expression || wire.matched_text != nil,
                  AtlasSearchText.accepts(actualText: wire.matched_text,
                    needle: state?.expression?.primaryNeedle ?? query,
                    caseSensitive: caseSensitive) else { throw invalid() }
            structure = nil
        } else {
            guard wire.matched_text == nil, let value = wire.structure,
                  !value.name.isEmpty, value.name.utf8.count <= 4096,
                  !value.kind.isEmpty, value.kind.utf8.count <= 64, !value.kind.utf8.contains(0),
                  !value.language.isEmpty, value.language.utf8.count <= 64,
                  value.evidence_level == mode.evidence,
                  value.heading_slug == nil || (value.heading_slug!.utf8.count <= 4096 && !value.heading_slug!.utf8.contains(0)),
                  length <= mode.structureFileByteLimit else { throw invalid() }
            let candidate = try number(value.candidate_id)
            let evidenceStart = try number(value.evidence_range.start)
            let evidenceEnd = try number(value.evidence_range.end)
            guard candidate > 0, candidate <= 4096, evidenceStart <= start,
                  evidenceEnd >= end, evidenceEnd <= length else { throw invalid() }
            if mode == .headings {
                guard let slug = value.heading_slug, !slug.isEmpty,
                      value.kind == "heading", value.language == "markdown",
                      evidenceStart == start, evidenceEnd == end else { throw invalid() }
            } else if mode == .occurrences {
                guard value.name.utf8.elementsEqual(query.utf8),
                      value.kind == "identifier-occurrence", value.language == "text",
                      value.heading_slug == nil, evidenceStart == start, evidenceEnd == end,
                      end - start == UInt64(query.utf8.count)
                        || end - start == UInt64(query.utf16.count * 2) else { throw invalid() }
            } else {
                guard value.heading_slug == nil,
                      ["rust", "python", "javascript", "typescript", "go", "cpp"].contains(value.language) else { throw invalid() }
            }
            structure = AtlasSearchStructure(name: value.name, kind: value.kind, language: value.language,
                evidence: value.evidence_level, candidate: candidate, headingSlug: value.heading_slug,
                evidenceStart: evidenceStart, evidenceEnd: evidenceEnd)
        }
        return SearchHit(id: position, path: wire.path.display, sourcePath: String(bytes: raw, encoding: .utf8),
            start: start, end: end, captureSHA256: wire.capture_sha256, captureByteLength: length,
            structure: structure, actualText: wire.matched_text,
            excerpt: try wire.excerpt?.validated(hitStart: start, hitEnd: end,
                capturedBytes: length, actualText: mode.isTextual
                    ? (wire.matched_text ?? state?.expression?.primaryNeedle ?? query)
                    : (mode == .occurrences ? query : nil)))
    }
    private func invalid() -> AtlasSearchError { .invalidResponse }
    private func number(_ text: String) throws -> UInt64 {
        guard let value = UInt64(text), String(value) == text else { throw invalid() }
        return value
    }
    private func count(_ text: String, max: Int) throws -> Int {
        let value = try number(text)
        guard value <= UInt64(max) else { throw invalid() }
        return Int(value)
    }
}
private struct SearchState: Equatable {
    let catalogued, eligible, visibleMissing, examined, scanned, unavailable, retained: Int
    let matches: UInt64
    let steps: Int
    let running, complete, discovery, truncated: Bool
    let stop: String?
    let structureCoverage: AtlasSearchStructureCoverage?
    let expression: AtlasSearchExpression?
}
