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
    let cancellation = AtlasSearchCancellation()
    let accessLease: AnyObject?
    let completion: @MainActor (Result<AtlasSearchReport, AtlasSearchError>) -> Void

    init(generation: UInt64, root: String, query: String,
         accessLease: AnyObject?, completion: @escaping @MainActor (Result<AtlasSearchReport, AtlasSearchError>) -> Void) {
        self.generation = generation
        self.root = root
        self.query = query
        self.accessLease = accessLease
        self.completion = completion
    }
}


/// Native-host scheduling of the existing synchronous Rust search call. This
/// is not another search engine or Rust executor. State is confined to the main
/// actor; the only cross-thread mutation is the locked cancellation flag.
///
/// At most ONE bridge call is in flight and ONE newest request is waiting. A
/// cancel invalidates delivery immediately but keeps the active slot and its
/// root-access lease until the foreign call actually returns. Rapid submissions
/// replace the pending descriptor instead of growing a DispatchQueue backlog.
@MainActor final class AtlasSearchCoordinator {
    typealias Work = @Sendable (String, String, AtlasSearchCancellation) throws -> AtlasSearchReport
    typealias Completion = @MainActor (Result<AtlasSearchReport, AtlasSearchError>) -> Void

    private let worker = DispatchQueue(label: "dev.frankencode.browser.search", qos: .userInitiated)
    private let work: Work
    private var active: AtlasSearchRequest?
    private var pending: AtlasSearchRequest?
    private var latest: UInt64?
    private var lastGeneration: UInt64 = 0

    init(work: @escaping Work) { self.work = work }

    deinit {
        // The worker retains its own Request, not this coordinator. Destruction
        // therefore invalidates delivery without freeing an in-flight grant.
        active?.cancellation.cancel()
        pending?.cancellation.cancel()
    }

    nonisolated static func validate(root: String, query: String) throws {
        guard !root.isEmpty, root.utf8.count <= 16_384, !root.utf8.contains(0),
              !query.isEmpty, query.utf8.count <= 1_024, !query.utf8.contains(0) else {
            throw AtlasSearchError.invalidRequest
        }
    }

    /// Validation/exhaustion fails before disturbing accepted or active work.
    @discardableResult
    func submit(root: String, query: String, accessLease: AnyObject? = nil,
                completion: @escaping Completion) throws -> UInt64 {
        precondition(Thread.isMainThread)
        try Self.validate(root: root, query: query)
        let (generation, exhausted) = lastGeneration.addingReportingOverflow(1)
        guard !exhausted else { throw AtlasSearchError.identityExhausted }
        lastGeneration = generation
        let request = AtlasSearchRequest(generation: generation, root: root, query: query,
                              accessLease: accessLease, completion: completion)
        latest = generation
        active?.cancellation.cancel()
        pending?.cancellation.cancel()
        if active == nil {
            start(request)
        } else {
            pending = request
        }
        return generation
    }

    /// Does not wait for a worker or claim that an in-flight syscall stopped.
    func cancel() {
        precondition(Thread.isMainThread)
        latest = nil
        active?.cancellation.cancel()
        pending?.cancellation.cancel()
        pending = nil
    }

    private func start(_ request: AtlasSearchRequest) {
        precondition(Thread.isMainThread && active == nil)
        active = request
        let work = self.work
        worker.async { [weak self, request] in
            let result: Result<AtlasSearchReport, AtlasSearchError>
            do {
                if request.cancellation.isCanceled { throw AtlasSearchError.canceled }
                let report = try withExtendedLifetime(request.accessLease) {
                    try work(request.root, request.query, request.cancellation)
                }
                if request.cancellation.isCanceled { throw AtlasSearchError.canceled }
                result = .success(report)
            } catch let error as AtlasSearchError {
                result = .failure(error)
            } catch {
                result = .failure(.unavailable)
            }
            DispatchQueue.main.async { [weak self, request] in
                self?.finish(request, result: result)
            }
        }
    }

    private func finish(_ request: AtlasSearchRequest, result: Result<AtlasSearchReport, AtlasSearchError>) {
        precondition(Thread.isMainThread)
        guard active === request else { return }
        active = nil
        if let next = pending {
            pending = nil
            start(next)
        } else if latest == request.generation {
            latest = nil
            // A host worker can also cancel itself. Deliver that terminal state,
            // never its source results, so the current UI cannot remain busy.
            // Explicit UI cancel/replace already invalidated latest and is silent.
            request.completion(request.cancellation.isCanceled ? .failure(.canceled) : result)
        }
    }
}
