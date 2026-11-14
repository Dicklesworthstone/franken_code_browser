import Foundation

extension AtlasReaderInput {
    /// The label of an already captured source, not a new filesystem path.
    /// Only pair with the capture-only transport below. Root/path validation
    /// still precedes admission, but no live source is opened by this route.
    init(captured target: AtlasSearchCapturedHit) throws {
        _ = try AtlasReaderInput(root: target.root, path: target.path)
        fullPath = target.path
    }
}

extension AtlasReaderTransport {
    /// Populate a fresh reader from the pinned search capture. Every other
    /// operation uses the ordinary retained reader and its decoder. A refused
    /// transfer never falls back to base.open or a live-file read.
    func importing(_ target: AtlasSearchCapturedHit) -> Self {
        Self(create: create, open: { handle, path, _ in
            guard path.utf8.elementsEqual(target.path.utf8) else { return nil }
            return try? target.openReader(handle)
        }, window: window, lines: lines, position: position, cancel: cancel, close: close,
        retirementFailed: retirementFailed, fork: fork, openBytes: openBytes, comparison: comparison,
        references: references, textSelection: textSelection)
    }
}

struct AtlasCapturedReaderResult<Page: Sendable>: Sendable {
    let page: Page
    let report: AtlasReaderFindReport
    let index: Int
    let target: AtlasReaderHitTarget
}

struct AtlasCapturedReferenceResult<Page: Sendable>: Sendable {
    let page: Page
    let referencePage: AtlasReferencePage
    let target: AtlasReferenceTarget
    let history: [AtlasReferencePageRequest]
}

/// Re-establish the exact occurrence in the independently owned reader's query
/// namespace. Bounded ordinal pages search only the retained capture, without
/// source I/O or a second matcher. The hit decoder maps UTF-8/UTF-16/BOM/NUL coordinates;
/// a workspace offset is never cast into a native text offset or a reader ID.
enum AtlasCapturedReader {
    /// Structural names may be prefixes or parsed heading text, so they do not
    /// authorize a literal Find. Open the retained evidence's original range.
    static func structuralRequest(_ selected: AtlasSearchCapturedHit,
                                  identity: AtlasReaderIdentity) throws -> AtlasReaderRequest {
        guard selected.mode.supportsNameMatching, identity.capturedBytes == selected.capturedBytes,
              identity.pathBytes.elementsEqual(selected.path.utf8),
              selected.start < selected.end, selected.end <= selected.capturedBytes,
              selected.end - selected.start <= AtlasReaderLimits.maximumPageBytes else {
            throw AtlasReaderError.invalidResponse
        }
        return .window(offset: selected.start,
            bytes: max(AtlasReaderLimits.pageBytes, selected.end - selected.start))
    }

    // Ordinal paging replays the capture from its beginning. Reserve the full
    // possible scan before each request, including the first; receipt sizes
    // cannot justify unbounded replay when an adversarial file has many hits.
    static let maximumActivationReplayBytes: UInt64 = 64 * 1024 * 1024
    static let maximumActivationPages = 64

    @MainActor static func activate<Page: Sendable>(
        _ selected: AtlasSearchCapturedHit, identity: AtlasReaderIdentity,
        find: @MainActor (String) async throws -> AtlasReaderFindReport,
        nextPage: (@MainActor (String, UInt64) async throws -> AtlasReaderFindReport)? = nil,
        read: @MainActor (AtlasReaderHitTarget) async throws -> Page,
        isCurrent: @MainActor () -> Bool = { true }
    ) async throws -> AtlasCapturedReaderResult<Page> {
        @MainActor func checkCurrent() throws {
            try Task.checkCancellation()
            guard isCurrent() else { throw AtlasReaderError.canceled }
        }
        try checkCurrent()
        guard selected.mode.isTextual, identity.capturedBytes == selected.capturedBytes,
              identity.pathBytes.elementsEqual(selected.path.utf8),
              selected.start < selected.end, selected.end <= selected.capturedBytes else {
            throw AtlasReaderError.invalidResponse
        }
        try AtlasReaderFindReport.validateNeedle(selected.needle)
        var remainingBytes = maximumActivationReplayBytes
        var pageCount = 0
        @MainActor func reserveScan() throws {
            try checkCurrent()
            guard pageCount < maximumActivationPages, identity.capturedBytes <= remainingBytes else {
                throw AtlasReaderError.engine("CAPTURED_SEARCH_HIT_UNAVAILABLE")
            }
            remainingBytes -= identity.capturedBytes
            pageCount += 1
        }
        try reserveScan()
        var report = try await find(selected.needle)
        var cursor: UInt64 = 0
        var previousGeneration: UInt64 = 0
        while true {
            try checkCurrent()
            guard report.identity.matches(identity), report.generation > previousGeneration,
                  report.needle.utf8.elementsEqual(selected.needle.utf8),
                  report.options == .init(),
                  report.hits.count <= Int(AtlasReaderFindReport.maxHits),
                  report.skippedMatches == cursor,
                  report.pageStartAfter == cursor || (pageCount == 1 && report.pageStartAfter == nil) else {
                throw AtlasReaderError.invalidResponse
            }
            // Long exact witnesses can lower the retained row allowance. A
            // workspace selection may therefore occur after the first page.
            // Rebind only its exact original range in the current generation.
            let matches = report.hits.indices.filter {
                report.hits[$0].start == selected.start && report.hits[$0].end == selected.end
            }
            if matches.count == 1, let index = matches.first, let target = report.target(at: index) {
                try checkCurrent()
                let page = try await read(target)
                try checkCurrent()
                return AtlasCapturedReaderResult(page: page, report: report, index: index, target: target)
            }
            guard matches.isEmpty else {
                throw AtlasReaderError.engine("CAPTURED_SEARCH_HIT_UNAVAILABLE")
            }
            // A legacy table has no continuation authority. A continuation is
            // derived only from a validated global occurrence, never a byte
            // offset, the look-ahead count, or a page-local hit index.
            guard report.pageStartAfter != nil, report.hasMoreMatches,
                  let nextPage, let last = report.hits.last,
                  last.occurrence > cursor, last.occurrence <= report.matchesSeen else {
                throw AtlasReaderError.engine("CAPTURED_SEARCH_HIT_UNAVAILABLE")
            }
            cursor = last.occurrence
            previousGeneration = report.generation
            try reserveScan()
            report = try await nextPage(selected.needle, cursor)
        }
    }

    /// Revalidate against the shared whole-token engine in this retained
    /// reader. Substring Find can exhaust its result allowance on unrelated
    /// identifier prefixes before the selected token, so it cannot authorize
    /// an Occurrences result. No source is reopened and no token parser runs
    /// in Swift. At most 4096 retained occurrence rows / 33 pages are visited.
    @MainActor static func activateOccurrence<Page: Sendable>(
        _ selected: AtlasSearchCapturedHit, identity: AtlasReaderIdentity,
        prepare: @MainActor (String) async throws -> AtlasReferencePage,
        nextPage: @MainActor (AtlasReferencePageRequest) async throws -> AtlasReferencePage,
        read: @MainActor (AtlasReferenceTarget) async throws -> Page,
        isCurrent: @MainActor () -> Bool = { true }
    ) async throws -> AtlasCapturedReferenceResult<Page> {
        @MainActor func checkCurrent() throws {
            try Task.checkCancellation()
            guard isCurrent() else { throw AtlasReaderError.canceled }
        }
        try checkCurrent()
        guard selected.mode == .occurrences, selected.caseSensitive,
              identity.capturedBytes == selected.capturedBytes,
              identity.capturedBytes <= AtlasReferencePage.sourceLimit,
              identity.pathBytes.elementsEqual(selected.path.utf8),
              selected.start < selected.end, selected.end <= selected.capturedBytes else {
            throw AtlasReaderError.invalidResponse
        }
        try AtlasReferencePage.validateName(selected.needle)
        var current = try await prepare(selected.needle)
        try checkCurrent()
        let inventory = current.inventory
        guard inventory.identity.matches(identity), inventory.generation > 0,
              inventory.name.utf8.elementsEqual(selected.needle.utf8),
              inventory.maxItems == AtlasReferencePage.maxItems,
              inventory.retained <= AtlasReferencePage.maxItems else {
            throw AtlasReaderError.invalidResponse
        }
        var request = AtlasReferencePageRequest.initial
        var history: [AtlasReferencePageRequest] = []
        var previousEnd: UInt64 = 0
        for _ in 0..<33 {
            try checkCurrent()
            guard current.inventory.matches(inventory), current.request == request,
                  request.start <= inventory.retained,
                  UInt64(current.rows.count) == min(request.limit, inventory.retained - request.start),
                  current.rows.first.map({ $0.range.start >= previousEnd }) ?? true else {
                throw AtlasReaderError.invalidResponse
            }
            let matches = current.rows.filter {
                $0.range.start == selected.start && $0.range.end == selected.end
            }
            if matches.count == 1, let row = matches.first, let target = current.target(id: row.id) {
                let selectedPage = try await read(target)
                try checkCurrent()
                return AtlasCapturedReferenceResult(page: selectedPage, referencePage: current,
                    target: target, history: history)
            }
            guard matches.isEmpty else { throw AtlasReaderError.invalidResponse }
            guard let next = current.nextOffset else {
                throw AtlasReaderError.engine("CAPTURED_OCCURRENCE_UNAVAILABLE")
            }
            guard next == request.start + UInt64(current.rows.count),
                  next > request.start, next < inventory.retained else {
                throw AtlasReaderError.invalidResponse
            }
            previousEnd = current.rows.last?.range.end ?? previousEnd
            history.append(request)
            request = AtlasReferencePageRequest(start: next, limit: 128)
            current = try await nextPage(request)
            try checkCurrent()
        }
        throw AtlasReaderError.invalidResponse
    }

}
