import Foundation

/// Bounded ownership for temporary supplied-source readers. A failed close is
/// retained with its exact transport and capacity charge until a later explicit
/// acquire retries it. The native transport keeps this pool for its lifetime.
/// No timer, executor, unbounded queue or cancellation-dependent cleanup exists.
/// acquire/finish belong on the viewport worker; foreign calls never hold lock.
final class AtlasParcelMarkdownRetirement: @unchecked Sendable {
    fileprivate struct Record: Sendable {
        let slot: Int
        let handle: UInt64
        let close: @Sendable (UInt64) -> Bool
    }

    final class Lease: @unchecked Sendable {
        let handle: UInt64
        private let pool: AtlasParcelMarkdownRetirement
        private let lock = NSLock()
        private var record: Record?
        private var completed = false

        fileprivate init(pool: AtlasParcelMarkdownRetirement, record: Record) {
            self.pool = pool; self.record = record; handle = record.handle
        }

        /// A successful close is idempotent. A failed close transfers ownership
        /// to the pool and returns false; callers must not publish its packet.
        /// A concurrent finish returns false until the entered close completes.
        func finish() -> Bool {
            lock.lock()
            guard let record else { let result = completed; lock.unlock(); return result }
            self.record = nil
            lock.unlock()
            let closed = pool.finish(record)
            lock.lock(); completed = closed; lock.unlock()
            return closed
        }

        deinit {
            // Forgotten/aborted handoffs retain ownership without performing
            // potentially large Rust destruction on the releasing UI thread.
            if let record { pool.quarantine(record) }
        }
    }

    private let lock = NSLock()
    private let maximumHandles: Int
    private let retryPause: @Sendable () -> Void
    private var free: [Int]
    private var retained: [Record] = []
    private var draining = false

    init(maximumHandles: Int = 8,
         retryPause: @escaping @Sendable () -> Void = { Thread.sleep(forTimeInterval: 0.001) }) {
        precondition((1...8).contains(maximumHandles))
        self.maximumHandles = maximumHandles; self.retryPause = retryPause
        free = Array(0..<maximumHandles)
        retained.reserveCapacity(maximumHandles)
    }

    /// Includes live leases, entered create/close calls, and quarantined records.
    var reservedHandles: Int { lock.lock(); defer { lock.unlock() }; return maximumHandles - free.count }
    var quarantinedHandles: Int { lock.lock(); defer { lock.unlock() }; return retained.count }

    /// Drain the preceding finite quarantine once, then admit before creation.
    /// Simultaneous acquisitions never duplicate a drain or exceed eight handles.
    /// A zero create result returns its reservation without calling close(0).
    func acquire(create: @Sendable () -> UInt64,
                 close: @escaping @Sendable (UInt64) -> Bool) -> Lease? {
        drain()
        lock.lock()
        guard let slot = free.popLast() else { lock.unlock(); return nil }
        lock.unlock()
        let handle = create()
        guard handle != 0 else { release(slot); return nil }
        return Lease(pool: self, record: Record(slot: slot, handle: handle, close: close))
    }

    private func close(_ record: Record) -> Bool {
        for attempt in 0..<8 {
            if record.close(record.handle) { return true }
            if attempt < 7 { retryPause() }
        }
        return false
    }

    private func finish(_ record: Record) -> Bool {
        if close(record) { release(record.slot); return true }
        quarantine(record)
        return false
    }

    private func quarantine(_ record: Record) {
        lock.lock(); retained.append(record); lock.unlock()
    }

    private func release(_ slot: Int) {
        lock.lock(); free.append(slot); lock.unlock()
    }

    private func drain() {
        lock.lock()
        guard !draining, !retained.isEmpty else { lock.unlock(); return }
        draining = true
        let pending = retained
        retained = []
        lock.unlock()
        // Claims remain capacity charged while foreign calls run. New failed
        // leases can enter retained concurrently without joining this pass.
        var refused: [Record] = []
        refused.reserveCapacity(pending.count)
        for record in pending {
            if close(record) { release(record.slot) } else { refused.append(record) }
        }
        lock.lock()
        retained.append(contentsOf: refused)
        draining = false
        lock.unlock()
    }
}
