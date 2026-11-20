import Foundation
import Dispatch

private final class Calls: @unchecked Sendable {
    private let lock = NSLock()
    private var next: UInt64 = 1
    private var values: [UInt64: Int] = [:]
    private var accepts = false
    private var pauses = 0
    var closeAccepted: Bool {
        get { lock.lock(); defer { lock.unlock() }; return accepts }
        set { lock.lock(); accepts = newValue; lock.unlock() }
    }
    var created: UInt64 { lock.lock(); defer { lock.unlock() }; return next - 1 }
    var pauseCount: Int { lock.lock(); defer { lock.unlock() }; return pauses }
    func create() -> UInt64 {
        lock.lock(); defer { lock.unlock() }; let result = next; next += 1; return result
    }
    func record(_ handle: UInt64) -> Int {
        lock.lock(); defer { lock.unlock() }; values[handle, default: 0] += 1; return values[handle]!
    }
    func count(_ handle: UInt64) -> Int { lock.lock(); defer { lock.unlock() }; return values[handle, default: 0] }
    func close(_ handle: UInt64) -> Bool { _ = record(handle); return closeAccepted }
    func pause() { lock.lock(); pauses += 1; lock.unlock() }
}

private final class LeaseBox: @unchecked Sendable {
    private let lock = NSLock()
    private var value: AtlasParcelMarkdownRetirement.Lease?
    func store(_ next: AtlasParcelMarkdownRetirement.Lease?) { lock.lock(); value = next; lock.unlock() }
    func take() -> AtlasParcelMarkdownRetirement.Lease? {
        lock.lock(); defer { lock.unlock() }; let next = value; value = nil; return next
    }
}

@main struct AtlasParcelMarkdownRetirementTests {
    static func main() {
        capacityPrecedesCreateAndForeignCallsHoldNoPoolLock()
        transientCloseRetriesAndSuccessfulFinishIsIdempotent()
        persistentFailureRetainsAuthorityUntilNextExplicitAcquire()
        fullQuarantineRefusesCreationAndBoundsEachRetryPass()
        zeroCreateReturnsItsReservationWithoutClosingZero()
        abandonedLeaseQuarantinesWithoutForeignWorkInDeinit()
        concurrentDrainRetainsCapacityAndNeverClosesOneHandleTwice()
        distinctTransportsRetireOnlyTheirOwnHandles()
        print("AtlasParcelMarkdownRetirement: 8 lifecycle scenarios passed")
    }

    private static func capacityPrecedesCreateAndForeignCallsHoldNoPoolLock() {
        let pool = AtlasParcelMarkdownRetirement(maximumHandles: 1, retryPause: {})
        let calls = Calls()
        let lease = pool.acquire(create: {
            precondition(pool.reservedHandles == 1)
            precondition(pool.acquire(create: { calls.create() }, close: { calls.close($0) }) == nil)
            return calls.create()
        }, close: { handle in
            precondition(pool.reservedHandles == 1)
            precondition(pool.acquire(create: { calls.create() }, close: { calls.close($0) }) == nil)
            _ = calls.record(handle); return true
        })!
        precondition(calls.created == 1)
        precondition(lease.finish())
        precondition(pool.reservedHandles == 0 && pool.quarantinedHandles == 0)
    }

    private static func transientCloseRetriesAndSuccessfulFinishIsIdempotent() {
        let calls = Calls()
        let pool = AtlasParcelMarkdownRetirement(retryPause: { calls.pause() })
        let lease = pool.acquire(create: { calls.create() }, close: { calls.record($0) == 2 })!
        precondition(lease.finish()); precondition(lease.finish())
        precondition(calls.count(lease.handle) == 2 && calls.pauseCount == 1)
        precondition(pool.reservedHandles == 0)
    }

    private static func persistentFailureRetainsAuthorityUntilNextExplicitAcquire() {
        let calls = Calls()
        let pool = AtlasParcelMarkdownRetirement(maximumHandles: 1, retryPause: { calls.pause() })
        let first = pool.acquire(create: { calls.create() }, close: { calls.close($0) })!
        precondition(!first.finish()); precondition(!first.finish())
        precondition(calls.count(first.handle) == 8 && calls.pauseCount == 7)
        precondition(pool.reservedHandles == 1 && pool.quarantinedHandles == 1)
        calls.closeAccepted = true
        let second = pool.acquire(create: { calls.create() }, close: { calls.close($0) })!
        precondition(second.handle != first.handle)
        precondition(calls.count(first.handle) == 9)
        precondition(pool.reservedHandles == 1 && pool.quarantinedHandles == 0)
        precondition(second.finish()); precondition(pool.reservedHandles == 0)
    }

    private static func fullQuarantineRefusesCreationAndBoundsEachRetryPass() {
        let calls = Calls()
        let pool = AtlasParcelMarkdownRetirement(maximumHandles: 2, retryPause: {})
        let first = pool.acquire(create: { calls.create() }, close: { calls.close($0) })!
        let second = pool.acquire(create: { calls.create() }, close: { calls.close($0) })!
        precondition(!first.finish() && !second.finish())
        precondition(pool.quarantinedHandles == 2)
        precondition(pool.acquire(create: { calls.create() }, close: { calls.close($0) }) == nil)
        precondition(calls.created == 2)
        precondition(calls.count(first.handle) == 16 && calls.count(second.handle) == 16)
        precondition(pool.reservedHandles == 2 && pool.quarantinedHandles == 2)
        calls.closeAccepted = true
        let last = pool.acquire(create: { calls.create() }, close: { calls.close($0) })!
        precondition(last.finish()); precondition(pool.reservedHandles == 0)
    }

    private static func zeroCreateReturnsItsReservationWithoutClosingZero() {
        let pool = AtlasParcelMarkdownRetirement(maximumHandles: 1, retryPause: {})
        let calls = Calls()
        precondition(pool.acquire(create: { 0 }, close: { calls.close($0) }) == nil)
        precondition(pool.reservedHandles == 0 && calls.count(0) == 0)
        calls.closeAccepted = true
        let lease = pool.acquire(create: { calls.create() }, close: { calls.close($0) })!
        precondition(lease.finish())
    }

    private static func abandonedLeaseQuarantinesWithoutForeignWorkInDeinit() {
        let pool = AtlasParcelMarkdownRetirement(maximumHandles: 1, retryPause: {})
        let calls = Calls(); calls.closeAccepted = true
        var first = pool.acquire(create: { calls.create() }, close: { calls.close($0) })
        let handle = first!.handle
        first = nil
        precondition(calls.count(handle) == 0)
        precondition(pool.reservedHandles == 1 && pool.quarantinedHandles == 1)
        let next = pool.acquire(create: { calls.create() }, close: { calls.close($0) })!
        precondition(calls.count(handle) == 1 && pool.quarantinedHandles == 0)
        precondition(next.finish())
    }

    private static func concurrentDrainRetainsCapacityAndNeverClosesOneHandleTwice() {
        let pool = AtlasParcelMarkdownRetirement(maximumHandles: 1, retryPause: {})
        let calls = Calls(), entered = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
        let done = DispatchSemaphore(value: 0), result = LeaseBox()
        let first = pool.acquire(create: { calls.create() }, close: { handle in
            _ = calls.record(handle)
            guard calls.closeAccepted else { return false }
            entered.signal()
            precondition(resume.wait(timeout: .now() + 5) == .success)
            return true
        })!
        precondition(!first.finish()); calls.closeAccepted = true
        DispatchQueue.global().async {
            result.store(pool.acquire(create: { calls.create() }, close: { calls.close($0) }))
            done.signal()
        }
        precondition(entered.wait(timeout: .now() + 5) == .success)
        precondition(pool.reservedHandles == 1)
        precondition(pool.acquire(create: { calls.create() }, close: { calls.close($0) }) == nil)
        precondition(calls.count(first.handle) == 9 && calls.created == 1)
        resume.signal()
        precondition(done.wait(timeout: .now() + 5) == .success)
        let next = result.take()!
        precondition(next.handle != first.handle && pool.quarantinedHandles == 0)
        precondition(next.finish()); precondition(pool.reservedHandles == 0)
    }

    private static func distinctTransportsRetireOnlyTheirOwnHandles() {
        let pool = AtlasParcelMarkdownRetirement(maximumHandles: 2, retryPause: {})
        let one = Calls(), two = Calls()
        let first = pool.acquire(create: { 17 }, close: { handle in
            precondition(handle == 17); return one.close(handle)
        })!
        let second = pool.acquire(create: { 29 }, close: { handle in
            precondition(handle == 29); return two.close(handle)
        })!
        precondition(!first.finish() && !second.finish())
        one.closeAccepted = true; two.closeAccepted = true
        let next = pool.acquire(create: { 33 }, close: { $0 == 33 })!
        precondition(one.count(17) == 9 && two.count(29) == 9)
        precondition(one.count(29) == 0 && two.count(17) == 0)
        precondition(next.finish()); precondition(pool.reservedHandles == 0)
    }
}
