// Portable Foundation/Dispatch tests. No Rust ABI or macOS presentation claim.
import Foundation
import Dispatch

private final class Gates: @unchecked Sendable {
    let offered = DispatchSemaphore(value: 0)
    let finish = DispatchSemaphore(value: 0)
}
private final class Lease {}

@main private enum AtlasSearchProgressTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        precondition(condition(), message)
        checks += 1
    }
    @MainActor private static func pump(until condition: () -> Bool) {
        let deadline = Date().addingTimeInterval(5)
        while !condition() && Date() < deadline {
            _ = RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.005))
        }
        check(condition(), "Timed out awaiting main-thread delivery")
    }
    @MainActor private static func settle() {
        let deadline = Date().addingTimeInterval(0.03)
        while Date() < deadline { _ = RunLoop.current.run(mode: .default, before: deadline) }
    }
    private static func report(_ count: UInt64 = 0, running: Bool = false, id: UUID? = nil) -> AtlasSearchReport {
        AtlasSearchReport(hits: [], complete: !running, truncated: false, unavailableFiles: 0,
            unsupportedFiles: 0, matchesSeen: count,
            progress: AtlasSearchProgress(isRunning: running, examinedFiles: Int(count), cataloguedFiles: 4096), streamID: id)
    }

    @MainActor static func main() throws {
        check(Thread.isMainThread, "Tests exercise the real main-thread preconditions")
        do {
            var results: [AtlasSearchReport] = []
            let coordinator = AtlasSearchCoordinator(work: { _, _, _ in report() })
            try coordinator.submit(root: "/project", query: "literal") { result in
                if case .success(let value) = result { results.append(value) }
            }
            pump { results.count == 1 }
            check(!results[0].isInProgress, "Legacy work still delivers one terminal result")
        }
        do {
            let gate = Gates(), id = UUID()
            var results: [AtlasSearchReport] = []
            let coordinator = AtlasSearchCoordinator(progressiveWork: { _, _, _, publish in
                publish(report(1, running: true, id: id)); gate.offered.signal()
                gate.finish.wait()
                return report(2, id: id)
            })
            try coordinator.submit(root: "/project", query: "x") { result in
                if case .success(let value) = result { results.append(value) }
            }
            pump { results.count == 1 }
            check(results[0].isInProgress, "Useful progress precedes terminal completion")
            check(results[0].summary.contains("provisional"), "Running is not reported as exhaustive")
            check(results[0].summary.contains("1 of 4096"), "Progress exposes catalog coverage")
            gate.finish.signal(); pump { results.count == 2 }
            check(!results[1].isInProgress, "Final delivery retires busy state")
            check(results.allSatisfy { $0.streamID == id }, "One stream keeps its identity")
            settle(); check(results.count == 2, "No late progress follows terminal result")
        }
        do {
            let gate = Gates()
            var results: [AtlasSearchReport] = []
            let coordinator = AtlasSearchCoordinator(progressiveWork: { _, _, _, publish in
                for count in 1...4096 { publish(report(UInt64(count), running: true)) }
                gate.offered.signal(); gate.finish.wait()
                return report(4096)
            })
            try coordinator.submit(root: "/project", query: "burst") { result in
                if case .success(let value) = result { results.append(value) }
            }
            check(gate.offered.wait(timeout: .now() + 5) == .success, "Worker can offer while UI is blocked")
            pump { !results.isEmpty }
            check(results.count == 1 && results[0].matchesSeen == 4096,
                  "4096 offers coalesce into one latest report, not a queue backlog")
            gate.finish.signal(); pump { results.count == 2 }
        }
        do {
            let gate = Gates()
            var delivered = 0
            var finished = false
            let coordinator = AtlasSearchCoordinator(progressiveWork: { _, query, _, publish in
                if query == "old" {
                    publish(report(1, running: true)); gate.offered.signal(); gate.finish.wait()
                }
                return report()
            })
            try coordinator.submit(root: "/project", query: "old") { _ in delivered += 1 }
            check(gate.offered.wait(timeout: .now() + 5) == .success, "Old progress is queued")
            coordinator.cancel()
            gate.finish.signal()
            try coordinator.submit(root: "/project", query: "new") { _ in finished = true }
            pump { finished }
            check(delivered == 0, "Canceled queued progress and terminal result are both silent")
        }
        do {
            let gate = Gates()
            var old = 0, middle = 0, new = 0
            let coordinator = AtlasSearchCoordinator(progressiveWork: { _, query, _, publish in
                if query == "old" {
                    publish(report(1, running: true)); gate.offered.signal(); gate.finish.wait()
                }
                return report()
            })
            try coordinator.submit(root: "/project", query: "old") { _ in old += 1 }
            check(gate.offered.wait(timeout: .now() + 5) == .success, "Replacement starts from active work")
            try coordinator.submit(root: "/project", query: "middle") { _ in middle += 1 }
            try coordinator.submit(root: "/project", query: "new") { _ in new += 1 }
            gate.finish.signal(); pump { new == 1 }
            check(old == 0 && middle == 0, "Only the newest request can deliver")
        }
        do {
            let gate = Gates()
            var successes = 0, failures = 0
            let coordinator = AtlasSearchCoordinator(progressiveWork: { _, _, _, publish in
                publish(report(1, running: true)); gate.finish.wait()
                throw AtlasSearchError.unavailable
            })
            try coordinator.submit(root: "/project", query: "failure") { result in
                switch result { case .success: successes += 1; case .failure: failures += 1 }
            }
            pump { successes == 1 }
            gate.finish.signal(); pump { failures == 1 }
            settle(); check(successes == 1 && failures == 1, "Failure after progress has exactly one terminal event")
        }
        do {
            let gate = Gates()
            var values = 0
            let coordinator = AtlasSearchCoordinator(progressiveWork: { _, _, cancellation, publish in
                publish(report(1, running: true)); gate.finish.wait()
                precondition(!cancellation.isCanceled)
                return report()
            })
            try coordinator.submit(root: "/project", query: "valid") { _ in values += 1 }
            pump { values == 1 }
            do {
                try coordinator.submit(root: "/project", query: "") { _ in preconditionFailure("Invalid query delivered") }
                preconditionFailure("Invalid input admitted")
            } catch AtlasSearchError.invalidRequest { check(true, "Invalid replacement rejected") }
            gate.finish.signal(); pump { values == 2 }
        }
        do {
            var failure = false
            let coordinator = AtlasSearchCoordinator(progressiveWork: { _, _, _, _ in report(running: true) })
            try coordinator.submit(root: "/project", query: "nonterminal") { result in
                if case .failure(.invalidResponse) = result { failure = true }
            }
            pump { failure }
        }
        do {
            let gate = Gates()
            var lease: Lease? = Lease()
            weak var weakLease = lease
            var finished = false
            let coordinator = AtlasSearchCoordinator(progressiveWork: { _, query, _, publish in
                if query == "lease" {
                    publish(report(running: true)); gate.offered.signal(); gate.finish.wait()
                }
                return report()
            })
            try coordinator.submit(root: "/project", query: "lease", accessLease: lease) { _ in }
            lease = nil
            check(gate.offered.wait(timeout: .now() + 5) == .success, "Lease test entered worker")
            coordinator.cancel()
            check(weakLease != nil, "Cancellation does not free an in-flight root grant")
            gate.finish.signal()
            try coordinator.submit(root: "/project", query: "after") { _ in finished = true }
            pump { finished }
            settle(); check(weakLease == nil, "Root grant retires after worker and delivery drain")
        }
        do {
            let gate = Gates()
            var calls = 0
            let coordinator = AtlasSearchCoordinator(progressiveWork: { _, _, _, publish in
                publish(report(running: true)); gate.finish.wait(); return report()
            })
            try coordinator.submit(root: "/project", query: "reentrant") { _ in
                calls += 1
                coordinator.cancel()
                gate.finish.signal()
            }
            pump { calls == 1 }
            settle(); check(calls == 1, "Progress callback may cancel without deadlock or late terminal callback")
        }
        do {
            let gate = Gates()
            var calls = 0
            var coordinator: AtlasSearchCoordinator? = AtlasSearchCoordinator(progressiveWork: { _, _, _, publish in
                publish(report(running: true)); gate.offered.signal(); gate.finish.wait(); return report()
            })
            weak var weakCoordinator = coordinator
            try coordinator?.submit(root: "/project", query: "destroy") { _ in calls += 1 }
            check(gate.offered.wait(timeout: .now() + 5) == .success, "Destruction test entered worker")
            coordinator = nil
            check(weakCoordinator == nil, "Mailbox must not retain the coordinator")
            gate.finish.signal(); settle()
            check(calls == 0, "Destroyed host receives no queued progress")
        }
        do {
            let context = AtlasSearchContext(root: "/project", query: "x", loadGeneration: UUID(),
                atlasRevision: UUID(), scope: "All files", customExtensions: "")
            let hit = SearchHit(id: 0, path: "a.rs", sourcePath: "a.rs", start: 0, end: 1,
                captureSHA256: String(repeating: "a", count: 64), captureByteLength: 2)
            let stream = UUID()
            let first = AtlasSearchPresentation(context: context, hits: [hit], verifiedHitIDs: [], id: stream)
            let later = AtlasSearchPresentation(context: context, hits: [hit], verifiedHitIDs: [0], id: stream)
            check(first.rowID(for: 0) == later.rowID(for: 0), "Append-only progress preserves selected row identity")
            check(later.captureCandidate(for: first.rowID(for: 0), in: context) != nil,
                  "An earlier stream row still resolves to its immutable witness")
            let replacement = AtlasSearchPresentation(context: context, hits: [hit], verifiedHitIDs: [])
            check(replacement.captureCandidate(for: first.rowID(for: 0), in: context) == nil,
                  "Same query text in a new stream does not inherit old row authority")
        }
        print("AtlasSearchProgressTests: \(checks) checks passed")
    }
}
