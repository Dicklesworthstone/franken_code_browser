import Foundation
import Dispatch

enum AtlasProjectIOError: Error, Sendable {
    case canceled, unavailable, invalidResponse, invalidRequest, concurrentRequest, limit
}

/// Only immutable input/value buffers cross this native-host queue. In
/// particular, callers must not capture AtlasDocument, NSView, font caches, or
/// mutable tile arrays in work. Lifetime leases are retained, never operated on.
private final class AtlasProjectJob: @unchecked Sendable {
    let cancellation: AtlasSearchCancellation
    let leases: [AnyObject]
    let work: @Sendable () -> (@MainActor @Sendable () -> Void)
    let abandon: @MainActor () -> Void
    init(cancellation: AtlasSearchCancellation, leases: [AnyObject],
         work: @escaping @Sendable () -> (@MainActor @Sendable () -> Void),
         abandon: @escaping @MainActor () -> Void) {
        self.cancellation = cancellation; self.leases = leases
        self.work = work; self.abandon = abandon
    }
}

/// One active blocking operation and one newest waiting project request.
/// This is native-host scheduling, not an additional Rust executor. Each load
/// awaits its operation before requesting another; a replacement project may
/// coalesce the waiting request but never overlap the active foreign call.
@MainActor final class AtlasProjectIO {
    private let queue = DispatchQueue(label: "dev.frankencode.browser.project-io", qos: .userInitiated)
    private var active: AtlasProjectJob?
    private var pending: AtlasProjectJob?
    var hasWaitingOperation: Bool { pending != nil }

    func perform<Value: Sendable>(cancellation: AtlasSearchCancellation,
        keepingAlive leases: [AnyObject] = [], work: @escaping @Sendable () throws -> Value) async throws -> Value {
        guard leases.count <= 4 else { throw AtlasProjectIOError.invalidRequest }
        return try await withTaskCancellationHandler(operation: {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Value, Error>) in
                if cancellation.isCanceled || Task.isCancelled {
                    continuation.resume(throwing: AtlasProjectIOError.canceled); return
                }
                // Same-load parallel submissions are a programming error, not
                // permission to cancel that load's own in-flight operation.
                if active?.cancellation === cancellation || pending?.cancellation === cancellation {
                    continuation.resume(throwing: AtlasProjectIOError.concurrentRequest); return
                }
                let job = AtlasProjectJob(cancellation: cancellation, leases: leases, work: {
                    let result: Result<Value, Error>
                    do {
                        if cancellation.isCanceled { throw AtlasProjectIOError.canceled }
                        let value = try work()
                        if cancellation.isCanceled { throw AtlasProjectIOError.canceled }
                        result = .success(value)
                    } catch { result = .failure(error) }
                    return {
                        if cancellation.isCanceled { continuation.resume(throwing: AtlasProjectIOError.canceled) }
                        else { continuation.resume(with: result) }
                    }
                }, abandon: { continuation.resume(throwing: AtlasProjectIOError.canceled) })
                if active == nil { start(job) }
                else {
                    active?.cancellation.cancel()
                    pending?.cancellation.cancel()
                    pending?.abandon()
                    pending = job
                }
            }
        }, onCancel: { cancellation.cancel() })
    }

    /// Never waits for I/O and never releases an active grant before return.
    func cancel() {
        active?.cancellation.cancel()
        pending?.cancellation.cancel()
        pending?.abandon()
        pending = nil
    }

    private func start(_ job: AtlasProjectJob) {
        precondition(active == nil)
        active = job
        // Retain this queue owner until the active operation and main-actor
        // continuation have drained. No unowned completion can lose a waiter.
        queue.async { [self, job] in
            let deliver = withExtendedLifetime(job.leases) { job.work() }
            DispatchQueue.main.async { [self, job] in
                precondition(active === job)
                active = nil
                if let next = pending { pending = nil; start(next) }
                deliver()
            }
        }
    }
}
