// Production mode/refresh selection over the actual captured-index worker.
// Scripted engine envelopes do not qualify Rust execution or native rendering.
import Foundation

private final class LiveCalls: @unchecked Sendable {
    private let lock = NSLock()
    private var count = 0
    var calls: Int { lock.lock(); defer { lock.unlock() }; return count }
    var work: AtlasWorkspaceSearch.Work {
        { _, _, flag, _, _ in
            if flag.isCanceled { throw AtlasSearchError.canceled }
            self.lock.lock(); self.count += 1; self.lock.unlock()
            return AtlasSearchReport(hits: [], complete: true, truncated: false,
                unavailableFiles: 0, unsupportedFiles: 0, matchesSeen: 0)
        }
    }
}

@main private enum AtlasWorkspaceSearchTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        precondition(condition(), message); checks += 1
    }
    private static func run(_ choice: AtlasWorkspaceSearch.Choice, query: String = "x",
                            root: String = "/repo", grant: AnyObject? = nil) throws -> AtlasSearchReport {
        try choice.work(root, query, AtlasSearchCancellation(), AtlasSearchAccessLease(grant), { _ in })
    }
    @MainActor static func main() throws {
        do {
            let f = IndexedFixture(), live = LiveCalls()
            var made = 0
            let service = AtlasWorkspaceSearch(live: live.work, makeIndex: {
                made += 1; return AtlasIndexedSearch(transport: f.transport)
            })
            check(made == 0 && live.calls == 0 && f.count("create:") == 0, "Construction is inert")
            let defaultChoice = service.selection(indexed: false)
            check(!defaultChoice.indexed && service.accepts(defaultChoice), "Live is the default explicit choice")
            check(made == 0 && live.calls == 0, "Selecting live work neither captures nor runs a query")
            let liveResult = try run(defaultChoice)
            check(live.calls == 1 && liveResult.indexStatus == nil && made == 0, "Live query does not construct an index")
            service.configure(indexed: false)
            check(service.accepts(defaultChoice), "Same mode does not invalidate the active choice")
            service.configure(indexed: true)
            check(!service.accepts(defaultChoice) && made == 0, "Mode change revokes delivery without doing source work")
            let firstChoice = service.selection(indexed: true)
            check(firstChoice.indexed && made == 1 && f.count("create:") == 0, "First indexed choice constructs only an inert owner")
            let first = try run(firstChoice)
            check(first.complete && first.indexStatus?.reused == false && live.calls == 1, "Captured mode runs the existing index worker")
            let secondChoice = service.selection(indexed: true)
            let second = try run(secondChoice, query: "another query")
            check(made == 1 && second.indexStatus?.reused == true, "Query edits retain the same index worker")
            check(f.count("open:") == 1 && f.count("build-step:") == 2, "Second query does not rediscover or recapture")
            check(service.accepts(firstChoice) && service.accepts(secondChoice), "Query freshness remains the coordinator's independent responsibility")
            let selected = first.capture!.target(first.hits[1])!
            service.refresh()
            check(!service.accepts(firstChoice) && !service.accepts(secondChoice), "Refresh revokes prior delivery choices")
            check(made == 1 && f.count("create:") == 1, "Refresh itself does not allocate a foreign session")
            f.configure { $0.digest = String(repeating: "b", count: 64) }
            let refreshedChoice = service.selection(indexed: true)
            let refreshed = try run(refreshedChoice)
            check(made == 2 && f.count("create:") == 2, "Next refreshed query gets a distinct captured source universe")
            check(refreshed.hits[0].captureSHA256 != first.hits[0].captureSHA256, "Refresh does not mutate a previous report's witnesses")
            _ = try selected.openReader(100)
            check(f.count("source:") == 1, "An earlier selected source still imports after refresh")
            check(service.accepts(refreshedChoice), "Fresh choice is accepted")
            service.configure(indexed: false)
            check(!service.accepts(refreshedChoice), "Leaving captured mode revokes its late results")
            let currentLive = service.selection(indexed: false)
            _ = try run(currentLive)
            check(live.calls == 2 && made == 2, "Live mode returns to the existing live worker")
            // A stale choice is an immutable operation, not a routing pointer.
            // It can finish on its old worker, but accepts() forbids delivery.
            let staleResult = try run(refreshedChoice)
            check(staleResult.indexStatus?.reused == true && !service.accepts(refreshedChoice), "Mode change cannot reroute in-flight work")
            let back = service.selection(indexed: true)
            _ = try run(back)
            check(made == 3 && f.count("create:") == 3, "Re-entering captured mode does not silently revive a discarded cache")
            service.refresh(); service.refresh()
            check(!service.accepts(back) && made == 3, "Repeated refresh remains inert until a valid query is submitted")
        }
        do {
            let f = IndexedFixture(), live = LiveCalls()
            let a = AtlasWorkspaceSearch(live: live.work, makeIndex: { AtlasIndexedSearch(transport: f.transport) })
            let b = AtlasWorkspaceSearch(live: live.work, makeIndex: { AtlasIndexedSearch(transport: f.transport) })
            let ac = a.selection(indexed: true), bc = b.selection(indexed: true)
            check(!a.accepts(bc) && !b.accepts(ac), "Window-local choices cannot authorize another window")
            _ = try run(ac); _ = try run(bc)
            a.refresh()
            check(!a.accepts(ac) && b.accepts(bc), "Refreshing one window preserves the other window")
            let result = try run(bc)
            check(result.indexStatus?.reused == true && f.count("create:") == 2, "Other window's index is not rebuilt")
        }
        do {
            let f = IndexedFixture(), live = LiveCalls()
            var service: AtlasWorkspaceSearch? = AtlasWorkspaceSearch(live: live.work,
                makeIndex: { AtlasIndexedSearch(transport: f.transport) })
            weak var weakService = service
            let choice = service!.selection(indexed: true)
            service = nil
            check(weakService == nil, "Frozen work does not retain its dismissed UI owner")
            let result = try run(choice)
            check(result.complete, "An admitted request can drain after UI owner dismissal")
        }
        do {
            final class Grant {}
            let f = IndexedFixture(), live = LiveCalls(), grant = Grant(), replacement = Grant()
            let service = AtlasWorkspaceSearch(live: live.work, makeIndex: { AtlasIndexedSearch(transport: f.transport) })
            let choice = service.selection(indexed: true)
            _ = try run(choice, grant: grant); _ = try run(choice, grant: grant)
            check(f.count("create:") == 1, "Same grant and root reuse admitted source")
            _ = try run(choice, grant: replacement)
            check(f.count("create:") == 2, "A new root-access lease identity never borrows the old grant")
            _ = try run(choice, root: "/re\u{301}po", grant: replacement)
            _ = try run(choice, root: "/répo", grant: replacement)
            check(f.count("create:") == 4, "Source roots are compared by encoded bytes")
        }
        do {
            let f = IndexedFixture(), live = LiveCalls()
            let service = AtlasWorkspaceSearch(live: live.work, makeIndex: { AtlasIndexedSearch(transport: f.transport) })
            let choice = service.selection(indexed: true)
            let flag = AtlasSearchCancellation(); flag.cancel()
            do {
                _ = try choice.work("/repo", "x", flag, AtlasSearchAccessLease(nil), { _ in })
                preconditionFailure("Canceled input admitted")
            } catch AtlasSearchError.canceled { checks += 1 }
            check(f.count("create:") == 0 && service.accepts(choice), "Cancellation is independent of mode authority")
            do { _ = try run(choice, query: ""); preconditionFailure("Empty query admitted") }
            catch AtlasSearchError.invalidRequest { checks += 1 }
            check(f.count("create:") == 0, "Invalid query does not create captured source")
            let retried = try run(choice)
            check(retried.complete && f.count("create:") == 1, "Valid retry works without changing mode")
        }
        print("AtlasWorkspaceSearchTests: \(checks) checks passed")
    }
}
