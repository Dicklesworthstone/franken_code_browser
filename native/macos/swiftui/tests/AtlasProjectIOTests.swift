import Foundation
import Dispatch

private final class Activity: @unchecked Sendable {
    private let lock = NSLock()
    private var names: [Int] = []
    private var active = 0, maximum = 0
    func enter(_ id: Int) {
        precondition(!Thread.isMainThread)
        lock.lock(); names.append(id); active += 1; maximum = max(maximum, active); lock.unlock()
    }
    func leave() { lock.lock(); active -= 1; lock.unlock() }
    var snapshot: ([Int], Int) { lock.lock(); defer { lock.unlock() }; return (names, maximum) }
}
private final class AccessLease {}

/// Actual serial queue, main actor and task-cancellation behavior. No mocked
/// executor, AppKit window, filesystem grant, or measured frame-time claim.
@MainActor @main struct AtlasProjectIOTests {
    static func until(_ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(5)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 1_000_000) }
        precondition(condition(), "bounded scheduling timeout")
    }
    nonisolated static func gate(_ semaphore: DispatchSemaphore) {
        precondition(semaphore.wait(timeout: .now() + 5) == .success)
    }
    static func expectCancellation(_ task: Task<Int, Error>) async {
        do { _ = try await task.value; preconditionFailure("canceled work succeeded") }
        catch AtlasProjectIOError.canceled { }
        catch { preconditionFailure("unexpected error: \(error)") }
    }
    static func main() async throws {
        try await serialWorkAndReturnValues()
        try await supersededWaitingWorkNeverRuns()
        try await canceledWorkKeepsItsGrantUntilReturn()
        try await taskCancellationReachesWorker()
        try await lateSuccessCannotEscapeCancellation()
        try await failureDoesNotPoisonTheQueue()
        try await concurrentSameLoadIsRefused()
        try await preCanceledRequestsDoNoWork()
        print("AtlasProjectIO: 8 bounded native-worker lifecycle scenarios passed")
    }
    static func serialWorkAndReturnValues() async throws {
        let io = AtlasProjectIO(), token = AtlasSearchCancellation(), activity = Activity()
        for id in 0..<8 {
            let result = try await io.perform(cancellation: token) {
                activity.enter(id); defer { activity.leave() }; return id * 2
            }
            precondition(result == id * 2)
        }
        precondition(activity.snapshot.0 == Array(0..<8) && activity.snapshot.1 == 1)
    }
    static func supersededWaitingWorkNeverRuns() async throws {
        let io = AtlasProjectIO(), activity = Activity(), release = DispatchSemaphore(value: 0)
        let first = AtlasSearchCancellation()
        let old = Task { try await io.perform(cancellation: first) {
            activity.enter(0); defer { activity.leave() }; gate(release); return 0
        } }
        try await until { activity.snapshot.0 == [0] }
        var waiting: [Task<Int, Error>] = []
        var previous: AtlasSearchCancellation?
        for id in 1...32 {
            let token = AtlasSearchCancellation()
            waiting.append(Task { try await io.perform(cancellation: token) {
                activity.enter(id); defer { activity.leave() }; return id
            } })
            if let previous { try await until { previous.isCanceled } }
            else { try await until { io.hasWaitingOperation } }
            previous = token
        }
        precondition(first.isCanceled)
        release.signal()
        await expectCancellation(old)
        for task in waiting.dropLast() { await expectCancellation(task) }
        let newest = try await waiting.last!.value
        precondition(newest == 32)
        precondition(activity.snapshot.0 == [0, 32] && activity.snapshot.1 == 1)
    }
    static func canceledWorkKeepsItsGrantUntilReturn() async throws {
        let io = AtlasProjectIO(), activity = Activity(), release = DispatchSemaphore(value: 0)
        let token = AtlasSearchCancellation()
        var access: AccessLease? = AccessLease(); weak var weakAccess = access
        let task = Task { [held = access!] in try await io.perform(cancellation: token, keepingAlive: [held]) {
            activity.enter(1); defer { activity.leave() }; gate(release); return 1
        } }
        try await until { !activity.snapshot.0.isEmpty }
        access = nil; io.cancel()
        precondition(weakAccess != nil && token.isCanceled)
        release.signal(); await expectCancellation(task)
        try await until { weakAccess == nil }
    }
    static func taskCancellationReachesWorker() async throws {
        let io = AtlasProjectIO(), activity = Activity(), token = AtlasSearchCancellation()
        let task = Task { try await io.perform(cancellation: token) {
            activity.enter(1); defer { activity.leave() }
            let deadline = Date().addingTimeInterval(5)
            while !token.isCanceled && Date() < deadline { Thread.sleep(forTimeInterval: 0.001) }
            precondition(token.isCanceled); return 1
        } }
        try await until { !activity.snapshot.0.isEmpty }
        task.cancel(); await expectCancellation(task)
    }
    static func lateSuccessCannotEscapeCancellation() async throws {
        let io = AtlasProjectIO(), token = AtlasSearchCancellation()
        let task = Task { try await io.perform(cancellation: token) { token.cancel(); return 1 } }
        await expectCancellation(task)
        let next = try await io.perform(cancellation: AtlasSearchCancellation()) { 7 }
        precondition(next == 7)
    }
    static func failureDoesNotPoisonTheQueue() async throws {
        let io = AtlasProjectIO(), token = AtlasSearchCancellation()
        do {
            let _: Int = try await io.perform(cancellation: token) { throw AtlasProjectIOError.unavailable }
            preconditionFailure("failure became success")
        } catch AtlasProjectIOError.unavailable { }
        let result = try await io.perform(cancellation: token) { 4 }
        precondition(result == 4)
    }
    static func concurrentSameLoadIsRefused() async throws {
        let io = AtlasProjectIO(), activity = Activity(), token = AtlasSearchCancellation()
        let release = DispatchSemaphore(value: 0)
        let first = Task { try await io.perform(cancellation: token) {
            activity.enter(1); defer { activity.leave() }; gate(release); return 1
        } }
        try await until { !activity.snapshot.0.isEmpty }
        do {
            let _: Int = try await io.perform(cancellation: token) { preconditionFailure("parallel same-load work ran") }
            preconditionFailure("parallel same-load work accepted")
        } catch AtlasProjectIOError.concurrentRequest { }
        precondition(!token.isCanceled)
        release.signal(); let result = try await first.value; precondition(result == 1)
    }
    static func preCanceledRequestsDoNoWork() async throws {
        let io = AtlasProjectIO(), token = AtlasSearchCancellation(); token.cancel()
        let task = Task { try await io.perform(cancellation: token) { preconditionFailure("canceled work ran"); return 1 } }
        await expectCancellation(task)
    }
}
