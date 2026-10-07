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
                                 start: UInt64 = 10, end: UInt64 = 12, bytes: UInt64 = 20) -> AtlasSearchCapturedHit {
        .init(root: root, path: path, needle: needle, start: start, end: end, capturedBytes: bytes, openReader: { _ in "captured-info" })
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
    @MainActor static func main() async throws {
        let imported = try AtlasReaderInput(captured: selected())
        check(imported.fullPath == "src.rs", "Imported label remains the engine's relative source identity")
        let live = try AtlasReaderInput(root: "/repo", path: "src.rs")
        check(live.fullPath == "/repo/src.rs", "Ordinary path opening keeps its original semantics")
        for path in ["", "../src.rs", "a/../b", "a//b", "/src.rs", "a\0b"] {
            do { _ = try AtlasReaderInput(captured: selected(path: path)); preconditionFailure("Invalid captured label") }
            catch AtlasReaderError.invalidInput { checks += 1 }
        }
        let calls = Calls()
        let base = AtlasReaderTransport(create: { calls.add("create"); return 100 },
            open: { _, _, _ in calls.add("live-open"); return "wrong live bytes" },
            window: { _, _, _ in calls.add("window"); return "window" },
            lines: { _, _, _, _ in calls.add("lines"); return "lines" },
            cancel: { _ in calls.add("cancel"); return true }, close: { _ in calls.add("close"); return true },
            retirementFailed: { calls.add("failed") })
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