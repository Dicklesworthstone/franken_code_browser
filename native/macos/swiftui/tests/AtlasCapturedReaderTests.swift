// Production activation pipeline with typed reader results. This does not
// substitute for the existing decoder or real Swift-to-Rust ABI qualification.
import Foundation

private final class Calls: @unchecked Sendable {
    private let lock = NSLock()
    private var counts: [String: Int] = [:]
    func add(_ key: String) { lock.lock(); counts[key, default: 0] += 1; lock.unlock() }
    func get(_ key: String) -> Int { lock.lock(); defer { lock.unlock() }; return counts[key, default: 0] }
}
@main private enum AtlasCapturedReaderTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        precondition(condition(), message); checks += 1
    }
    private static func selected(path: String = "src.rs", root: String = "/repo", needle: String = "x",
                                 start: UInt64 = 10, end: UInt64 = 12, bytes: UInt64 = 20,
                                 mode: AtlasSearchMode = .text, caseSensitive: Bool = true) -> AtlasSearchCapturedHit {
        .init(root: root, path: path, needle: needle, start: start, end: end, capturedBytes: bytes,
            openReader: { _ in "captured-info" }, mode: mode, caseSensitive: caseSensitive)
    }
    private static func identity(owner: UInt64 = 100, revision: UInt64 = 1, bytes: UInt64 = 20,
                                 path: String = "src.rs", encoding: String = "utf16le") -> AtlasReaderIdentity {
        .init(owner: owner, file: 1, revision: revision, capturedBytes: bytes,
              pathBytes: Array(path.utf8), encoding: encoding, displayPath: "escaped label")
    }
    private static func report(identity: AtlasReaderIdentity = identity(), needle: String = "x",
                               ranges: [(UInt64, UInt64)] = [(2, 4), (10, 12)],
                               hex: String? = "7800", complete: Bool = true,
                               generation: UInt64 = 23, startAfter: UInt64? = nil,
                               more: Bool = false, retainedLimit: UInt64 = 4096,
                               options: AtlasReaderFindOptions = .init()) throws -> AtlasReaderFindReport {
        let seen = (startAfter ?? 0) + UInt64(ranges.count) + (more ? 1 : 0)
        var wire: [String: Any] = [
            "schema": "fcb.reader-session/1", "status": "ok", "command": "find",
            "owner": String(identity.owner), "file_id": String(identity.file),
            "source_revision": String(identity.revision), "captured_bytes": String(identity.capturedBytes),
            "encoding": identity.encoding, "additional_source_bytes_read": "0", "native_presented": false,
            "path": ["encoding": "unix-bytes", "display": identity.displayPath,
                "hex": identity.pathBytes.map { String(format: "%02x", $0) }.joined()],
            "query_generation": String(generation), "needle": needle,
            "mode": options.caseSensitive ? "exact-decoded-literal" : "unicode-lowercase-literal",
            "state": complete ? "complete-observed-input" : "match-limit", "search_complete": complete,
            "scanned_bytes": String(identity.capturedBytes), "matches_seen": String(seen),
            "retained_hits": String(ranges.count), "retained_limit": String(retainedLimit),
            "case_sensitive": options.caseSensitive, "count_all_matches": options.countAllMatches,
            "match_count_complete": complete, "rows_truncated": seen > UInt64(ranges.count),
            "literal_original_hex": options.caseSensitive && !ranges.isEmpty ? (hex.map { $0 as Any } ?? NSNull()) : NSNull(),
            "unsupported_at": NSNull()
        ]
        wire["hits"] = ranges.enumerated().map { index, range -> [String: Any] in
            var row: [String: Any] = [
                "hit_index": String(index), "occurrence_id": String((startAfter ?? 0) + UInt64(index) + 1),
                "original_range": ["start": String(range.0), "end": String(range.1)]
            ]
            if !options.caseSensitive {
                row["matched_text"] = needle
                row["original_hex"] = hex.map { $0 as Any } ?? NSNull()
            }
            return row
        }
        if let startAfter {
            wire["page_start_after"] = String(startAfter)
            wire["skipped_matches"] = String(startAfter)
            wire["has_more_matches"] = more
        }
        let json = String(data: try JSONSerialization.data(withJSONObject: wire), encoding: .utf8)!
        let info = AtlasReaderInfo(identity: identity, origin: "regular-file-observation-not-atomic", captureSHA256: nil)
        return try AtlasReaderFindReport.decode(json, info: info, generation: generation, needle: needle,
            options: options, startAfter: startAfter)
    }
    @MainActor private static func refused(_ target: AtlasSearchCapturedHit = selected(),
                                          identity: AtlasReaderIdentity = identity(),
                                          result: () throws -> AtlasReaderFindReport = { try report() }) async {
        var readCount = 0
        do {
            _ = try await AtlasCapturedReader.activate(target, identity: identity,
                find: { _ in try result() }, read: { _ in readCount += 1; return "wrong page" })
            preconditionFailure("Invalid selection admitted")
        } catch {}
        check(readCount == 0, "Invalid source/range must not trigger any approximate read")
    }
    private static func referencePage(identity: AtlasReaderIdentity, name: String = "foo",
        ranges: [(UInt64, UInt64)], request: AtlasReferencePageRequest = .initial,
        generation: UInt64 = 29) -> AtlasReferencePage {
        let retained = UInt64(ranges.count)
        let end = min(retained, request.start + request.limit)
        let rows = ranges.enumerated().dropFirst(Int(request.start)).prefix(Int(request.limit)).map { index, range in
            AtlasReferenceOccurrence(id: UInt64(index + 1),
                range: AtlasOutlineRange(start: range.0, end: range.1), line: 1)
        }
        return AtlasReferencePage(id: UUID(),
            inventory: AtlasReferenceInventory(identity: identity, generation: generation, name: name,
                maxItems: AtlasReferencePage.maxItems, retained: retained, matchesCounted: retained, limited: false),
            request: request, rows: rows, nextOffset: end < retained ? end : nil)
    }
    @MainActor private static func occurrenceActivation() async throws {
        // A substring search would spend all 4096 retained hits on these
        // identifier prefixes. The local whole-token inventory has one actual
        // token. This is a fixed engine receipt, not a native token parser.
        let prefix = String(repeating: "foobar ", count: 5000)
        let source = prefix + "foo"
        let start = UInt64(prefix.utf8.count), end = UInt64(source.utf8.count)
        let sourceID = identity(bytes: end, encoding: "utf8")
        let selected = Self.selected(needle: "foo", start: start, end: end, bytes: end, mode: .occurrences)
        let single = referencePage(identity: sourceID, ranges: [(start, end)])
        var prepares = 0, pageCalls = 0, reads = 0
        let activated = try await AtlasCapturedReader.activateOccurrence(selected, identity: sourceID,
            prepare: { name in
                prepares += 1
                check(name.utf8.elementsEqual("foo".utf8), "Pass original token spelling to shared reference engine")
                return single
            }, nextPage: { _ in pageCalls += 1; return single },
            read: { target in reads += 1; return target })
        check(prepares == 1 && pageCalls == 0 && reads == 1,
            "One bounded whole-token inventory activates beyond 4096 substring prefixes")
        check(activated.page.occurrence.id == 1 && activated.page.occurrence.range.start == start
              && activated.page.occurrence.range.end == end && activated.page.generation == 29,
            "Selection uses local reference ID and exact original range, not substring ordinal")
        check(activated.target.pageID == single.id && activated.history.isEmpty,
            "Initial reference page and its selection authority stay together")
        do {
            _ = try AtlasCapturedReader.structuralRequest(selected, identity: sourceID)
            preconditionFailure("Occurrence fell back to an unselected structural byte window")
        } catch AtlasReaderError.invalidResponse { checks += 1 }
        await refused(selected, identity: sourceID)

        let ranges = (0..<4096).map { (UInt64($0 * 4), UInt64($0 * 4 + 3)) }
        let manyID = identity(bytes: 16_384, encoding: "utf8")
        let last = Self.selected(needle: "foo", start: ranges.last!.0, end: ranges.last!.1,
            bytes: manyID.capturedBytes, mode: .occurrences)
        var visited: [AtlasReferencePageRequest] = []
        let paged = try await AtlasCapturedReader.activateOccurrence(last, identity: manyID,
            prepare: { _ in referencePage(identity: manyID, ranges: ranges) },
            nextPage: { request in
                visited.append(request)
                return referencePage(identity: manyID, ranges: ranges, request: request)
            }, read: { $0 })
        check(visited.count == 32 && paged.history.count == 32
              && visited.first == .init(start: 64, limit: 128)
              && paged.page.occurrence.id == 4096 && paged.referencePage.nextOffset == nil,
            "Maximum local inventory is bounded to 33 pages and retains backward page history")
        check(paged.page.pageID == paged.referencePage.id, "Selected later row belongs to the displayed current page")

        for (encoding, name, width) in [
            ("utf8", "éx", UInt64(3)), ("utf16le", "éx", 4), ("utf16be", "éx", 4)
        ] {
            let sourceID = identity(bytes: 20, encoding: encoding)
            let target = Self.selected(needle: name, start: 2, end: 2 + width, mode: .occurrences)
            let local = referencePage(identity: sourceID, name: name, ranges: [(2, 2 + width)])
            let result = try await AtlasCapturedReader.activateOccurrence(target, identity: sourceID,
                prepare: { _ in local }, nextPage: { _ in local }, read: { $0 })
            check(result.page.name.utf8.elementsEqual(name.utf8)
                  && result.page.occurrence.range.end == 2 + width,
                "Original UTF-8/UTF-16 source coordinates survive whole-token activation for " + encoding)
            var selectedRead = false
            do {
                _ = try await AtlasCapturedReader.activateOccurrence(target, identity: sourceID,
                    prepare: { _ in referencePage(identity: sourceID, name: "e\u{301}x", ranges: [(2, 2 + width)]) },
                    nextPage: { _ in local }, read: { _ in selectedRead = true; return "wrong spelling" })
                preconditionFailure("Canonical-equivalent reference name substituted")
            } catch AtlasReaderError.invalidResponse { checks += 1 }
            check(!selectedRead, "Unicode spelling mismatch cannot authorize selection")
        }

        for wrong in [
            referencePage(identity: identity(bytes: end, path: "other.rs", encoding: "utf8"), ranges: [(start, end)]),
            referencePage(identity: sourceID, name: "other", ranges: [(start, end)]),
            referencePage(identity: sourceID, ranges: [(start - 1, end - 1)]),
            referencePage(identity: sourceID, ranges: [(start, end)], generation: 0)
        ] {
            var attemptedRead = false
            do {
                _ = try await AtlasCapturedReader.activateOccurrence(selected, identity: sourceID,
                    prepare: { _ in wrong }, nextPage: { _ in wrong },
                    read: { _ in attemptedRead = true; return "approximate occurrence" })
                preconditionFailure("Invalid local occurrence inventory accepted")
            } catch {}
            check(!attemptedRead, "Wrong source/name/range/generation has no approximate activation")
        }

        let shortRanges = Array(ranges.prefix(70))
        let smallID = identity(bytes: 280, encoding: "utf8")
        let later = Self.selected(needle: "foo", start: 276, end: 279, bytes: 280, mode: .occurrences)
        var crossed = false
        do {
            _ = try await AtlasCapturedReader.activateOccurrence(later, identity: smallID,
                prepare: { _ in referencePage(identity: smallID, ranges: shortRanges) },
                nextPage: { request in
                    referencePage(identity: smallID, ranges: shortRanges, request: request, generation: 30)
                }, read: { _ in crossed = true; return "changed generation" })
            preconditionFailure("Reference pages from two inventories mixed")
        } catch AtlasReaderError.invalidResponse { checks += 1 }
        check(!crossed, "Changed inventory is rejected before retained selection")

        // Source edits or selection replacement revoke the same opening ticket
        // even if a suspended retained operation completes successfully.
        var current = true
        var prepareContinuation: CheckedContinuation<AtlasReferencePage, Never>?
        var staleReads = 0
        let stalePrepare = Task { @MainActor in
            try await AtlasCapturedReader.activateOccurrence(selected, identity: sourceID,
                prepare: { _ in await withCheckedContinuation { prepareContinuation = $0 } },
                nextPage: { _ in single }, read: { _ in staleReads += 1; return "stale" },
                isCurrent: { current })
        }
        while prepareContinuation == nil { await Task.yield() }
        current = false; prepareContinuation!.resume(returning: single)
        do { _ = try await stalePrepare.value; preconditionFailure("Superseded preparation delivered") }
        catch AtlasReaderError.canceled { checks += 1 }
        check(staleReads == 0, "Superseded preparation cannot issue selection")

        current = true
        var nextContinuation: CheckedContinuation<AtlasReferencePage, Never>?
        let staleNext = Task { @MainActor in
            try await AtlasCapturedReader.activateOccurrence(later, identity: smallID,
                prepare: { _ in referencePage(identity: smallID, ranges: shortRanges) },
                nextPage: { _ in await withCheckedContinuation { nextContinuation = $0 } },
                read: { _ in staleReads += 1; return "stale" }, isCurrent: { current })
        }
        while nextContinuation == nil { await Task.yield() }
        current = false
        nextContinuation!.resume(returning: referencePage(identity: smallID, ranges: shortRanges,
            request: .init(start: 64, limit: 128)))
        do { _ = try await staleNext.value; preconditionFailure("Superseded pagination delivered") }
        catch AtlasReaderError.canceled { checks += 1 }
        check(staleReads == 0, "Superseded continuation cannot issue selection")

        current = true
        var readContinuation: CheckedContinuation<String, Never>?
        let staleRead = Task { @MainActor in
            try await AtlasCapturedReader.activateOccurrence(selected, identity: sourceID,
                prepare: { _ in single }, nextPage: { _ in single },
                read: { _ in await withCheckedContinuation { readContinuation = $0 } },
                isCurrent: { current })
        }
        while readContinuation == nil { await Task.yield() }
        current = false; readContinuation!.resume(returning: "late selected page")
        do { _ = try await staleRead.value; preconditionFailure("Superseded occurrence page delivered") }
        catch AtlasReaderError.canceled { checks += 1 }

        var preparedCanceled = false
        let canceled = Task { @MainActor in
            try await AtlasCapturedReader.activateOccurrence(selected, identity: sourceID,
                prepare: { _ in preparedCanceled = true; return single },
                nextPage: { _ in single }, read: { _ in "canceled" })
        }
        canceled.cancel()
        do { _ = try await canceled.value; preconditionFailure("Pre-canceled occurrence work ran") }
        catch is CancellationError { checks += 1 }
        check(!preparedCanceled, "Canceled opening performs no local occurrence extraction")
    }


    @MainActor private static func pagedTextActivation() async throws {
        let needle = String(repeating: "x", count: 4096)
        let witness = String(repeating: "78", count: 4096)
        let sourceID = identity(bytes: 8192, encoding: "utf8")
        for mode in [AtlasSearchMode.text, .expression] {
            let target = selected(needle: needle, start: 4, end: 4100, bytes: 8192,
                mode: mode, caseSensitive: false)
            var cursors: [UInt64] = [], reads = 0
            let activated = try await AtlasCapturedReader.activate(target, identity: sourceID,
                find: { exact in
                    check(exact.utf8.elementsEqual(needle.utf8), "Initial activation uses the exact source spelling")
                    return try report(identity: sourceID, needle: exact, ranges: [(0, 4096), (1, 4097)],
                        hex: witness, complete: false, startAfter: 0, more: true, retainedLimit: 2)
                }, nextPage: { exact, cursor in
                    check(exact.utf8.elementsEqual(needle.utf8), "Continuation preserves exact source spelling")
                    cursors.append(cursor)
                    if cursor == 2 {
                        return try report(identity: sourceID, needle: exact, ranges: [(2, 4098), (3, 4099)],
                            hex: witness, complete: false, generation: 24, startAfter: 2, more: true, retainedLimit: 2)
                    }
                    check(cursor == 4, "Resume after the last global occurrence, not the look-ahead count")
                    return try report(identity: sourceID, needle: exact, ranges: [(4, 4100)],
                        hex: witness, generation: 25, startAfter: 4, retainedLimit: 2)
                }, read: { selected in reads += 1; return selected })
            check(cursors == [2, 4] && reads == 1, "Long witnesses activate beyond the initial retained row allowance")
            check(activated.index == 0 && activated.target.index == 0 && activated.target.generation == 25,
                "Later activation uses the new generation and page-local index")
            check(activated.report.hits[0].occurrence == 5 && activated.target.start == 4
                  && activated.target.end == 4100 && activated.target.originalHex == witness,
                "Only the selected original range and witness authorize the page")
        }
    }

    @MainActor private static func pagedActivationRefusals() async throws {
        let first = try report(ranges: [(0, 2)], complete: false, startAfter: 0, more: true, retainedLimit: 1)
        let wrongPages = try [
            report(ranges: [(10, 12)], generation: 24, startAfter: 0, retainedLimit: 1),
            report(ranges: [(10, 12)], generation: 23, startAfter: 1, retainedLimit: 1),
            report(identity: identity(owner: 101), ranges: [(10, 12)],
                generation: 24, startAfter: 1, retainedLimit: 1),
            report(identity: identity(revision: 2), ranges: [(10, 12)],
                generation: 24, startAfter: 1, retainedLimit: 1),
            report(needle: "y", ranges: [(10, 12)], generation: 24, startAfter: 1, retainedLimit: 1),
            report(ranges: [(10, 12)], generation: 24, startAfter: 1, retainedLimit: 1,
                options: .init(caseSensitive: false, countAllMatches: false)),
            report(ranges: [(10, 12)], generation: 24, startAfter: 1, retainedLimit: 1,
                options: .init(caseSensitive: true, countAllMatches: true))
        ]
        for wrong in wrongPages {
            var reads = 0
            do {
                _ = try await AtlasCapturedReader.activate(selected(), identity: identity(),
                    find: { _ in first }, nextPage: { _, _ in wrong },
                    read: { _ in reads += 1; return "wrong page" })
                preconditionFailure("Changed page authority activated")
            } catch AtlasReaderError.invalidResponse { checks += 1 }
            check(reads == 0, "Wrong cursor, generation, source, query or options cannot read an approximate selection")
        }

        let legacy = try report(ranges: [(0, 2)], complete: false, more: true, retainedLimit: 1)
        let terminal = try report(ranges: [(9, 11)], startAfter: 0, retainedLimit: 1)
        for unavailable in [legacy, terminal] {
            var nextCalls = 0, reads = 0
            do {
                _ = try await AtlasCapturedReader.activate(selected(), identity: identity(),
                    find: { _ in unavailable },
                    nextPage: { _, _ in nextCalls += 1; return first },
                    read: { _ in reads += 1; return "nearby page" })
                preconditionFailure("Unavailable captured range substituted")
            } catch AtlasReaderError.engine(let code) {
                check(code == "CAPTURED_SEARCH_HIT_UNAVAILABLE", "Absent ranges use the explicit unavailable outcome")
            }
            check(nextCalls == 0 && reads == 0, "Legacy and terminal reports authorize neither continuation nor nearby selection")
        }
        do {
            _ = try await AtlasCapturedReader.activate(selected(), identity: identity(),
                find: { _ in first }, read: { _ in "missing continuation" })
            preconditionFailure("Missing continuation capability accepted")
        } catch AtlasReaderError.engine(let code) {
            check(code == "CAPTURED_SEARCH_HIT_UNAVAILABLE", "Single-table callers remain source-compatible and bounded")
        }
    }

    @MainActor private static func pagedActivationBudgets() async throws {
        // Receipts carry these capture sizes; the fixture allocates only its
        // bounded JSON rows, not a giant source or a second search engine.
        for (bytes, admittedPages) in [(UInt64(32 * 1024 * 1024), 2), (UInt64(200), 64)] {
            let sourceID = identity(bytes: bytes)
            for reachesBoundary in [false, true] {
                let desiredPage = reachesBoundary ? admittedPages : admittedPages + 1
                let offset = UInt64((desiredPage - 1) * 2)
                let target = selected(start: offset, end: offset + 2, bytes: bytes)
                var scans = 0, reads = 0
                do {
                    let result = try await AtlasCapturedReader.activate(target, identity: sourceID,
                        find: { _ in
                            scans += 1
                            return try report(identity: sourceID, ranges: [(0, 2)], complete: false,
                                startAfter: 0, more: true, retainedLimit: 1)
                        }, nextPage: { _, cursor in
                            scans += 1
                            return try report(identity: sourceID, ranges: [(cursor * 2, cursor * 2 + 2)],
                                complete: false, generation: 23 + cursor, startAfter: cursor, more: true, retainedLimit: 1)
                        }, read: { target in reads += 1; return target })
                    check(reachesBoundary && result.target.start == offset,
                        "The last admitted scan may still activate its exact selected row")
                } catch AtlasReaderError.engine(let code) {
                    check(!reachesBoundary && code == "CAPTURED_SEARCH_HIT_UNAVAILABLE",
                        "Byte/page exhaustion refuses the next scan explicitly")
                }
                check(scans == admittedPages && reads == (reachesBoundary ? 1 : 0),
                    "Reserve each entire capture before scanning: at most 64 MiB and 64 pages")
            }
        }
        var attempts = 0
        do {
            _ = try await AtlasCapturedReader.activate(selected(bytes: UInt64.max),
                identity: identity(bytes: UInt64.max),
                find: { _ in attempts += 1; return try report() }, read: { _ in "overflow" })
            preconditionFailure("Oversized initial replay admitted")
        } catch AtlasReaderError.engine(let code) {
            check(code == "CAPTURED_SEARCH_HIT_UNAVAILABLE", "Unadmitted initial replay has a bounded failure")
        }
        check(attempts == 0, "Replay admission never overflows or allocates before its byte reservation")
    }

    @MainActor private static func pagedActivationCurrentAndCancellation() async throws {
        let first = try report(ranges: [(0, 2)], complete: false, startAfter: 0, more: true, retainedLimit: 1)
        let second = try report(ranges: [(10, 12)], generation: 24, startAfter: 1, retainedLimit: 1)
        for revokedAt in 0...3 {
            var current = revokedAt != 0
            var finds = 0, pages = 0, reads = 0
            do {
                _ = try await AtlasCapturedReader.activate(selected(), identity: identity(),
                    find: { _ in
                        finds += 1
                        if revokedAt == 1 { current = false }
                        return first
                    }, nextPage: { _, _ in
                        pages += 1
                        if revokedAt == 2 { current = false }
                        return second
                    }, read: { _ in
                        reads += 1
                        if revokedAt == 3 { current = false }
                        return "obsolete selection"
                    }, isCurrent: { current })
                preconditionFailure("Revoked captured activation published")
            } catch AtlasReaderError.canceled { checks += 1 }
            check(finds == (revokedAt == 0 ? 0 : 1) && pages == (revokedAt >= 2 ? 1 : 0)
                  && reads == (revokedAt == 3 ? 1 : 0),
                "Current-owner fences surround initial query, continuation and selected-page delivery")
        }

        var pending: CheckedContinuation<AtlasReaderFindReport, Never>?
        var reads = 0
        let canceled = Task { @MainActor in
            try await AtlasCapturedReader.activate(selected(), identity: identity(),
                find: { _ in first },
                nextPage: { _, _ in await withCheckedContinuation { pending = $0 } },
                read: { _ in reads += 1; return "canceled page" })
        }
        while pending == nil { await Task.yield() }
        canceled.cancel()
        pending!.resume(returning: second)
        do { _ = try await canceled.value; preconditionFailure("Canceled continuation selected a row") }
        catch is CancellationError { checks += 1 }
        check(reads == 0, "Late successful continuation cannot revive a canceled activation")
    }

    @MainActor static func main() async throws {
        try await occurrenceActivation()
        try await pagedTextActivation()
        try await pagedActivationRefusals()
        try await pagedActivationBudgets()
        try await pagedActivationCurrentAndCancellation()
        let imported = try AtlasReaderInput(captured: selected())
        check(imported.fullPath == "src.rs", "Imported label remains the engine's relative source identity")
        let live = try AtlasReaderInput(root: "/repo", path: "src.rs")
        check(live.fullPath == "/repo/src.rs", "Ordinary path opening keeps its original semantics")
        for path in ["", "../src.rs", "a/../b", "a//b", "/src.rs", "a\0b"] {
            do { _ = try AtlasReaderInput(captured: selected(path: path)); preconditionFailure("Invalid captured label") }
            catch AtlasReaderError.invalidInput { checks += 1 }
        }
        // Names and heading syntax do not necessarily equal a literal source
        // substring. Structural navigation uses the qualified retained range.
        let heading = selected(needle: "Deploy!", start: 3, end: 18, bytes: 40, mode: .headings)
        let headingRequest = try AtlasCapturedReader.structuralRequest(heading,
            identity: identity(bytes: 40, encoding: "utf8"))
        if case let .window(offset, bytes) = headingRequest {
            check(offset == 3 && bytes >= 15, "Heading opens its original syntax range without literal re-search")
        } else { preconditionFailure("Structural navigation must request a byte window") }
        try headingRequest.validate(capturedBytes: 40)
        let symbol = selected(needle: "run", start: 8, end: 14, bytes: 40, mode: .symbols)
        let symbolRequest = try AtlasCapturedReader.structuralRequest(symbol,
            identity: identity(bytes: 40, encoding: "utf8"))
        if case let .window(offset, bytes) = symbolRequest {
            check(offset == 8 && bytes >= 6, "Prefix symbol result retains the full selected name")
        } else { preconditionFailure("Symbol navigation must request a byte window") }
        let maximum = AtlasReaderLimits.maximumPageBytes
        let full = selected(start: 0, end: maximum, bytes: maximum, mode: .headings)
        let fullRequest = try AtlasCapturedReader.structuralRequest(full, identity: identity(bytes: maximum))
        if case let .window(offset, bytes) = fullRequest {
            check(offset == 0 && bytes == maximum, "A full admitted heading capture fits the reader window cap")
        } else { preconditionFailure("Full-capacity heading lost its exact window") }
        try fullRequest.validate(capturedBytes: maximum)
        for invalid in [
            selected(mode: .text),
            selected(mode: .expression),
            selected(mode: .occurrences),
            selected(path: "other.rs", mode: .headings),
            selected(start: 12, end: 10, mode: .symbols),
            selected(start: 10, end: 21, mode: .headings),
            selected(start: 0, end: maximum + 1, bytes: maximum + 1, mode: .headings)
        ] {
            do {
                _ = try AtlasCapturedReader.structuralRequest(invalid,
                    identity: identity(bytes: invalid.capturedBytes))
                preconditionFailure("Invalid structural source range accepted")
            } catch AtlasReaderError.invalidResponse { checks += 1 }
        }
        do {
            _ = try AtlasCapturedReader.structuralRequest(selected(path: "é.rs", mode: .headings),
                identity: identity(path: "e\u{301}.rs"))
            preconditionFailure("Canonically equivalent paths substituted in structural activation")
        } catch AtlasReaderError.invalidResponse { checks += 1 }
        await refused(selected(mode: .headings))
        await refused(selected(mode: .symbols))
        let calls = Calls()
        let base = AtlasReaderTransport(create: { calls.add("create"); return 100 },
            open: { _, _, _ in calls.add("live-open"); return "wrong live bytes" },
            window: { _, _, _ in calls.add("window"); return "window" },
            lines: { _, _, _, _ in calls.add("lines"); return "lines" },
            cancel: { _ in calls.add("cancel"); return true }, close: { _ in calls.add("close"); return true },
            retirementFailed: { calls.add("failed") },
            fork: { source, destination in
                precondition(source == 100 && destination == 101)
                calls.add("fork"); return "independent captured info"
            }, openBytes: { _, label, bytes in
                precondition(label == "supplied" && bytes == Data("abc".utf8))
                calls.add("supplied"); return .init(json: "supplied info", sha256: "fixture digest")
            })
        let target = AtlasSearchCapturedHit(root: "/repo", path: "src.rs", needle: "x", start: 10, end: 12,
            capturedBytes: 20, openReader: { handle in
                precondition(handle == 100); calls.add("import"); return "exact captured info"
            })
        let transport = base.importing(target)
        check(transport.create() == 100, "Receiver allocated through its existing registry")
        check(transport.open(100, "src.rs", 100) == "exact captured info", "Open delegates only to captured import")
        check(transport.open(100, "other.rs", 100) == nil, "Wrong label cannot activate captured capability")
        check(calls.get("live-open") == 0 && calls.get("import") == 1, "No live open on success or label refusal")
        check(transport.window(100, 0, 4) == "window" && transport.lines(100, 1, 1, 4) == "lines", "Later reads use unchanged retained-reader operations")
        check(transport.cancel(100) && transport.close(100), "Reader cancellation/retirement remains independent")
        check(transport.fork?(100, 101) == "independent captured info" && calls.get("fork") == 1,
              "Captured-search transport lost independent pin capability")
        check(transport.openBytes?(101, "supplied", Data("abc".utf8))?.json == "supplied info"
              && calls.get("supplied") == 1, "Captured-search transport lost supplied-byte capability")
        let failure = AtlasSearchCapturedHit(root: "/repo", path: "src.rs", needle: "x", start: 10, end: 12,
            capturedBytes: 20, openReader: { _ in throw AtlasSearchError.invalidResponse })
        check(base.importing(failure).open(100, "src.rs", 20) == nil && calls.get("live-open") == 0,
              "Failed or corrupt handoff never falls back to disk")

        var chosen: AtlasReaderHitTarget?
        let activated = try await AtlasCapturedReader.activate(selected(), identity: identity(),
            find: { needle in
                check(needle == "x", "Original exact query bytes passed through")
                return try report()
            }, read: { target in chosen = target; return "page from reader" })
        check(activated.page == "page from reader" && activated.index == 1, "Select exact later occurrence, not the first matching word")
        check(chosen?.generation == 23 && chosen?.index == 1, "Selection uses the new reader's query namespace")
        check(chosen?.start == 10 && chosen?.end == 12 && chosen?.originalHex == "7800", "Original UTF-16 byte evidence is preserved")
        let expressionSource = identity(bytes: 64)
        let expressionTarget = selected(needle: "pub fn", start: 30, end: 42, bytes: 64, mode: .expression)
        let expression = try await AtlasCapturedReader.activate(expressionTarget, identity: expressionSource,
            find: { primary in
                check(primary == "pub fn", "Expression activation searches only the exact primary source text")
                return try report(identity: expressionSource, needle: primary, ranges: [(2, 14), (30, 42)],
                    hex: "700075006200200066006e00")
            }, read: { $0 })
        check(expression.index == 1 && expression.page.generation == 23,
            "Expression selection rebinds the later occurrence in the retained reader's namespace")
        check(expression.page.start == 30 && expression.page.end == 42
              && expression.page.originalHex == "700075006200200066006e00",
            "Expression activation preserves original UTF-16 phrase bytes")
        let partial = try await AtlasCapturedReader.activate(selected(), identity: identity(),
            find: { _ in try report(complete: false) }, read: { $0.index })
        check(partial.page == 1 && !partial.report.complete, "A verified retained hit is usable even when total count is partial")
        for (encoding, start, end, needle, hex) in [
            ("utf8", UInt64(3), UInt64(7), "🎵", "f09f8eb5"),
            ("utf16le", 4, 8, "🎵", "3cd8b5df"),
            ("utf16be", 4, 8, "🎵", "d83cdfb5"),
            ("utf8", 2, 3, "x", "78")] {
            let request = selected(needle: needle, start: start, end: end)
            let source = identity(encoding: encoding)
            let value = try await AtlasCapturedReader.activate(request, identity: source,
                find: { _ in try report(identity: source, needle: needle, ranges: [(start, end)], hex: hex) },
                read: { $0.originalHex })
            check(value.page == hex, "Decoder-owned witness preserved for " + encoding)
        }
        for (encoding, query, actualText, hex, count) in [
            ("utf8", "foo", "FOO", "464f4f", UInt64(3)),
            ("utf8", "k", "K", "e284aa", 3),
            ("utf8", "i\u{307}", "İ", "c4b0", 2),
            ("utf16le", "i\u{307}", "İ", "3001", 2),
            ("utf16be", "i\u{307}", "İ", "0130", 2)
        ] {
            let source = identity(encoding: encoding)
            for mode in [AtlasSearchMode.text, .expression] {
                let request = selected(needle: actualText, start: 2, end: 2 + count,
                    mode: mode, caseSensitive: false)
                let value = try await AtlasCapturedReader.activate(request, identity: source,
                    find: { exact in
                        check(exact.utf8.elementsEqual(actualText.utf8) && !exact.utf8.elementsEqual(query.utf8),
                            "Insensitive activation performs exact Find using the engine's source spelling")
                        return try report(identity: source, needle: actualText, ranges: [(2, 2 + count)], hex: hex)
                    }, read: { $0 })
                check(value.page.start == 2 && value.page.end == 2 + count && value.page.originalHex == hex,
                    "Insensitive activation preserves original-byte evidence for " + encoding + " " + mode.rawValue)
            }
        }
        await refused(identity: identity(bytes: 21))
        await refused(identity: identity(path: "other.rs"))
        await refused(result: { try report(identity: identity(owner: 101)) })
        await refused(result: { try report(identity: identity(revision: 2)) })
        await refused(result: { try report(needle: "y") })
        await refused(result: { try report(ranges: [(2, 4), (11, 13)]) })
        await refused(result: { try report(ranges: [(10, 12), (10, 12)]) })
        await refused(result: { try report(hex: nil) })
        await refused(selected(start: 12, end: 10))
        await refused(selected(start: 10, end: 21))
        await refused(selected(needle: ""))
        await refused(selected(needle: "e\u{301}"), result: { try report(needle: "é") })
        await refused(selected(path: "é.rs"), identity: identity(path: "e\u{301}.rs"))
        let many = (0..<4097).map { _ in (UInt64(10), UInt64(12)) }
        await refused(result: { try report(ranges: many) })
        var findCalled = false
        let canceled = Task { @MainActor in
            try await AtlasCapturedReader.activate(selected(), identity: identity(),
                find: { _ in findCalled = true; return try report() }, read: { _ in "must not read" })
        }
        canceled.cancel()
        do { _ = try await canceled.value; preconditionFailure("Pre-cancel ignored") }
        catch is CancellationError { checks += 1 }
        check(!findCalled, "Pre-cancellation prevents even retained-file query")
        var continuation: CheckedContinuation<AtlasReaderFindReport, Never>?
        var reads = 0
        let mid = Task { @MainActor in
            try await AtlasCapturedReader.activate(selected(), identity: identity(),
                find: { _ in await withCheckedContinuation { continuation = $0 } },
                read: { _ in reads += 1; return "wrong page" })
        }
        while continuation == nil { await Task.yield() }
        mid.cancel(); continuation!.resume(returning: try report())
        do { _ = try await mid.value; preconditionFailure("Mid-query cancellation ignored") }
        catch is CancellationError { checks += 1 }
        check(reads == 0, "Canceled find cannot become a later native selection")
        var pageContinuation: CheckedContinuation<String, Never>?
        let late = Task { @MainActor in
            try await AtlasCapturedReader.activate(selected(), identity: identity(),
                find: { _ in try report() }, read: { _ in await withCheckedContinuation { pageContinuation = $0 } })
        }
        while pageContinuation == nil { await Task.yield() }
        late.cancel(); pageContinuation!.resume(returning: "stale page")
        do { _ = try await late.value; preconditionFailure("Late cancellation ignored") }
        catch is CancellationError { checks += 1 }
        print("AtlasCapturedReaderTests: \(checks) checks passed")
    }
}
