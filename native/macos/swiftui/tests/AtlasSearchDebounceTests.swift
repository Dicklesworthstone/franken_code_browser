// Portable production-scheduler tests; no Apple UI, filesystem or Rust bridge.
// swiftc -swift-version 6 AtlasSearch.swift AtlasSearchCoordinator.swift \
//   tests/AtlasSearchDebounceTests.swift -o /tmp/fcb-search-debounce-tests
import Foundation
import Dispatch

private final class Calls: @unchecked Sendable {
    private let lock = NSLock()
    private var values: [String] = []
    func record(_ value: String) { lock.lock(); values.append(value); lock.unlock() }
    var snapshot: [String] { lock.lock(); defer { lock.unlock() }; return values }
}

private func report() -> AtlasSearchReport {
    AtlasSearchReport(hits: [], complete: true, truncated: false,
        unavailableFiles: 0, unsupportedFiles: 0, matchesSeen: 0)
}

@main struct AtlasSearchDebounceTests {
    @MainActor static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        precondition(condition(), message)
    }

    @MainActor static func pump(for duration: TimeInterval) {
        let end = Date().addingTimeInterval(duration)
        while Date() < end { _ = RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.005)) }
    }

    @MainActor static func wait(_ condition: () -> Bool, _ message: String) {
        let end = Date().addingTimeInterval(3)
        while !condition(), Date() < end { pump(for: 0.005) }
        check(condition(), message)
    }

    @MainActor static func main() throws {
        // A typing burst admits only the newest immutable input. No scan is
        // queued until the quiet period, even with an idle bridge worker.
        do {
            let calls = Calls()
            var delivered = 0
            let coordinator = AtlasSearchCoordinator { _, query, _ in calls.record(query); return report() }
            for query in ["a", "ab", "abc"] {
                try coordinator.submit(root: "/project", query: query, debounce: true) { _ in delivered += 1 }
            }
            check(calls.snapshot.isEmpty, "typing started a bridge call synchronously")
            wait({ delivered == 1 }, "debounced search did not finish")
            check(calls.snapshot == ["abc"], "superseded typing reached the engine")
            pump(for: 0.2)
            check(delivered == 1, "timer delivered the same report twice")
        }

        // Return/Search bypasses debounce and disarms the pending timer.
        do {
            let calls = Calls()
            var delivered = 0
            let coordinator = AtlasSearchCoordinator { _, query, _ in calls.record(query); return report() }
            try coordinator.submit(root: "/project", query: "typing", debounce: true) { _ in delivered += 100 }
            try coordinator.submit(root: "/project", query: "explicit") { _ in delivered += 1 }
            wait({ delivered == 1 }, "explicit search did not finish")
            pump(for: 0.2)
            check(calls.snapshot == ["explicit"] && delivered == 1, "explicit submission left delayed work alive")
        }

        // Clearing/canceling before the deadline performs no engine work and
        // publishes no synthetic empty-success report.
        do {
            let calls = Calls()
            var delivered = false
            let coordinator = AtlasSearchCoordinator { _, query, _ in calls.record(query); return report() }
            try coordinator.submit(root: "/project", query: "canceled", debounce: true) { _ in delivered = true }
            coordinator.cancel()
            pump(for: 0.25)
            check(calls.snapshot.isEmpty && !delivered, "canceled debounce leaked work or a completion")
            try coordinator.submit(root: "/project", query: "reused", debounce: true) { _ in delivered = true }
            wait({ delivered }, "cancel left the reusable timer disabled")
            check(calls.snapshot == ["reused"], "reused timer ran the wrong input")
        }

        // Invalid input cannot cancel a previously admitted query.
        do {
            let calls = Calls()
            var delivered = false
            let coordinator = AtlasSearchCoordinator { _, query, _ in calls.record(query); return report() }
            try coordinator.submit(root: "/project", query: "accepted", debounce: true) { _ in delivered = true }
            do {
                try coordinator.submit(root: "/project", query: "", debounce: true) { _ in fatalError("invalid completion") }
                fatalError("empty query was admitted")
            } catch AtlasSearchError.invalidRequest { }
            wait({ delivered }, "invalid request disturbed admitted work")
            check(calls.snapshot == ["accepted"], "invalid query reached the engine")
        }

        // Replacement during a foreign call invalidates its delivery immediately,
        // but does not start a second call or lose the newest pending request.
        do {
            let calls = Calls()
            let gate = DispatchSemaphore(value: 0)
            let started = DispatchSemaphore(value: 0)
            var delivered: [String] = []
            let coordinator = AtlasSearchCoordinator { _, query, _ in
                calls.record(query)
                if query == "active" {
                    started.signal()
                    precondition(gate.wait(timeout: .now() + .seconds(3)) == .success)
                }
                return report()
            }
            try coordinator.submit(root: "/project", query: "active") { _ in delivered.append("active") }
            check(started.wait(timeout: .now() + .seconds(3)) == .success, "first worker did not start")
            try coordinator.submit(root: "/project", query: "superseded", debounce: true) { _ in delivered.append("superseded") }
            try coordinator.submit(root: "/project", query: "latest", debounce: true) { _ in delivered.append("latest") }
            check(calls.snapshot == ["active"], "replacement ran alongside the active bridge")
            gate.signal()
            wait({ delivered == ["latest"] }, "latest debounced replacement was lost")
            check(calls.snapshot == ["active", "latest"], "pending replacement was not coalesced")
        }

        // An old worker can outlive the quiet period. Finishing it must drain
        // the newest request immediately rather than requiring another timer.
        do {
            let calls = Calls()
            let gate = DispatchSemaphore(value: 0)
            let started = DispatchSemaphore(value: 0)
            var delivered = false
            let coordinator = AtlasSearchCoordinator { _, query, _ in
                calls.record(query)
                if query == "slow" {
                    started.signal()
                    precondition(gate.wait(timeout: .now() + .seconds(3)) == .success)
                }
                return report()
            }
            try coordinator.submit(root: "/project", query: "slow") { _ in fatalError("stale completion") }
            check(started.wait(timeout: .now() + .seconds(3)) == .success, "slow worker did not start")
            try coordinator.submit(root: "/project", query: "next", debounce: true) { _ in delivered = true }
            pump(for: 0.2)
            check(calls.snapshot == ["slow"], "quiet period started overlapping work")
            gate.signal()
            wait({ delivered }, "expired pending request was stranded")
            check(calls.snapshot == ["slow", "next"], "wrong pending input ran after slow work")
        }

        // Destroying the owner also destroys pending work; the timer cannot
        // retain a view/coordinator or publish after the window closes.
        do {
            let calls = Calls()
            var coordinator: AtlasSearchCoordinator? = AtlasSearchCoordinator { _, query, _ in
                calls.record(query); return report()
            }
            weak var weakCoordinator = coordinator
            try coordinator?.submit(root: "/project", query: "closed", debounce: true) { _ in fatalError("closed completion") }
            coordinator = nil
            check(weakCoordinator == nil, "debounce timer retained its owner")
            pump(for: 0.2)
            check(calls.snapshot.isEmpty, "closed window still launched a scan")
        }
        print("PASS: 7 AtlasSearch debounce/coalescing/cancellation scenarios")
    }
}
