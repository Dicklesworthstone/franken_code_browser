import Foundation

extension AtlasReaderInput {
    /// The label of an already captured source, not a new filesystem path.
    /// Only pair with the capture-only transport below. Root/path validation
    /// still precedes admission, but no live source is opened by this route.
    init(captured target: AtlasSearchCapturedHit) throws {
        guard !target.root.isEmpty, target.root.utf8.count <= 16_384,
              !target.root.utf8.contains(0), !target.path.isEmpty, target.path.utf8.count <= 65_536,
              !target.path.utf8.contains(0), SearchHit.validPath(target.pathBytes) else {
            throw AtlasReaderError.invalidInput
        }
        fullPath = target.path
        capturedPathBytes = target.pathBytes
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
        references: references, textSelection: textSelection, matchingDelimiter: matchingDelimiter)
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
              identity.pathBytes == selected.pathBytes,
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
              identity.pathBytes == selected.pathBytes,
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
        // Zero-based occurrence positions, not byte offsets passed as ranks.
        // An exact literal has at most one occurrence starting at an original
        // byte, so the selected start provides a conservative finite ceiling.
        // UTF-16, BOMs and gaps can make this ceiling much larger than the count.
        var lower: UInt64 = 0
        var upper = selected.start + 1 // start < end <= capturedBytes
        var exactTotal: UInt64?
        while true {
            try checkCurrent()
            guard report.identity.matches(identity), report.generation > previousGeneration,
                  report.needle.utf8.elementsEqual(selected.needle.utf8),
                  report.options == .init(), report.expression == nil,
                  report.matchesSeen <= identity.capturedBytes,
                  report.retainedLimit > 0, report.retainedLimit <= AtlasReaderFindReport.maxHits,
                  report.hits.count <= Int(report.retainedLimit) else {
                throw AtlasReaderError.invalidResponse
            }
            if let pageStart = report.pageStartAfter {
                // A rank beyond EOF is a valid empty completed page. It proves
                // an upper bound, not a changed source or a cursor substitution.
                let skipped = min(cursor, report.matchesSeen)
                guard pageStart == cursor, report.skippedMatches == skipped,
                      UInt64(report.hits.count) == min(report.retainedLimit, report.matchesSeen - skipped),
                      report.hasMoreMatches == (report.matchesSeen - skipped > UInt64(report.hits.count)) else {
                    throw AtlasReaderError.invalidResponse
                }
            } else {
                guard pageCount == 1, cursor == 0, report.skippedMatches == 0 else {
                    throw AtlasReaderError.invalidResponse
                }
            }
            if let exactTotal {
                guard report.matchesSeen <= exactTotal,
                      !report.complete || report.matchesSeen == exactTotal else {
                    throw AtlasReaderError.invalidResponse
                }
            }
            if report.complete { exactTotal = report.matchesSeen }
            var previousStart: UInt64?
            for (index, hit) in report.hits.enumerated() {
                guard hit.index == UInt64(index), hit.occurrence > 0,
                      hit.occurrence <= report.matchesSeen,
                      hit.start < hit.end, hit.end <= identity.capturedBytes,
                      hit.occurrence <= hit.start + 1,
                      previousStart.map({ hit.start > $0 }) ?? true,
                      hit.matchedText.utf8.elementsEqual(selected.needle.utf8) else {
                    throw AtlasReaderError.invalidResponse
                }
                if report.pageStartAfter != nil {
                    let (expected, overflow) = cursor.addingReportingOverflow(UInt64(index) + 1)
                    guard !overflow, hit.occurrence == expected else { throw AtlasReaderError.invalidResponse }
                }
                // Overlapping literal occurrences are valid; only starts must
                // increase. Comparing the preceding end would skip long hits.
                previousStart = hit.start
            }
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
            // Legacy tables cannot authorize rank probes. A partial scan that
            // stopped for another reason cannot establish absent later ranks.
            guard report.pageStartAfter != nil, let nextPage,
                  report.complete || (report.state == "match-limit" && report.hasMoreMatches) else {
                throw AtlasReaderError.engine("CAPTURED_SEARCH_HIT_UNAVAILABLE")
            }
            if report.complete { upper = min(upper, report.matchesSeen) }
            if let first = report.hits.first, let last = report.hits.last {
                if selected.start < first.start { upper = min(upper, cursor) }
                else if selected.start > last.start { lower = max(lower, last.occurrence) }
                else {
                    // The source-ordered page straddles the requested start
                    // but has no exact range. Never select a nearby occurrence.
                    throw AtlasReaderError.engine("CAPTURED_SEARCH_HIT_UNAVAILABLE")
                }
            } else if !report.complete {
                throw AtlasReaderError.engine("CAPTURED_SEARCH_HIT_UNAVAILABLE")
            }
            guard lower < upper else { throw AtlasReaderError.engine("CAPTURED_SEARCH_HIT_UNAVAILABLE") }
            // Keep the adjacent page cheap for ordinary selections, then
            // halve the possible ranks instead of replaying every preceding
            // page. A dense 1 MiB capture remains within the same 64 MiB cap.
            let next = pageCount == 1 && report.hits.last.map({ selected.start > $0.start }) == true
                ? lower : lower + (upper - lower) / 2
            guard next != cursor else { throw AtlasReaderError.invalidResponse }
            cursor = next
            previousGeneration = report.generation
            try reserveScan()
            report = try await nextPage(selected.needle, cursor)
        }
    }

    /// Revalidate against the shared whole-token engine in this retained
    /// reader. Substring Find can exhaust its result allowance on unrelated
    /// identifier prefixes before the selected token, so it cannot authorize
    /// an Occurrences result. No source is reopened and no token parser runs
    /// in Swift. Legacy inventories visit at most 4096 rows / 33 pages. Counted
    /// windows locate a later range through bounded ordinal probes instead of
    /// loading every intervening occurrence into the native client.
    @MainActor static func activateOccurrence<Page: Sendable>(
        _ selected: AtlasSearchCapturedHit, identity: AtlasReaderIdentity,
        prepare: @MainActor (String) async throws -> AtlasReferencePage,
        nextPage: @MainActor (AtlasReferencePageRequest) async throws -> AtlasReferencePage,
        prepareWindow: (@MainActor (String, UInt64) async throws -> AtlasReferencePage)? = nil,
        read: @MainActor (AtlasReferenceTarget) async throws -> Page,
        isCurrent: @MainActor () -> Bool = { true }
    ) async throws -> AtlasCapturedReferenceResult<Page> {
        @MainActor func checkCurrent() throws {
            try Task.checkCancellation()
            guard isCurrent() else { throw AtlasReaderError.canceled }
        }
        try checkCurrent()
        let query = selected.occurrenceQuery ?? selected.needle
        guard selected.mode == .occurrences,
              AtlasSearchOccurrencePolicy.matches(source: selected.needle, query: query,
                  caseSensitive: selected.caseSensitive),
              identity.capturedBytes == selected.capturedBytes,
              identity.capturedBytes <= AtlasReferencePage.sourceLimit,
              identity.pathBytes == selected.pathBytes,
              selected.start < selected.end, selected.end <= selected.capturedBytes else {
            throw AtlasReaderError.invalidResponse
        }
        try AtlasReferencePage.validateName(query)
        var current = try await prepare(query)
        try checkCurrent()
        if let prepareWindow {
            guard current.inventory.window != nil else { throw AtlasReaderError.invalidResponse }
            return try await activateOccurrenceWindow(selected, identity: identity, first: current,
                prepareWindow: prepareWindow, read: read, isCurrent: isCurrent)
        }
        let inventory = current.inventory
        guard inventory.identity.matches(identity), inventory.generation > 0,
              inventory.name.utf8.elementsEqual(query.utf8),
              inventory.caseSensitive == selected.caseSensitive,
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

    /// Original ranges are strictly ordered whole-token occurrences. Narrow the
    /// possible ordinal interval using validated first-page ranges; the shared
    /// engine alone decides which bytes match. Each probe replays at most one
    /// admitted capture, with the same fixed scan/page allowances as Find.
    @MainActor private static func activateOccurrenceWindow<Page: Sendable>(
        _ selected: AtlasSearchCapturedHit, identity: AtlasReaderIdentity, first: AtlasReferencePage,
        prepareWindow: @MainActor (String, UInt64) async throws -> AtlasReferencePage,
        read: @MainActor (AtlasReferenceTarget) async throws -> Page,
        isCurrent: @MainActor () -> Bool
    ) async throws -> AtlasCapturedReferenceResult<Page> {
        @MainActor func checkCurrent() throws {
            try Task.checkCancellation()
            guard isCurrent() else { throw AtlasReaderError.canceled }
        }
        let query = selected.occurrenceQuery ?? selected.needle
        guard first.inventory.window?.request.offset == 0,
              first.inventory.matchesCounted <= identity.capturedBytes,
              identity.capturedBytes <= maximumActivationReplayBytes else { throw AtlasReaderError.invalidResponse }
        let total = first.inventory.matchesCounted
        var lower: UInt64 = 0, upper = total, offset: UInt64 = 0
        var current = first, previousGeneration: UInt64 = 0
        var remainingBytes = maximumActivationReplayBytes - identity.capturedBytes
        for probe in 0..<maximumActivationPages {
            try checkCurrent()
            let inventory = current.inventory
            guard inventory.identity.matches(identity), inventory.generation > previousGeneration,
                  inventory.name.utf8.elementsEqual(query.utf8),
                  inventory.caseSensitive == selected.caseSensitive,
                  inventory.maxItems == AtlasReferencePage.maxItems,
                  inventory.matchesCounted == total, inventory.countComplete,
                  let window = inventory.window, window.request.countAllMatches,
                  window.request.offset == offset, offset <= total,
                  window.skipped == offset, current.request == .initial,
                  inventory.retained == min(AtlasReferencePage.maxItems, total - offset),
                  UInt64(current.rows.count) == min(AtlasReferencePageRequest.initial.limit, inventory.retained),
                  current.nextOffset == (UInt64(current.rows.count) < inventory.retained ? UInt64(current.rows.count) : nil)
            else { throw AtlasReaderError.invalidResponse }
            let windowEnd = offset + inventory.retained
            guard inventory.limited == (total > windowEnd), window.hasMore == inventory.limited,
                  window.rowsTruncated == (total > inventory.retained),
                  window.nextOffset == (window.hasMore && inventory.retained > 0 ? windowEnd : nil) else {
                throw AtlasReaderError.invalidResponse
            }
            var previousEnd: UInt64 = 0
            for (index, row) in current.rows.enumerated() {
                let id = UInt64(index) + 1
                guard row.id == id, row.occurrence == offset + id,
                      row.range.start >= previousEnd, row.range.start < row.range.end,
                      row.range.end <= identity.capturedBytes else { throw AtlasReaderError.invalidResponse }
                previousEnd = row.range.end
            }
            let matches = current.rows.filter { $0.range.start == selected.start && $0.range.end == selected.end }
            if matches.count == 1, let row = matches.first, let target = current.target(id: row.id) {
                let page = try await read(target)
                try checkCurrent()
                return AtlasCapturedReferenceResult(page: page, referencePage: current, target: target, history: [])
            }
            guard matches.isEmpty, let firstRow = current.rows.first, let lastRow = current.rows.last else {
                throw AtlasReaderError.engine("CAPTURED_OCCURRENCE_UNAVAILABLE")
            }
            if selected.start < firstRow.range.start { upper = offset }
            else if selected.start >= lastRow.range.end { lower = offset + UInt64(current.rows.count) }
            else { throw AtlasReaderError.engine("CAPTURED_OCCURRENCE_UNAVAILABLE") }
            guard lower < upper, probe + 1 < maximumActivationPages,
                  identity.capturedBytes <= remainingBytes else {
                throw AtlasReaderError.engine("CAPTURED_OCCURRENCE_UNAVAILABLE")
            }
            offset = lower + (upper - lower) / 2
            previousGeneration = inventory.generation
            remainingBytes -= identity.capturedBytes
            try checkCurrent()
            current = try await prepareWindow(query, offset)
        }
        throw AtlasReaderError.engine("CAPTURED_OCCURRENCE_UNAVAILABLE")
    }

}
