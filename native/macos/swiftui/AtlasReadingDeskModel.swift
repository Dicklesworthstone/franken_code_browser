import Foundation
import Observation
import Dispatch

/// Closing is reviewed against this exact accepted desk and these exact draft
/// edits. A nested AppKit event loop must not turn approval for older work into
/// permission to discard a newer note or a newly completed mutation.
struct AtlasDeskDraftRevision: Hashable, Sendable {
    let id: UUID
    let revision: UUID
}
struct AtlasDeskCloseSnapshot: Equatable, Sendable {
    let owner: UInt64?
    let revision: UInt64?
    let busy: Bool
    let unsavedContents: Bool
    let unverifiedChanges: Bool
    let drafts: Set<AtlasDeskDraftRevision>
    var requiresReview: Bool { busy || unsavedContents || unverifiedChanges || !drafts.isEmpty }
    static let empty = Self(owner: nil, revision: nil, busy: false, unsavedContents: false,
        unverifiedChanges: false, drafts: [])
}
private struct AtlasDeskCheckpointRevision: Equatable, Sendable {
    let owner: UInt64
    let revision: UInt64
    init(_ state: AtlasDeskState) { owner = state.owner; revision = state.revision }
}

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
    private(set) var exporting = false
    private(set) var closed = false
    private(set) var notice = "Keep exact source captures, add bookmarks and personal notes, and save your reading desk."
    private(set) var lastSave: AtlasDeskSaveReceipt?
    private(set) var lastExport: AtlasDeskSourcePackReceipt?
    private(set) var hasUnverifiedChanges = false
    private var acceptedRevision: AtlasDeskCheckpointRevision?
    private var checkpointRevision: AtlasDeskCheckpointRevision?
    private var acceptedContents = false
    var bookmarkLabel = ""
    var offsetInput = "0"
    var selectionEndInput = "0"
    @ObservationIgnored private let transport: AtlasReadingDeskTransport
    @ObservationIgnored private let io = AtlasDeskIO()
    @ObservationIgnored private var session: AtlasDeskSessionLease?
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var operationCancellation: AtlasDeskCancellation?
    @ObservationIgnored private var lastIssuedAttempt: UInt64 = 0
    @ObservationIgnored private var noteDrafts: [UUID: @MainActor () -> UUID?] = [:]

    init(transport: AtlasReadingDeskTransport) { self.transport = transport }
    var canOperate: Bool { !busy && !closed }
    var readerDomain: UUID? { transport.readerDomain }
    /// A failed attempt can advance lastAttempt without changing any personal
    /// work. Only the accepted model revision participates in dirty tracking.
    var hasUnsavedContents: Bool {
        guard !closed else { return false }
        if hasUnverifiedChanges { return true }
        guard let acceptedRevision else { return false }
        if let checkpointRevision { return acceptedRevision != checkpointRevision }
        return acceptedContents
    }
    /// Saving another copy of an unchanged or restored checkpoint is useful,
    /// even though that desk no longer needs a close warning.
    var hasCheckpointContents: Bool { state.map(Self.containsContents) ?? false }
    var canSaveCheckpoint: Bool { canOperate && state != nil && (hasCheckpointContents || hasUnsavedContents) }
    var hasOpenNoteEditor: Bool { !noteDrafts.isEmpty }
    var closeSnapshot: AtlasDeskCloseSnapshot {
        guard !closed else { return .empty }
        let drafts = Set(noteDrafts.compactMap { id, readRevision in
            readRevision().map { AtlasDeskDraftRevision(id: id, revision: $0) }
        })
        return AtlasDeskCloseSnapshot(owner: acceptedRevision?.owner, revision: acceptedRevision?.revision,
            busy: busy, unsavedContents: hasUnsavedContents, unverifiedChanges: hasUnverifiedChanges, drafts: drafts)
    }
    /// The editor supplies a cheap live revision, never a copy of its text.
    /// Registration ends when the editor is explicitly dismissed or its window
    /// has passed close review; hiding the app does not dismiss the editor.
    func registerNoteDraft(_ id: UUID, revision: @escaping @MainActor () -> UUID?) { noteDrafts[id] = revision }
    func unregisterNoteDraft(_ id: UUID) { noteDrafts.removeValue(forKey: id) }

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
        mutate(accessLease: accessLease, restoringCheckpoint: true,
            success: "Opened the checkpoint’s exact saved captures and personal state. Working-tree files were not reopened.") {
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


    /// Submit the confirmed source mapping once. A lost reply triggers only a
    /// state read, so it cannot move the note twice or consume another version.
    func reattachBookmark(_ target: AtlasDeskReattachment) {
        guard target.isCurrent(in: state) else {
            notice = AtlasReadingDeskError.staleReattachment.message; return
        }
        perform { model, _, cancellation in
            let lease = try await model.ensureStarted(cancellation)
            guard target.isCurrent(in: model.state) else { throw AtlasReadingDeskError.staleReattachment }
            let attempt = try model.nextAttempt(), request = target.request(attempt: attempt)
            let transport = model.transport
            let accepted: AtlasDeskState
            model.hasUnverifiedChanges = true
            do {
                let json = try await model.io.perform(keepingAlive: [lease], cancellation: cancellation) {
                    transport.run(lease.handle, request)
                }
                let receipt = try AtlasDeskState.decode(json, handle: lease.handle, acceptedAttempt: attempt)
                guard case .accepted = target.outcome(in: receipt, attempt: attempt) else {
                    throw AtlasReadingDeskError.invalidResponse
                }
                accepted = receipt
            } catch {
                do {
                    let json = try await model.io.perform(keepingAlive: [lease]) {
                        transport.run(lease.handle, .state)
                    }
                    accepted = try AtlasDeskState.decode(json, handle: lease.handle)
                } catch {
                    if !model.closed { model.invalidateState() }
                    model.notice = "The reattachment outcome could not be verified. Reload state to inspect the saved note before choosing another destination."
                    return
                }
            }
            guard !model.closed else { return }
            model.installState(accepted); model.page = nil
            let message: String
            switch target.outcome(in: accepted, attempt: attempt) {
            case .accepted:
                message = "Reattached the saved note to the selected range after comparing its original bytes. Its label and text are preserved. Save a new checkpoint to retain this change."
            case .notAccepted:
                message = "Reattachment was not accepted. The saved note is unchanged; select a nonempty range with identical bytes and review the destination again."
            case .unconfirmed:
                model.hasUnverifiedChanges = true
                message = "The reattachment outcome remains unconfirmed. Inspect the saved note and its source range before choosing another destination."
            }
            // A page failure cannot turn a verified mapping into a failed
            // mutation or erase its accepted state.
            do { try await model.loadPage(lease); model.notice = message }
            catch { model.page = nil; model.notice = message + " The source page is unavailable; reload state to retry reading it." }
        }
    }

    /// Save only against the editor's original anchor/version. An entered
    /// mutation may commit before cancellation or a lost reply, so reconciliation
    /// decides the outcome and the caller keeps an unresolved submission.
    func saveNote(target: AtlasDeskNoteTarget, label: String, body: String) async throws -> AtlasDeskNoteOutcome {
        guard canOperate else { throw AtlasReadingDeskError.unavailable }
        guard AtlasDeskNoteTarget.validText(label: label, body: body) else { throw AtlasReadingDeskError.invalidNote }
        guard target.isCurrent(in: state) else { throw AtlasReadingDeskError.staleNote }
        try Task.checkCancellation()
        let cancellation = AtlasDeskCancellation()
        operationCancellation = cancellation; busy = true
        defer {
            busy = false; operationCancellation = nil
            if closed { releaseSession() }
        }
        do {
            let lease = try await ensureStarted(cancellation)
            try Task.checkCancellation()
            guard let state, target.isCurrent(in: state) else { throw AtlasReadingDeskError.staleNote }
            let submission = try AtlasDeskNoteSubmission(target: target, state: state,
                attempt: nextAttempt(), label: label, body: body)
            let transport = transport
            hasUnverifiedChanges = true
            do {
                let json = try await withTaskCancellationHandler(operation: {
                    try await io.perform(keepingAlive: [lease], cancellation: cancellation) {
                        transport.run(lease.handle, submission.request)
                    }
                }, onCancel: { cancellation.cancel(); _ = transport.cancel(lease.handle) })
                let accepted = try AtlasDeskState.decode(json, handle: lease.handle, acceptedAttempt: submission.attempt)
                let outcome = submission.outcome(in: accepted)
                guard case .accepted = outcome else { throw AtlasReadingDeskError.invalidResponse }
                await installNoteState(accepted, outcome: outcome, lease: lease)
                return outcome
            } catch {
                // Never repeat annotate/edit to guess whether it succeeded.
                return await reconcileNote(submission, lease: lease)
            }
        } catch {
            notice = (error as? AtlasReadingDeskError)?.message ?? "The note was not submitted. Your draft is preserved."
            throw error
        }
    }

    /// Resolve the exact previously issued attempt. This operation performs no
    /// note mutation and cannot turn an ambiguous create into a duplicate note.
    func resolveNote(_ submission: AtlasDeskNoteSubmission) async -> AtlasDeskNoteOutcome {
        guard canOperate, let lease = session, lease.handle == submission.target.owner else {
            notice = "The original desk session is unavailable. The note outcome remains unconfirmed; your draft is preserved."
            return .unconfirmed(submission)
        }
        let cancellation = AtlasDeskCancellation()
        operationCancellation = cancellation; busy = true
        defer {
            busy = false; operationCancellation = nil
            if closed { releaseSession() }
        }
        let transport = transport
        return await withTaskCancellationHandler(operation: {
            await reconcileNote(submission, lease: lease, cancellation: cancellation)
        }, onCancel: { cancellation.cancel(); _ = transport.cancel(lease.handle) })
    }

    private func reconcileNote(_ submission: AtlasDeskNoteSubmission, lease: AtlasDeskSessionLease,
        cancellation: AtlasDeskCancellation? = nil) async -> AtlasDeskNoteOutcome {
        let transport = transport
        do {
            let json = try await io.perform(keepingAlive: [lease], cancellation: cancellation) {
                transport.run(lease.handle, .state)
            }
            let accepted = try AtlasDeskState.decode(json, handle: lease.handle)
            let outcome = submission.outcome(in: accepted)
            await installNoteState(accepted, outcome: outcome, lease: lease)
            return outcome
        } catch {
            if !closed { invalidateState() }
            notice = "The note outcome could not be verified. Your draft is preserved. Check saved state before submitting another copy."
            return .unconfirmed(submission)
        }
    }
    private func installNoteState(_ accepted: AtlasDeskState, outcome: AtlasDeskNoteOutcome,
        lease: AtlasDeskSessionLease) async {
        guard !closed else { return }
        installState(accepted); page = nil
        let message: String
        switch outcome {
        case .accepted:
            message = "Saved the note on its exact captured source range. Save a new checkpoint to retain its text after closing the app."
        case .notAccepted:
            message = "The note change was not accepted. Your draft is preserved; review its label, text and original source range before saving again."
        case .unconfirmed:
            hasUnverifiedChanges = true
            message = "The note outcome remains unconfirmed. Your draft is preserved; inspect the desk before discarding it or creating another copy."
        }
        // A page delivery failure must not erase an accepted personal note or
        // invite a duplicate retry. The state receipt already proves the write.
        do { try await loadPage(lease); notice = message }
        catch { page = nil; notice = message + " The source page is unavailable; reload state to retry reading it." }
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
                if receipt.succeeded { model.checkpointRevision = AtlasDeskCheckpointRevision(state) }
                model.notice = receipt.message
            } catch {
                model.notice = "The save outcome could not be verified. The selected destination may contain a complete or partial checkpoint. Inspect it before retrying and use a new filename."
                throw error
            }
        }
    }

    /// The destination grant and exact disclosure review survive until the
    /// worker's terminal receipt. Neither cancellation nor a missing reply can
    /// automatically retry an export that may already have written a file.
    func exportSourcePack(_ target: AtlasDeskSourcePackTarget, limits: AtlasDeskSourcePackLimits,
        path: String, accessLease: AnyObject?) {
        guard canOperate, Self.validPath(path), limits.isValid else { return }
        guard target.isCurrent(in: state) else { notice = AtlasReadingDeskError.staleExport.message; return }
        saving = true; exporting = true; lastExport = nil
        perform(accessLease: accessLease) { model, access, cancellation in
            let lease = try await model.ensureStarted(cancellation)
            guard target.isCurrent(in: model.state) else {
                model.notice = AtlasReadingDeskError.staleExport.message; return
            }
            let transport = model.transport
            let request = AtlasDeskRequest.exportSourcePack(revision: target.state.revision, path: path, limits: limits)
            let json = try await model.io.perform(keepingAlive: [lease] + access, cancellation: cancellation) {
                transport.run(lease.handle, request)
            }
            do {
                let receipt = try AtlasDeskSourcePackReceipt.decode(json, target: target, limits: limits)
                model.lastExport = receipt; model.notice = receipt.message
            } catch {
                model.notice = "The export outcome could not be verified. The destination may contain a complete or partial source pack. Inspect it before retrying and choose a new filename."
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
            model.hasUnverifiedChanges = true
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
                model.installState(accepted)
                model.page = nil
                try await model.loadPage(lease)
                model.notice = "Kept the exact captured page in the reading desk. Name a bookmark to keep this location in your saved desk."
            } catch {
                try? await model.reconcile(lease)
                throw error
            }
        }
    }

    /// Serialize export against desk mutations and hold its source owner until
    /// the destination reader's worker drains. The callback receives immutable
    /// state and an initializer, never a live path or an unleased source handle.
    func withRetainedPane<Value: Sendable>(_ paneID: UInt64, destinationDomain: UUID,
        prepare: @MainActor (AtlasDeskState, AtlasDeskPane,
            @escaping @Sendable (UInt64) throws -> String?) async throws -> Value) async throws -> Value {
        guard canOperate, readerDomain == destinationDomain else { throw AtlasReadingDeskError.unavailable }
        let cancellation = AtlasDeskCancellation()
        operationCancellation = cancellation
        busy = true
        defer {
            busy = false; operationCancellation = nil
            if closed { releaseSession() }
        }
        do {
            let lease = try await ensureStarted(cancellation)
            guard let original = state, let pane = original.panes.first(where: { $0.id == paneID }) else {
                throw AtlasReadingDeskError.unavailable
            }
            let transport = transport
            let result = try await withTaskCancellationHandler(operation: {
                try await prepare(original, pane) { destination in
                    try cancellation.check()
                    return withExtendedLifetime(lease) {
                        transport.run(lease.handle, .openReader(revision: original.revision,
                            pane: pane.id, destination: destination))
                    }
                }
            }, onCancel: { cancellation.cancel(); _ = transport.cancel(lease.handle) })
            try cancellation.check()
            try Task.checkCancellation()
            guard !closed, state?.owner == original.owner, state?.revision == original.revision,
                  state?.panes.first(where: { $0.id == paneID })?.location == pane.location else {
                throw AtlasReadingDeskError.canceled
            }
            notice = "Opened an independent reader from the desk’s exact capture. Its saved range remains unchanged until you keep a new page."
            return result
        } catch {
            notice = (error as? AtlasReadingDeskError)?.message ?? "The retained source reader could not be opened."
            throw error
        }
    }

    /// Update only the original retained pane. A checkpoint replacement or a
    /// closed pane cannot redirect an older reader's byte offsets into new data.
    @discardableResult func updateRetainedLocation(owner: UInt64, pane: UInt64, file: UInt64,
        sourceRevision: UInt64, start: UInt64, end: UInt64) -> Bool {
        guard canOperate, let state, state.owner == owner,
              let target = state.panes.first(where: { $0.id == pane }),
              target.location.file == file, target.location.revision == sourceRevision,
              start <= end, end <= 4 * 1024 * 1024 else {
            notice = "The original retained pane is no longer current. This independent reader remains available."
            return false
        }
        apply(kind: 2, subject: pane, start: start, end: end)
        return true
    }

    func cancel() {
        guard busy else { return }
        operationCancellation?.cancel()
        if let session { _ = transport.cancel(session.handle) }
        notice = exporting ? "Cancel requested. Waiting for the source pack’s actual write outcome."
            : saving ? "Cancel requested. Waiting for the checkpoint’s actual write outcome."
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

    private func mutate(accessLease: AnyObject? = nil, restoringCheckpoint: Bool = false, success: String? = nil,
        _ request: @escaping @MainActor (AtlasDeskState, UInt64) -> AtlasDeskRequest) {
        perform(accessLease: accessLease) { model, access, cancellation in
            let lease = try await model.ensureStarted(cancellation)
            guard let state = model.state else { throw AtlasReadingDeskError.unavailable }
            let attempt = try model.nextAttempt()
            let request = request(state, attempt), transport = model.transport
            model.hasUnverifiedChanges = true
            do {
                let json = try await model.io.perform(keepingAlive: [lease] + access, cancellation: cancellation) {
                    transport.run(lease.handle, request)
                }
                let accepted = try AtlasDeskState.decode(json, handle: lease.handle, acceptedAttempt: attempt)
                model.installState(accepted, fromCheckpoint: restoringCheckpoint)
                model.page = nil
                try await model.loadPage(lease)
                model.notice = success ?? "Reading desk updated. Save a new checkpoint to retain this state after closing the app."
            } catch {
                // State may have committed before a delivery/encoding/cancel
                // error. Never replay the operation to guess its outcome.
                try? await model.reconcile(lease)
                // An accepted revision equal to this restore's unique attempt
                // proves its outcome even when the initial reply was lost.
                if restoringCheckpoint, let accepted = model.state,
                   accepted.revision == attempt, accepted.lastAttempt == attempt {
                    model.checkpointRevision = AtlasDeskCheckpointRevision(accepted)
                }
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
                    notice = exporting ? "Export canceled before the source-pack request began. No destination was created."
                        : saving ? "Save canceled before the checkpoint write began. No destination was created."
                            : error.message
                } else if !saving { notice = (error as? AtlasReadingDeskError)?.message ?? "The reading desk operation could not finish. Reload its accepted state before continuing." }
                else if !notice.contains("outcome") { notice = "The save outcome is unavailable. Inspect the selected destination before retrying; a partial or complete file may exist." }
            }
            busy = false; saving = false; exporting = false; task = nil; operationCancellation = nil
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
        do { installState(try AtlasDeskState.decode(json, handle: lease.handle)) }
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
        let accepted: AtlasDeskState
        do {
            accepted = try AtlasDeskState.decode(json, handle: lease.handle)
        } catch {
            // An unverified revision must not authorize the next mutation.
            invalidateState()
            throw error
        }
        installState(accepted); page = nil
        // Page delivery is not the mutation receipt. Keep verified personal
        // state and its persistence baseline if only this read fails.
        try await loadPage(lease)
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
    private func installState(_ accepted: AtlasDeskState, fromCheckpoint: Bool = false) {
        let version = AtlasDeskCheckpointRevision(accepted)
        let contents = Self.containsContents(accepted)
        if fromCheckpoint || (acceptedRevision == nil && !contents) { checkpointRevision = version }
        state = accepted; acceptedRevision = version; acceptedContents = contents; hasUnverifiedChanges = false
    }
    private func invalidateState() {
        state = nil; page = nil; hasUnverifiedChanges = true
    }
    private static func containsContents(_ state: AtlasDeskState) -> Bool {
        state.retainedSources > 0 || !state.panes.isEmpty || !state.bookmarks.isEmpty || state.canBack || state.canForward
    }
    private func releaseSession() { page = nil; state = nil; session = nil; noteDrafts.removeAll() }
    private static func validPath(_ path: String) -> Bool {
        path.hasPrefix("/") && path.utf8.count <= 16_384 && !path.utf8.contains(0)
    }
    private static func offset(_ text: String) -> UInt64? {
        guard !text.isEmpty, text.utf8.count <= 20, text.utf8.allSatisfy({ (48...57).contains($0) }),
              let result = UInt64(text), result <= 4 * 1024 * 1024 else { return nil }
        return result
    }
}
