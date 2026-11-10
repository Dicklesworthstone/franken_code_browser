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
                               hex: String? = "7800", complete: Bool = true) -> AtlasReaderFindReport {
        .init(identity: identity, generation: 23, needle: needle, complete: complete,
            state: complete ? "complete-observed-input" : "match-limit", matchesSeen: UInt64(ranges.count),
            hits: ranges.enumerated().map { i, range in
                .init(index: UInt64(i), occurrence: UInt64(i + 1), start: range.0, end: range.1)
            }, originalHex: hex)
    }
    @MainActor private static func refused(_ target: AtlasSearchCapturedHit = selected(),
                                          identity: AtlasReaderIdentity = identity(),
                                          result: AtlasReaderFindReport = report()) async {
        var readCount = 0
        do {
            _ = try await AtlasCapturedReader.activate(target, identity: identity,
                find: { _ in result }, read: { _ in readCount += 1; return "wrong page" })
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

    @MainActor static func main() async throws {
        try await occurrenceActivation()
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
                return report()
            }, read: { target in chosen = target; return "page from reader" })
        check(activated.page == "page from reader" && activated.index == 1, "Select exact later occurrence, not the first matching word")
        check(chosen?.generation == 23 && chosen?.index == 1, "Selection uses the new reader's query namespace")
        check(chosen?.start == 10 && chosen?.end == 12 && chosen?.originalHex == "7800", "Original UTF-16 byte evidence is preserved")
        let expressionSource = identity(bytes: 64)
        let expressionTarget = selected(needle: "pub fn", start: 30, end: 42, bytes: 64, mode: .expression)
        let expression = try await AtlasCapturedReader.activate(expressionTarget, identity: expressionSource,
            find: { primary in
                check(primary == "pub fn", "Expression activation searches only the exact primary source text")
                return report(identity: expressionSource, needle: primary, ranges: [(2, 14), (30, 42)],
                    hex: "700075006200200066006e00")
            }, read: { $0 })
        check(expression.index == 1 && expression.page.generation == 23,
            "Expression selection rebinds the later occurrence in the retained reader's namespace")
        check(expression.page.start == 30 && expression.page.end == 42
              && expression.page.originalHex == "700075006200200066006e00",
            "Expression activation preserves original UTF-16 phrase bytes")
        let partial = try await AtlasCapturedReader.activate(selected(), identity: identity(),
            find: { _ in report(complete: false) }, read: { $0.index })
        check(partial.page == 1 && !partial.report.complete, "A verified retained hit is usable even when total count is partial")
        for (encoding, start, end, needle, hex) in [
            ("utf8", UInt64(3), UInt64(7), "🎵", "f09f8eb5"),
            ("utf16le", 4, 8, "🎵", "3cd8b5df"),
            ("utf16be", 4, 8, "🎵", "d83cdfb5"),
            ("utf8", 2, 3, "x", "78")] {
            let request = selected(needle: needle, start: start, end: end)
            let source = identity(encoding: encoding)
            let value = try await AtlasCapturedReader.activate(request, identity: source,
                find: { _ in report(identity: source, needle: needle, ranges: [(start, end)], hex: hex) },
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
                        return report(identity: source, needle: actualText, ranges: [(2, 2 + count)], hex: hex)
                    }, read: { $0 })
                check(value.page.start == 2 && value.page.end == 2 + count && value.page.originalHex == hex,
                    "Insensitive activation preserves original-byte evidence for " + encoding + " " + mode.rawValue)
            }
        }
        await refused(identity: identity(bytes: 21))
        await refused(identity: identity(path: "other.rs"))
        await refused(result: report(identity: identity(owner: 101)))
        await refused(result: report(identity: identity(revision: 2)))
        await refused(result: report(needle: "y"))
        await refused(result: report(ranges: [(2, 4), (11, 13)]))
        await refused(result: report(ranges: [(10, 12), (10, 12)]))
        await refused(result: report(hex: nil))
        await refused(selected(start: 12, end: 10))
        await refused(selected(start: 10, end: 21))
        await refused(selected(needle: ""))
        await refused(selected(needle: "e\u{301}"), result: report(needle: "é"))
        await refused(selected(path: "é.rs"), identity: identity(path: "e\u{301}.rs"))
        let many = (0..<4097).map { _ in (UInt64(10), UInt64(12)) }
        await refused(result: report(ranges: many))
        var findCalled = false
        let canceled = Task { @MainActor in
            try await AtlasCapturedReader.activate(selected(), identity: identity(),
                find: { _ in findCalled = true; return report() }, read: { _ in "must not read" })
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
        mid.cancel(); continuation!.resume(returning: report())
        do { _ = try await mid.value; preconditionFailure("Mid-query cancellation ignored") }
        catch is CancellationError { checks += 1 }
        check(reads == 0, "Canceled find cannot become a later native selection")
        var pageContinuation: CheckedContinuation<String, Never>?
        let late = Task { @MainActor in
            try await AtlasCapturedReader.activate(selected(), identity: identity(),
                find: { _ in report() }, read: { _ in await withCheckedContinuation { pageContinuation = $0 } })
        }
        while pageContinuation == nil { await Task.yield() }
        late.cancel(); pageContinuation!.resume(returning: "stale page")
        do { _ = try await late.value; preconditionFailure("Late cancellation ignored") }
        catch is CancellationError { checks += 1 }
        print("AtlasCapturedReaderTests: \(checks) checks passed")
    }
}
