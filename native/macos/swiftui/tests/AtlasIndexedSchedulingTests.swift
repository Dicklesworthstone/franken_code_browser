// The real shared coordinator must bound requests across live/indexed mode
// switches. These work descriptors intentionally perform no Rust/source work.
import Foundation
import Dispatch
private final class ScheduleLog: @unchecked Sendable {
    private let lock = NSLock()
    private var events: [Int] = []
    func append(_ n: Int) { lock.lock(); events.append(n); lock.unlock() }
    var values: [Int] { lock.lock(); defer { lock.unlock() }; return events }
}
private final class ScheduleGate: @unchecked Sendable {
    let entered = DispatchSemaphore(value: 0)
    let release = DispatchSemaphore(value: 0)
}
@main private enum AtlasIndexedSchedulingTests {
    @MainActor private static var checks = 0
    private static func report(_ count: UInt64, running: Bool = false) -> AtlasSearchReport {
        AtlasSearchReport(hits: [], complete: !running, truncated: false, unavailableFiles: 0,
            unsupportedFiles: 0, matchesSeen: count,
            progress: AtlasSearchProgress(isRunning: running, examinedFiles: 0, cataloguedFiles: 0))
    }
    @MainActor private static func check(_ condition: @autoclosure () -> Bool, _ label: String) {
        precondition(condition(), label); checks += 1
    }
    @MainActor private static func pump(_ done: () -> Bool) {
        let until = Date().addingTimeInterval(5)
        while !done() && Date() < until { _ = RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.005)) }
        check(done(), "Main delivery did not drain")
    }
    @MainActor static func main() throws {
        check(Thread.isMainThread, "Real coordinator thread preconditions")
        do {
            var value: UInt64?
            let c = AtlasSearchCoordinator(capturedWork: { _, _, _, _, _ in report(1) })
            try c.submit(root: "/repo", query: "default") { if case .success(let r) = $0 { value = r.matchesSeen } }
            pump { value != nil }; check(value == 1, "No override keeps the original live worker")
            value = nil
            try c.submit(root: "/repo", query: "indexed", using: { _, _, _, _, _ in report(2) }) {
                if case .success(let r) = $0 { value = r.matchesSeen }
            }
            pump { value != nil }; check(value == 2, "Explicit descriptor chooses the indexed worker")
            value = nil
            try c.submit(root: "/repo", query: "default-again") { if case .success(let r) = $0 { value = r.matchesSeen }
            }
            pump { value != nil }; check(value == 1, "An override does not mutate future default behavior")
        }
        do {
            let log = ScheduleLog(), gate = ScheduleGate()
            var early = 0, oldTerminal = 0, pendingDeliveries = 0
            var newest: UInt64?
            let c = AtlasSearchCoordinator(capturedWork: { _, _, _, _, _ in log.append(-1); return report(0) })
            try c.submit(root: "/repo", query: "first", using: { _, _, flag, _, publish in
                log.append(100)
                publish(report(0, running: true)); gate.entered.signal(); gate.release.wait()
                precondition(flag.isCanceled, "Replacement must cancel active mode")
                return report(100)
            }) { result in
                if case .success(let r) = result { if r.isInProgress { early += 1 } else { oldTerminal += 1 } }
            }
            pump { early == 1 }
            for n in 0..<40 {
                try c.submit(root: "/repo", query: "mode-\(n)", using: { _, _, _, _, _ in
                    log.append(n); return report(UInt64(n))
                }) { result in
                    if n == 39, case .success(let r) = result { newest = r.matchesSeen }
                    else { pendingDeliveries += 1 }
                }
            }
            check(log.values == [100], "Mode switching does not overlap source/index work")
            gate.release.signal(); pump { newest != nil }
            check(log.values == [100, 39], "Forty mode edits keep only one newest descriptor")
            check(oldTerminal == 0 && pendingDeliveries == 0 && newest == 39, "Stale or coalesced mode results cannot publish")
        }
        do {
            let gate = ScheduleGate(), log = ScheduleLog()
            var value: UInt64?
            let c = AtlasSearchCoordinator(capturedWork: { _, _, flag, _, _ in
                gate.entered.signal(); gate.release.wait()
                precondition(!flag.isCanceled, "Invalid request must not cancel valid work")
                log.append(1); return report(1)
            })
            try c.submit(root: "/repo", query: "valid") { if case .success(let r) = $0 { value = r.matchesSeen } }
            check(gate.entered.wait(timeout: .now() + 5) == .success, "Admitted work entered")
            do {
                try c.submit(root: "/repo", query: "", using: { _, _, _, _, _ in log.append(2); return report(2) }) { _ in }
                preconditionFailure("Invalid query admitted")
            } catch AtlasSearchError.invalidRequest { checks += 1 }
            gate.release.signal(); pump { value != nil }
            check(log.values == [1] && value == 1, "Invalid mode request neither executes nor destroys valid work")
        }
        print("AtlasIndexedSchedulingTests: \(checks) checks passed")
    }
}
