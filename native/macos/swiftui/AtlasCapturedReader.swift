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
        }, window: window, lines: lines, cancel: cancel, close: close,
        retirementFailed: retirementFailed)
    }
}

struct AtlasCapturedReaderResult<Page: Sendable>: Sendable {
    let page: Page
    let report: AtlasReaderFindReport
    let index: Int
    let target: AtlasReaderHitTarget
}

/// Re-establish the exact occurrence in the independently owned reader's query
/// namespace. This is one bounded retained-file search, not source I/O or a
/// second matcher. The existing hit decoder maps UTF-8/UTF-16/BOM/NUL coordinates;
/// a workspace offset is never cast into a native text offset or a reader ID.
enum AtlasCapturedReader {
    @MainActor static func activate<Page: Sendable>(
        _ selected: AtlasSearchCapturedHit, identity: AtlasReaderIdentity,
        find: @MainActor (String) async throws -> AtlasReaderFindReport,
        read: @MainActor (AtlasReaderHitTarget) async throws -> Page
    ) async throws -> AtlasCapturedReaderResult<Page> {
        try Task.checkCancellation()
        guard identity.capturedBytes == selected.capturedBytes,
              identity.pathBytes.elementsEqual(selected.path.utf8),
              selected.start < selected.end, selected.end <= selected.capturedBytes else {
            throw AtlasReaderError.invalidResponse
        }
        try AtlasReaderFindReport.validateNeedle(selected.needle)
        let report = try await find(selected.needle)
        try Task.checkCancellation()
        guard report.identity.matches(identity), report.needle.utf8.elementsEqual(selected.needle.utf8),
              report.hits.count <= Int(AtlasReaderFindReport.maxHits) else { throw AtlasReaderError.invalidResponse }
        // Native workspace reports retain at most 1000 hits; the retained-file
        // query admits 4096. The selected occurrence must actually be present:
        // never substitute the first hit, the nearest byte, or file start.
        let matches = report.hits.indices.filter {
            report.hits[$0].start == selected.start && report.hits[$0].end == selected.end
        }
        guard matches.count == 1, let index = matches.first, let target = report.target(at: index) else {
            throw AtlasReaderError.engine("CAPTURED_SEARCH_HIT_UNAVAILABLE")
        }
        let page = try await read(target)
        try Task.checkCancellation()
        return AtlasCapturedReaderResult(page: page, report: report, index: index, target: target)
    }
}
