import Foundation

private typealias SearchPoll = @convention(c) (UnsafeMutableRawPointer?) -> Int32
@_silgen_name("fcb_atlas_create") private func createSearchAtlas() -> UInt64
@_silgen_name("fcb_atlas_open_cancelable")
private func openSearchAtlas(_ handle: UInt64, _ root: UnsafePointer<CChar>?, _ maxFiles: UInt64,
    _ poll: SearchPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_search_begin")
private func beginSearch(_ handle: UInt64, _ generation: UInt64, _ query: UnsafePointer<CChar>?,
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
        try AtlasSearchCoordinator.validate(root: root, query: query)
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
            let opened: SearchOpen = try read(root.withCString {
                rootPointer in owner.call { openSearchAtlas($0, rootPointer, 4096, poll, context) }
            }, cancellation)
            guard opened.status == "ok" else { throw AtlasSearchError.unavailable }
            var page: SearchPage = try read(query.withCString {
                queryPointer in owner.call { beginSearch($0, 1, queryPointer, 1000, 4096, 1024 * 1024, 32 * 1024 * 1024) }
            }, cancellation)
            var stream = SearchStream(query: query)
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
            let result = try JSONDecoder().decode(T.self, from: Data(bytes: pointer, count: length))
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
    let discovery_complete, search_in_progress, search_complete, truncated: Bool
    let stop_reason: String?
    let catalogued_files, examined_files, scanned_files, unavailable_files, pending_files: String
    let matches_seen, retained_hits, displayed_hits, step_count: String
    let hits: [SearchWireHit]
    let next_offset: String?
}
private struct SearchWireHit: Decodable, Equatable {
    let hit_id, file_id, source_revision, capture_sha256, capture_byte_length: String
    let original_range: SearchWireRange
    let path: SearchWirePath
}
private struct SearchWireRange: Decodable, Equatable { let start, end: String }
private struct SearchWirePath: Decodable, Equatable { let encoding, hex, display: String }

/// A single worker-confined accumulator. Identity/coverage are pinned before
/// appending; earlier hit witnesses must be byte-identical on repeated pages.
/// Exactly one canonical all-files row order is admitted. Raw paths are never
/// reconstructed from display labels, including non-UTF-8 Unix filenames.
private struct SearchStream {
    let query: String
    let id = UUID()
    private var identity: [String]?
    private var state: SearchState?
    private var wireHits: [SearchWireHit] = []
    private var hits: [SearchHit] = []
    private var files: [String: SearchWireHit] = [:]
    private var retainedTextBytes = 0
    init(query: String) { self.query = query }
    var count: Int { hits.count }
    var retained: Int { state?.retained ?? 0 }

    mutating func advance(_ page: SearchPage) throws {
        let next = try validated(page)
        if let previous = state {
            guard page.command == "step", previous.running,
                  next.steps == previous.steps + 1, next.steps <= 4097,
                  next.catalogued == previous.catalogued, next.discovery == previous.discovery,
                  next.examined >= previous.examined, next.examined <= previous.examined + 1,
                  next.scanned >= previous.scanned, next.unavailable >= previous.unavailable,
                  next.retained >= previous.retained, next.matches >= previous.matches else { throw invalid() }
        } else {
            guard page.command == "begin", next.steps == 0, next.examined == 0,
                  next.retained == 0, next.matches == 0, next.running else { throw invalid() }
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
                guard wire == wireHits[position] else { throw invalid() }
                continue
            }
            guard position == hits.count, hits.count < 1000 else { throw invalid() }
            let hit = try decode(wire, position: position)
            if let previous = files[wire.file_id] {
                guard wire.source_revision == previous.source_revision, wire.path.hex == previous.path.hex,
                      wire.capture_sha256 == previous.capture_sha256,
                      wire.capture_byte_length == previous.capture_byte_length else { throw invalid() }
            }
            let charge = wire.path.hex.utf8.count + 2 * wire.path.display.utf8.count
                + (hit.sourcePath?.utf8.count ?? 0) + 512
            guard charge <= 8 * 1024 * 1024 - retainedTextBytes else { throw invalid() }
            retainedTextBytes += charge
            files[wire.file_id] = wire
            wireHits.append(wire)
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
            identity: source, root: root, needle: query, witnesses: witnesses,
            importReader: AtlasNativeSearch.importReader)
        return AtlasSearchReport(hits: hits, complete: state.complete, truncated: state.truncated,
            unavailableFiles: state.unavailable, unsupportedFiles: 0, matchesSeen: state.matches,
            progress: AtlasSearchProgress(isRunning: state.running, examinedFiles: state.examined,
                cataloguedFiles: state.catalogued), streamID: id, capture: capture)
    }

    private func validated(_ page: SearchPage) throws -> SearchState {
        guard page.schema == "fcb.atlas-search/1", page.status == "ok",
              ["begin", "step", "page"].contains(page.command),
              page.mode == "exact-decoded-literal", page.search_strategy == "live-capture-scan",
              page.needle.utf8.elementsEqual(query.utf8), try number(page.query_generation) == 1 else { throw invalid() }
        for key in [page.owner, page.source_manifest, page.layout_revision] {
            guard try number(key) > 0 else { throw invalid() }
        }
        if let identity {
            guard identity == [page.owner, page.source_manifest, page.layout_revision, page.query_generation] else { throw invalid() }
        }
        let catalogued = try count(page.catalogued_files, max: 4096)
        let examined = try count(page.examined_files, max: catalogued)
        let scanned = try count(page.scanned_files, max: examined)
        let unavailable = try count(page.unavailable_files, max: examined)
        let pending = try count(page.pending_files, max: catalogued)
        let retained = try count(page.retained_hits, max: 1000)
        let matches = try number(page.matches_seen)
        let steps = try count(page.step_count, max: 4097)
        guard pending == catalogued - examined, matches >= UInt64(retained),
              matches <= 1001, try number(page.displayed_hits) == UInt64(retained),
              page.search_in_progress == (page.stop_reason == nil) else { throw invalid() }
        if let stop = page.stop_reason {
            guard ["all-files-examined", "file-limit", "source-byte-limit", "match-limit", "verification-byte-limit"].contains(stop) else { throw invalid() }
        }
        if page.search_complete {
            guard !page.search_in_progress, page.discovery_complete, pending == 0,
                  unavailable == 0, !page.truncated, matches == UInt64(retained) else { throw invalid() }
        }
        return SearchState(catalogued: catalogued, examined: examined, scanned: scanned,
            unavailable: unavailable, retained: retained, matches: matches, steps: steps,
            running: page.search_in_progress, complete: page.search_complete,
            discovery: page.discovery_complete, truncated: page.truncated, stop: page.stop_reason)
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
              }) else { throw invalid() }
        return SearchHit(id: position, path: wire.path.display, sourcePath: String(bytes: raw, encoding: .utf8),
            start: start, end: end, captureSHA256: wire.capture_sha256, captureByteLength: length)
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
    let catalogued, examined, scanned, unavailable, retained: Int
    let matches: UInt64
    let steps: Int
    let running, complete, discovery, truncated: Bool
    let stop: String?
}
