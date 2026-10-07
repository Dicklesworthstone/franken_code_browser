import Foundation
import Dispatch

/// A callback-safe flag. It owns no input, UI state, filesystem grant or runtime.
/// The NSLock protects the sole mutable field; polling never waits for search.
final class AtlasSearchCancellation: @unchecked Sendable {
    private let lock = NSLock()
    private var canceled = false

    var isCanceled: Bool {
        lock.lock()
        defer { lock.unlock() }
        return canceled
    }

    func cancel() {
        lock.lock()
        canceled = true
        lock.unlock()
    }
}

/// Immutable after admission. completion is invoked only on the main thread;
/// accessLease is retained, never operated on, by the worker. Its lifetime
/// covers the entire foreign call even if the view/coordinator is destroyed.
private final class AtlasSearchRequest: @unchecked Sendable {
    let generation: UInt64
    let root: String
    let query: String
    let notBefore: DispatchTime
    let cancellation = AtlasSearchCancellation()
    let accessLease: AnyObject?
    let completion: @MainActor (Result<AtlasSearchReport, AtlasSearchError>) -> Void

    init(generation: UInt64, root: String, query: String, notBefore: DispatchTime,
         accessLease: AnyObject?, completion: @escaping @MainActor (Result<AtlasSearchReport, AtlasSearchError>) -> Void) {
        self.generation = generation
        self.root = root
        self.query = query
        self.notBefore = notBefore
        self.accessLease = accessLease
        self.completion = completion
    }
}

/// Dispatch timer operations are thread-safe. Scheduling belongs to the main
/// actor, but releasing the last owner may cancel the source on any thread.
/// The wrapper makes that limited lifetime guarantee explicit to Swift 6.
private final class AtlasSearchDebounceTimer: @unchecked Sendable {
    private let source: DispatchSourceTimer

    init(handler: @escaping @Sendable () -> Void) {
        source = DispatchSource.makeTimerSource(queue: .main)
        source.setEventHandler(handler: handler)
        source.schedule(deadline: .distantFuture)
        source.resume()
    }

    @MainActor func schedule(deadline: DispatchTime) { source.schedule(deadline: deadline) }
    deinit { source.cancel() }
}

/// One latest immutable progress report and at most one queued main callback.
/// A slow main thread cannot accumulate a report-sized DispatchQueue backlog.
/// Closing on the worker retires pending progress before terminal delivery.
private final class AtlasSearchProgressMailbox: @unchecked Sendable {
    private let lock = NSLock()
    private var latest: AtlasSearchReport?
    private var scheduled = false
    private var closed = false
    private let deliver: @MainActor (AtlasSearchReport) -> Void

    init(deliver: @escaping @MainActor (AtlasSearchReport) -> Void) { self.deliver = deliver }

    func offer(_ report: AtlasSearchReport) {
        guard report.isInProgress else { return }
        lock.lock()
        guard !closed else { lock.unlock(); return }
        latest = report
        let enqueue = !scheduled
        scheduled = true
        lock.unlock()
        if enqueue { DispatchQueue.main.async { self.drain() } }
    }

    func close() {
        lock.lock()
        closed = true
        latest = nil
        lock.unlock()
    }

    @MainActor private func drain() {
        lock.lock()
        let report = closed ? nil : latest
        latest = nil
        scheduled = false
        lock.unlock()
        // Never call client code under the lock: delivery can cancel or replace.
        if let report { deliver(report) }
    }
}

/// Native-host scheduling of the existing synchronous Rust search call. This
/// is not another search engine or Rust executor. State is confined to the main
/// actor; cancellation and the single-slot progress mailbox are independently locked.
///
/// At most ONE bridge call is in flight and ONE newest request is waiting. A
/// cancel invalidates delivery immediately but keeps the active slot and its
/// root-access lease until the foreign call actually returns. Rapid submissions
/// replace the pending descriptor instead of growing a DispatchQueue backlog.
/// Typing uses one reusable timer, and never starts a scan before its quiet
/// period expires. Explicit submission bypasses that delay.
@MainActor final class AtlasSearchCoordinator {
    typealias Work = @Sendable (String, String, AtlasSearchCancellation) throws -> AtlasSearchReport
    typealias ProgressiveWork = @Sendable (String, String, AtlasSearchCancellation, @Sendable (AtlasSearchReport) -> Void) throws -> AtlasSearchReport
    typealias Completion = @MainActor (Result<AtlasSearchReport, AtlasSearchError>) -> Void

    private let worker = DispatchQueue(label: "dev.frankencode.browser.search", qos: .userInitiated)
    private let work: ProgressiveWork
    private var active: AtlasSearchRequest?
    private var pending: AtlasSearchRequest?
    private var latest: UInt64?
    private var lastGeneration: UInt64 = 0
    private var debounceTimer: AtlasSearchDebounceTimer?

    init(work: @escaping Work) {
        self.work = { root, query, cancellation, _ in try work(root, query, cancellation) }
    }
    init(progressiveWork: @escaping ProgressiveWork) { self.work = progressiveWork }

    deinit {
        // The worker retains its own Request, not this coordinator. Destruction
        // therefore invalidates delivery without freeing an in-flight grant.
        active?.cancellation.cancel()
        pending?.cancellation.cancel()
    }

    nonisolated static func validate(root: String, query: String) throws {
        _ = try AtlasSearchInput(root: root, query: query)
    }

    /// Validation/exhaustion fails before disturbing accepted or active work.
    @discardableResult
    func submit(root: String, query: String, accessLease: AnyObject? = nil,
                debounce: Bool = false, completion: @escaping Completion) throws -> UInt64 {
        try submit(input: AtlasSearchInput(root: root, query: query),
            accessLease: accessLease, debounce: debounce, completion: completion)
    }

    @discardableResult
    func submit(input: AtlasSearchInput, accessLease: AnyObject? = nil,
                debounce: Bool = false, completion: @escaping Completion) throws -> UInt64 {
        precondition(Thread.isMainThread)
        let (generation, exhausted) = lastGeneration.addingReportingOverflow(1)
        guard !exhausted else { throw AtlasSearchError.identityExhausted }
        lastGeneration = generation
        let deadline = debounce ? DispatchTime.now() + .milliseconds(150) : DispatchTime.now()
        let request = AtlasSearchRequest(generation: generation, root: input.root, query: input.query,
                              notBefore: deadline, accessLease: accessLease, completion: completion)
        latest = generation
        active?.cancellation.cancel()
        pending?.cancellation.cancel()
        pending = request
        startPendingWhenReady()
        return generation
    }

    /// Does not wait for a worker or claim that an in-flight syscall stopped.
    func cancel() {
        precondition(Thread.isMainThread)
        latest = nil
        active?.cancellation.cancel()
        pending?.cancellation.cancel()
        pending = nil
        debounceTimer?.schedule(deadline: .distantFuture)
    }

    private func startPendingWhenReady() {
        precondition(Thread.isMainThread)
        debounceTimer?.schedule(deadline: .distantFuture)
        guard active == nil, let request = pending else { return }
        if DispatchTime.now() < request.notBefore {
            if debounceTimer == nil {
                debounceTimer = AtlasSearchDebounceTimer { [weak self] in
                    MainActor.assumeIsolated { self?.startPendingWhenReady() }
                }
            }
            debounceTimer?.schedule(deadline: request.notBefore)
            return
        }
        pending = nil
        start(request)
    }

    private func start(_ request: AtlasSearchRequest) {
        precondition(Thread.isMainThread && active == nil)
        active = request
        let work = self.work
        let mailbox = AtlasSearchProgressMailbox { [weak self, weak request] report in
            guard let self, let request, self.active === request,
                  self.latest == request.generation, !request.cancellation.isCanceled else { return }
            request.completion(.success(report))
        }
        worker.async { [weak self, request] in
            let result: Result<AtlasSearchReport, AtlasSearchError>
            do {
                if request.cancellation.isCanceled { throw AtlasSearchError.canceled }
                let report = try withExtendedLifetime(request.accessLease) {
                    try work(request.root, request.query, request.cancellation, mailbox.offer)
                }
                if request.cancellation.isCanceled { throw AtlasSearchError.canceled }
                guard !report.isInProgress else { throw AtlasSearchError.invalidResponse }
                result = .success(report)
            } catch let error as AtlasSearchError {
                result = .failure(error)
            } catch {
                result = .failure(.unavailable)
            }
            mailbox.close()
            DispatchQueue.main.async { [weak self, request] in
                self?.finish(request, result: result)
            }
        }
    }

    private func finish(_ request: AtlasSearchRequest, result: Result<AtlasSearchReport, AtlasSearchError>) {
        precondition(Thread.isMainThread)
        guard active === request else { return }
        active = nil
        if pending != nil {
            startPendingWhenReady()
        } else if latest == request.generation {
            latest = nil
            // A host worker can also cancel itself. Deliver that terminal state,
            // never its source results, so the current UI cannot remain busy.
            // Explicit UI cancel/replace already invalidated latest and is silent.
            request.completion(request.cancellation.isCanceled ? .failure(.canceled) : result)
        }
    }
}
