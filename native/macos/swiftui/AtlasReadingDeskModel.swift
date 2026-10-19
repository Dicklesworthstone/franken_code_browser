import Foundation
import Observation
import Dispatch

/// Holds a native handle until its final worker operation drains. Closing is
/// dispatched away from the interaction thread because retained sources can be
/// released there. Immutable closures and the handle are safe to share.
final class AtlasDeskSessionLease: Sendable {
    let handle: UInt64
    let transport: AtlasReadingDeskTransport
    private static let retirement = DispatchQueue(label: "dev.frankencode.browser.desk-retirement")
    init(transport: AtlasReadingDeskTransport) throws {
        let handle = transport.create()
        guard handle != 0 else { throw AtlasReadingDeskError.unavailable }
        self.handle = handle; self.transport = transport
    }
    deinit {
        let handle = handle, transport = transport
        Self.retirement.async {
            for attempt in 0..<8 {
                if transport.close(handle) { return }
                if attempt < 7 { Thread.sleep(forTimeInterval: 0.001) }
            }
            transport.retirementFailed()
        }
    }
}

/// Native grants are lifetime-only objects on the worker; no framework method
/// is invoked through this container. The main-actor model admits one operation
/// at a time, including reconciliation and page loading, so this queue has no
/// unbounded request backlog and no competing Rust executor.
private final class AtlasDeskWorkLease: @unchecked Sendable {
    let objects: [AnyObject]
    init(_ objects: [AnyObject]) { self.objects = objects }
}
/// A reusable native epoch can only cancel work that has already entered that
/// registry. This per-operation token also revokes queued work and the next
/// mutation after asynchronous startup. It never filters an entered result.
private final class AtlasDeskCancellation: @unchecked Sendable {
    private let lock = NSLock()
    private var canceled = false
    func cancel() { lock.lock(); canceled = true; lock.unlock() }
    func check() throws {
        lock.lock(); let stopped = canceled; lock.unlock()
        if stopped { throw AtlasReadingDeskError.canceled }
    }
}
@MainActor private final class AtlasDeskIO {
    private let queue = DispatchQueue(label: "dev.frankencode.browser.reading-desk", qos: .userInitiated)
    func perform<Value: Sendable>(keepingAlive: [AnyObject], cancellation: AtlasDeskCancellation? = nil,
        _ work: @escaping @Sendable () throws -> Value) async throws -> Value {
        guard keepingAlive.count <= 3 else { throw AtlasReadingDeskError.invalidInput }
        let lifetime = AtlasDeskWorkLease(keepingAlive)
        // Deliberately no post-work cancellation filter. A save may have written
        // a file, or a mutation may have committed, before cancellation arrived.
        return try await withCheckedThrowingContinuation { continuation in
            queue.async {
                let result = withExtendedLifetime(lifetime) { Result {
                    try cancellation?.check()
                    return try work()
                } }
                continuation.resume(with: result)
            }
        }
    }
}

@Observable @MainActor final class AtlasReadingDeskModel {
    private(set) var state: AtlasDeskState?
    private(set) var page: AtlasDeskPage?
    private(set) var busy = false
    private(set) var saving = false
    private(set) var closed = false
    private(set) var notice = "Keep exact source captures, name bookmarks, and save your reading desk."
    private(set) var lastSave: AtlasDeskSaveReceipt?
    var bookmarkLabel = ""
    var offsetInput = "0"
    var selectionEndInput = "0"
    @ObservationIgnored private let transport: AtlasReadingDeskTransport
    @ObservationIgnored private let io = AtlasDeskIO()
    @ObservationIgnored private var session: AtlasDeskSessionLease?
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var operationCancellation: AtlasDeskCancellation?
    @ObservationIgnored private var lastIssuedAttempt: UInt64 = 0

    init(transport: AtlasReadingDeskTransport) { self.transport = transport }
    var canOperate: Bool { !busy && !closed }
    var readerDomain: UUID? { transport.readerDomain }
    var hasUnsavedContents: Bool { (state?.retainedSources ?? 0) > 0 || !(state?.bookmarks.isEmpty ?? true) }

    func start() {
        guard state == nil, canOperate else { return }
        perform { model, _, cancellation in _ = try await model.ensureStarted(cancellation) }
    }
    func reload() {
        guard canOperate else { return }
        perform { model, _, cancellation in
            let lease = try await model.ensureStarted(cancellation)
            try await model.reconcile(lease)
        }
    }
    func capture(path: String, accessLease: AnyObject?) {
        guard Self.validPath(path) else { notice = AtlasReadingDeskError.invalidInput.message; return }
        mutate(accessLease: accessLease) { state, attempt in .capture(revision: state.revision, attempt: attempt, path: path) }
    }
    /// Explicit replacement imports a verified source-bearing checkpoint. The
    /// view confirms replacement of a nonempty desk before calling this method.
    func restore(path: String, accessLease: AnyObject?) {
        guard Self.validPath(path) else { notice = AtlasReadingDeskError.invalidInput.message; return }
        mutate(accessLease: accessLease, success: "Opened the checkpoint’s exact saved captures and personal state. Working-tree files were not reopened.") {
            state, attempt in .restore(revision: state.revision, attempt: attempt, path: path)
        }
    }
    func apply(kind: UInt64, subject: UInt64 = 0, start: UInt64 = 0, end: UInt64 = 0, label: String = "") {
        guard kind <= 11, label.utf8.count <= 256, !label.utf8.contains(0),
              kind != 3 || !label.isEmpty else { notice = AtlasReadingDeskError.invalidInput.message; return }
        mutate { state, attempt in .apply(revision: state.revision, attempt: attempt, kind: kind,
            subject: subject, start: start, end: end, label: label) }
    }
    func bookmark() {
        guard let pane = state?.active else { return }
        apply(kind: 3, subject: pane.id, label: bookmarkLabel)
    }
    func goToOffset(select: Bool = false) {
        guard let pane = state?.active, let offset = Self.offset(offsetInput),
              let end = select ? Self.offset(selectionEndInput) : offset,
              offset <= end else { notice = AtlasReadingDeskError.invalidInput.message; return }
        apply(kind: select ? 2 : 1, subject: pane.id, start: offset, end: end)
    }
    func nextPage() {
        guard let active = state?.active, let next = page?.nextOffset else { return }
        apply(kind: 1, subject: active.id, start: next)
    }
    func save(path: String, accessLease: AnyObject?) {
        guard canOperate, Self.validPath(path) else { return }
        saving = true
        lastSave = nil
        perform(accessLease: accessLease) { model, access, cancellation in
            let lease = try await model.ensureStarted(cancellation)
            guard let state = model.state else { throw AtlasReadingDeskError.unavailable }
            let request = AtlasDeskRequest.save(revision: state.revision, path: path, sourceBytes: state.retainedBytes)
            let transport = model.transport
            let json = try await model.io.perform(keepingAlive: [lease] + access, cancellation: cancellation) {
                transport.run(lease.handle, request)
            }
            do {
                let receipt = try AtlasDeskSaveReceipt.decode(json, state: state)
                model.lastSave = receipt
                model.notice = receipt.message
            } catch {
                model.notice = "The save outcome could not be verified. The selected destination may contain a complete or partial checkpoint. Inspect it before retrying and use a new filename."
                throw error
            }
        }
    }

    /// Existing reader integration supplies a validated handle and retains that
    /// reader's native grant throughout the callback. The desk owns its copied
    /// source after acceptance; subsequent operations never use the source path.
    func keepCapture(start: UInt64, end: UInt64, sourceDomain: UUID,
        using source: @escaping @MainActor (@escaping @Sendable (UInt64) throws -> String?) async throws -> String?) {
        guard let admittedDomain = readerDomain, admittedDomain == sourceDomain,
              start <= end, end <= 4 * 1024 * 1024 else { return }
        perform { model, _, cancellation in
            let lease = try await model.ensureStarted(cancellation)
            guard let state = model.state else { throw AtlasReadingDeskError.unavailable }
            let attempt = try model.nextAttempt()
            let transport = model.transport
            do {
                let json = try await source { reader in
                    try cancellation.check()
                    return withExtendedLifetime(lease) {
                        transport.run(lease.handle, .keep(revision: state.revision, attempt: attempt,
                            reader: reader, start: start, end: end))
                    }
                }
                let accepted = try AtlasDeskState.decode(json, handle: lease.handle, acceptedAttempt: attempt)
                guard let target = accepted.active?.location, target.offset == start,
                      target.selection?.start == start, target.selection?.end == end else {
                    throw AtlasReadingDeskError.invalidResponse
                }
                model.state = accepted
                model.page = nil
                try await model.loadPage(lease)
                model.notice = "Kept the exact captured page in the reading desk. Name a bookmark to keep this location in your saved desk."
            } catch {
                try? await model.reconcile(lease)
                throw error
            }
        }
    }

    func cancel() {
        guard busy else { return }
        operationCancellation?.cancel()
        if let session { _ = transport.cancel(session.handle) }
        notice = saving ? "Cancel requested. Waiting for the checkpoint’s actual write outcome."
            : "Cancel requested. Reconciling the desk’s accepted state."
    }
    /// Window teardown revokes admission immediately. The in-flight task keeps
    /// handles and grants until return; even a save still delivers its receipt.
    func close() {
        guard !closed else { return }
        closed = true
        if busy { cancel() }
        else { releaseSession() }
    }

    private func mutate(accessLease: AnyObject? = nil, success: String? = nil,
        _ request: @escaping @MainActor (AtlasDeskState, UInt64) -> AtlasDeskRequest) {
        perform(accessLease: accessLease) { model, access, cancellation in
            let lease = try await model.ensureStarted(cancellation)
            guard let state = model.state else { throw AtlasReadingDeskError.unavailable }
            let attempt = try model.nextAttempt()
            let request = request(state, attempt), transport = model.transport
            do {
                let json = try await model.io.perform(keepingAlive: [lease] + access, cancellation: cancellation) {
                    transport.run(lease.handle, request)
                }
                model.state = try AtlasDeskState.decode(json, handle: lease.handle, acceptedAttempt: attempt)
                model.page = nil
                try await model.loadPage(lease)
                model.notice = success ?? "Reading desk updated. Save a new checkpoint to retain this state after closing the app."
            } catch {
                // State may have committed before a delivery/encoding/cancel
                // error. Never replay the operation to guess its outcome.
                try? await model.reconcile(lease)
                throw error
            }
        }
    }
    private func perform(accessLease: AnyObject? = nil,
        _ work: @escaping @MainActor (AtlasReadingDeskModel, [AnyObject], AtlasDeskCancellation) async throws -> Void) {
        guard canOperate else { return }
        let cancellation = AtlasDeskCancellation()
        operationCancellation = cancellation
        busy = true
        task = Task { [self] in
            do { try await work(self, accessLease.map { [$0] } ?? [], cancellation) }
            catch {
                if let error = error as? AtlasReadingDeskError, case .canceled = error {
                    notice = saving ? "Save canceled before the checkpoint write began. No destination was created."
                        : error.message
                } else if !saving { notice = (error as? AtlasReadingDeskError)?.message ?? "The reading desk operation could not finish. Reload its accepted state before continuing." }
                else if !notice.contains("outcome") { notice = "The save outcome is unavailable. Inspect the selected destination before retrying; a partial or complete file may exist." }
            }
            busy = false; saving = false; task = nil; operationCancellation = nil
            if closed { releaseSession() }
        }
    }
    private func ensureStarted(_ cancellation: AtlasDeskCancellation) async throws -> AtlasDeskSessionLease {
        try cancellation.check()
        guard !closed else { throw AtlasReadingDeskError.unavailable }
        if let session, state != nil { return session }
        let lease: AtlasDeskSessionLease
        if let current = session { lease = current }
        else { lease = try AtlasDeskSessionLease(transport: transport); session = lease }
        let transport = transport
        let json = try await io.perform(keepingAlive: [lease], cancellation: cancellation) { transport.run(lease.handle, .start) }
        do { state = try AtlasDeskState.decode(json, handle: lease.handle) }
        catch {
            // Start may already have accepted before a response was lost.
            do { try await reconcile(lease) }
            catch { try cancellation.check(); throw error }
        }
        try cancellation.check()
        guard !closed else { throw AtlasReadingDeskError.unavailable }
        return lease
    }
    private func reconcile(_ lease: AtlasDeskSessionLease) async throws {
        guard !closed else { throw AtlasReadingDeskError.unavailable }
        let transport = transport
        let json = try await io.perform(keepingAlive: [lease]) { transport.run(lease.handle, .state) }
        do {
            state = try AtlasDeskState.decode(json, handle: lease.handle)
            page = nil
            try await loadPage(lease)
        } catch {
            // An unverified revision must not authorize the next mutation.
            state = nil; page = nil
            throw error
        }
    }
    private func loadPage(_ lease: AtlasDeskSessionLease) async throws {
        guard !closed else { throw AtlasReadingDeskError.unavailable }
        guard let state, let pane = state.active else { page = nil; return }
        let transport = transport, offset = pane.location.offset
        let request = AtlasDeskRequest.window(revision: state.revision, pane: pane.id, offset: offset)
        let json = try await io.perform(keepingAlive: [lease]) { transport.run(lease.handle, request) }
        page = try AtlasDeskPage.decode(json, state: state, pane: pane, offset: offset)
        offsetInput = String(offset)
        selectionEndInput = String(pane.location.selection?.end ?? page!.range.end)
    }
    private func nextAttempt() throws -> UInt64 {
        let current = max(lastIssuedAttempt, state?.lastAttempt ?? 0)
        guard current < UInt64.max else { throw AtlasReadingDeskError.unavailable }
        lastIssuedAttempt = current + 1
        return lastIssuedAttempt
    }
    private func releaseSession() { page = nil; state = nil; session = nil }
    private static func validPath(_ path: String) -> Bool {
        path.hasPrefix("/") && path.utf8.count <= 16_384 && !path.utf8.contains(0)
    }
    private static func offset(_ text: String) -> UInt64? {
        guard !text.isEmpty, text.utf8.count <= 20, text.utf8.allSatisfy({ (48...57).contains($0) }),
              let result = UInt64(text), result <= 4 * 1024 * 1024 else { return nil }
        return result
    }
}
