import Foundation

/// Injected transport exercises production decoding, mutation reconciliation,
/// bounded scheduling and native-grant lifetime. It is not a native ABI receipt.
private final class Fixture: @unchecked Sendable {
    private let lock = NSLock()
    private var revision: UInt64 = 0
    private var lastAttempt: UInt64 = 0
    private var captured = false
    private var position: UInt64 = 0
    private var selected: (UInt64, UInt64)?
    private var marks: [(UInt64, String, UInt64)] = []
    private var mutations = 0
    private var starts = 0
    private var saves = 0
    private var exports = 0
    private var closes = 0
    private var lostMutationResponse = false
    private var failedStateReads = 0
    private var failedPageReads = 0
    private var rejectedMutation = false
    private var canceledSave = false
    private var lostSaveResponse = false
    private var wrongSaveRevision = false
    private var acceptedRestore = false
    private struct Checkpoint {
        let captured: Bool
        let position: UInt64
        let selected: (UInt64, UInt64)?
        let marks: [(UInt64, String, UInt64)]
    }
    private var checkpoint: Checkpoint?
    private var canceledExport = false
    private var lostExportResponse = false
    private var openedGate: DispatchSemaphore?
    private var enteredGate: DispatchSemaphore?
    private enum Gate: Equatable { case start, capture, save, export }
    private var gatedRequest: Gate?
    private(set) var owner: UInt64 = 44
    let readerDomain = UUID()
    let text = "captured source\n"
    var transport: AtlasReadingDeskTransport {
        AtlasReadingDeskTransport(create: { [self] in owner }, run: { [self] in response($0, $1) },
            cancel: { _ in true }, close: { [self] _ in locked { closes += 1 }; return true },
            retirementFailed: { fatalError("retirement refused") }, readerDomain: readerDomain)
    }
    func loseNextMutationReply(stateFailures: Int = 0) { locked { lostMutationResponse = true; failedStateReads = stateFailures } }
    func rejectNextMutation() { locked { rejectedMutation = true } }
    func failNextPages(_ count: Int) { locked { failedPageReads = count } }
    func returnCanceledSave() { locked { canceledSave = true } }
    func returnSuccessfulSave() { locked { canceledSave = false } }
    func loseNextSaveReply() { locked { lostSaveResponse = true } }
    func returnWrongSaveRevision() { locked { wrongSaveRevision = true } }
    func acceptNextRestore() { locked { acceptedRestore = true } }
    func returnCanceledExport() { locked { canceledExport = true } }
    func loseNextExportReply() { locked { lostExportResponse = true } }
    func gateNextStart(entered: DispatchSemaphore, resume: DispatchSemaphore) { gate(.start, entered, resume) }
    func gateNextCapture(entered: DispatchSemaphore, resume: DispatchSemaphore) { gate(.capture, entered, resume) }
    func gateNextSave(entered: DispatchSemaphore, resume: DispatchSemaphore) { gate(.save, entered, resume) }
    func gateNextExport(entered: DispatchSemaphore, resume: DispatchSemaphore) { gate(.export, entered, resume) }
    private func gate(_ request: Gate, _ entered: DispatchSemaphore, _ resume: DispatchSemaphore) {
        locked { gatedRequest = request; enteredGate = entered; openedGate = resume }
    }
    var mutationCount: Int { locked { mutations } }
    var startCount: Int { locked { starts } }
    var saveCount: Int { locked { saves } }
    var exportCount: Int { locked { exports } }
    var closeCount: Int { locked { closes } }
    private func locked<T>(_ body: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return body() }
    private func response(_ handle: UInt64, _ request: AtlasDeskRequest) -> String? {
        let kind: Gate?
        switch request {
        case .start: kind = .start
        case .capture: kind = .capture
        case .save: kind = .save
        case .exportSourcePack: kind = .export
        default: kind = nil
        }
        let gates = locked { () -> (DispatchSemaphore?, DispatchSemaphore?) in
            guard let kind, kind == gatedRequest else { return (nil, nil) }
            let result = (enteredGate, openedGate)
            gatedRequest = nil; enteredGate = nil; openedGate = nil
            return result
        }
        gates.0?.signal(); gates.1?.wait()
        return locked {
            precondition(handle == owner)
            switch request {
            case .start: starts += 1; return state()
            case .state:
                if failedStateReads > 0 { failedStateReads -= 1; return nil }
                return state()
            case .capture(let expected, let attempt, _):
                guard expected == revision, attempt > lastAttempt else { return error("DESK_STALE_ATTEMPT") }
                revision = attempt; lastAttempt = attempt; captured = true; position = 0; selected = nil; mutations += 1
                if lostMutationResponse { lostMutationResponse = false; return nil }
                return state()
            case .keep(let expected, let attempt, _, let start, let end):
                guard expected == revision, attempt > lastAttempt else { return error("DESK_STALE_ATTEMPT") }
                revision = attempt; lastAttempt = attempt; captured = true; position = start; selected = (start, end); mutations += 1
                if lostMutationResponse { lostMutationResponse = false; return nil }
                return state()
            case .apply(let expected, let attempt, let kind, _, let start, _, let label):
                guard expected == revision, attempt > lastAttempt else { return error("DESK_STALE_ATTEMPT") }
                lastAttempt = attempt
                if rejectedMutation { rejectedMutation = false; return error("DESK_CANCELED") }
                revision = attempt; mutations += 1
                if kind == 3 { marks.append((UInt64(marks.count + 1), label, position)) }
                if kind == 1 { position = start }
                if lostMutationResponse { lostMutationResponse = false; return nil }
                return state()
            case .restore(let expected, let attempt, _):
                guard expected == revision, attempt > lastAttempt else { return error("DESK_STALE_ATTEMPT") }
                lastAttempt = attempt
                guard acceptedRestore, let checkpoint else { return error("DESK_CHECKPOINT_INTEGRITY") }
                acceptedRestore = false; revision = attempt; mutations += 1
                captured = checkpoint.captured; position = checkpoint.position
                selected = checkpoint.selected; marks = checkpoint.marks
                if lostMutationResponse { lostMutationResponse = false; return nil }
                return state()
            case .openReader:
                return error("DESK_READER_TRANSFER_NOT_IN_THIS_FIXTURE")
            case .annotate, .editBookmark, .suggestReattachment, .reattachRange:
                return error("DESK_NOTES_USE_DEDICATED_FIXTURE")
            case .window(let expected, let pane, let offset):
                guard expected == revision, pane == 1, offset <= UInt64(text.utf8.count) else { return error("DESK_INVALID_LOCATION") }
                if failedPageReads > 0 { failedPageReads -= 1; return nil }
                var w = header("window")
                w.merge(location(position)) { _, new in new }
                w["pane"] = "1"; w["original_range"] = ["start": String(offset), "end": String(text.utf8.count)]
                w["text"] = String(decoding: text.utf8.dropFirst(Int(offset)), as: UTF8.self)
                w["text_kind"] = "logical-captured-text-not-shaped"; w["line_semantics"] = "editor-rows"
                w["has_replacements"] = false; w["reaches_eof"] = true; w["next_offset"] = NSNull()
                w["rows"] = [["line": "1"]]
                return encode(w)
            case .save(let expected, _, let bytes):
                precondition(expected == revision && bytes == (captured ? UInt64(text.utf8.count) : 0))
                saves += 1
                if !canceledSave {
                    checkpoint = Checkpoint(captured: captured, position: position, selected: selected, marks: marks)
                }
                if lostSaveResponse { lostSaveResponse = false; return nil }
                var w = header("save")
                if wrongSaveRevision { wrongSaveRevision = false; w["model_revision"] = String(revision + 1) }
                w["status"] = canceledSave ? "error" : "ok"
                w["effect"] = canceledSave ? "destination-created-incomplete" : "complete-file-and-parent-sync-requested"
                w["bytes_written"] = canceledSave ? "8" : "256"; w["checkpoint_bytes"] = "256"
                w["source_bytes"] = String(bytes); w["checkpoint_digest"] = String(repeating: "a", count: 64)
                w["contains_source_payloads"] = true; w["contains_personal_labels"] = true
                w["encrypted"] = false; w["overwritten"] = false
                w["error"] = canceledSave ? ["code": "DESK_CHECKPOINT_CANCELED"] : NSNull()
                return encode(w)
            case .exportSourcePack(let expected, _, let limits):
                precondition(expected == revision)
                exports += 1
                if lostExportResponse { lostExportResponse = false; return nil }
                let exported = selected == nil ? 0 : marks.count
                let selectedBytes = selected.map { ($0.1 - $0.0) * UInt64(exported) } ?? 0
                var w = header("export")
                w["pack_schema"] = "fcb.desk-source-pack/1"
                w["status"] = canceledExport ? "error" : "ok"
                w["effect"] = canceledExport ? "destination-created-incomplete" : "complete-file-and-parent-sync-requested"
                w["bytes_written"] = canceledExport ? "8" : "256"
                w["export_bytes"] = "256"; w["export_scalars"] = "256"
                w["export_digest"] = String(repeating: "b", count: 64)
                w["budgets"] = ["source_bytes": String(limits.sourceBytes), "output_bytes": String(limits.outputBytes), "output_scalars": String(limits.outputScalars)]
                w["coverage"] = ["total_bookmarks": String(marks.count), "exported_selections": String(exported),
                    "omitted_no_selection": String(marks.count - exported), "omitted_source_limit": "0", "omitted_output_limit": "0", "selected_original_bytes": String(selectedBytes)]
                w["complete"] = exported == marks.count
                w["contains_source_payloads"] = selectedBytes > 0
                w["contains_personal_labels"] = true; w["contains_user_text"] = true
                w["encrypted"] = false; w["redacted"] = false; w["overwritten"] = false
                w["live_roots_accessed"] = false; w["power_loss_qualified"] = false
                w["error"] = canceledExport ? ["code": "DESK_CHECKPOINT_CANCELED"] : NSNull()
                return encode(w)
            }
        }
    }
    private func header(_ command: String) -> [String: Any] {
        ["schema": "fcb.reading-desk/1", "status": "ok", "command": command, "owner": String(owner),
            "model_revision": String(revision), "last_attempt": String(lastAttempt),
            "additional_source_bytes_read": "0", "native_presented": false]
    }
    private func location(_ offset: UInt64) -> [String: Any] {
        ["file_id": "1", "source_revision": "1", "offset": String(offset),
            "selection": selected.map { ["start": String($0.0), "end": String($0.1)] } as Any? ?? NSNull()]
    }
    private func state() -> String {
        var w = header("state")
        w["persistence"] = "explicit-checkpoint"; w["autosave"] = false
        w["retained_sources"] = captured ? "1" : "0"
        w["retained_source_bytes"] = captured ? String(text.utf8.count) : "0"
        w["active_pane"] = captured ? "1" : NSNull()
        w["can_go_back"] = false; w["can_go_forward"] = false
        var pane = location(position); pane["pane"] = "1"; pane["label"] = "retained.rs"; pane["pinned"] = false
        w["panes"] = captured ? [pane] : []
        w["bookmarks"] = marks.map { mark in
            var bookmark = location(mark.2); bookmark["bookmark"] = String(mark.0); bookmark["label"] = mark.1
            bookmark["body"] = ""; bookmark["update_sequence"] = "1"
            return bookmark
        }
        return encode(w)
    }
    private func error(_ code: String) -> String {
        encode(["schema": "fcb.reading-desk/1", "status": "error", "owner": String(owner), "error": ["code": code]])
    }
    private func encode(_ value: [String: Any]) -> String { String(decoding: try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self) }
}

private final class Flag: @unchecked Sendable {
    private let lock = NSLock(); private var count = 0
    func advance() { lock.lock(); count += 1; lock.unlock() }
    var value: Int { lock.lock(); defer { lock.unlock() }; return count }
}
private final class Grant {
    let flag: Flag
    init(_ flag: Flag) { self.flag = flag }
    deinit { flag.advance() }
}

/// This fake owns metadata only. Filesystem privacy, quota and publication are
/// covered by AtlasDeskRecoveryTests; these checks exercise the real model's
/// worker admission and interpretation of typed bridge save/restore receipts.
private final class RecoveryFixture: @unchecked Sendable {
    private let lock = NSLock()
    private var copies: [AtlasDeskRecoveryEntry] = []
    private var listed = 0
    private var attempted = 0
    private var read = 0
    private var removed = 0
    private var failedRemovalAfterEffect = false
    private var failedReadAfterEffect = false
    private var acceptedRevisions: [UInt64] = []
    var listCount: Int { locked { listed } }
    var attemptCount: Int { locked { attempted } }
    var readCount: Int { locked { read } }
    var removeCount: Int { locked { removed } }
    var revisions: [UInt64] { locked { acceptedRevisions } }
    func failNextRemovalAfterEffect() { locked { failedRemovalAfterEffect = true } }
    func failNextReadAfterEffect() { locked { failedReadAfterEffect = true } }
    func removeExternally(_ entry: AtlasDeskRecoveryEntry) { locked { copies.removeAll { $0.id == entry.id } } }
    func addInterruptedCopy(checkpointBytes: UInt64 = 256) -> AtlasDeskRecoveryEntry {
        let entry = AtlasDeskRecoveryEntry(id: UUID(), session: UUID(),
            url: URL(fileURLWithPath: "/private/reading-desk-recovery/\(UUID().uuidString).fcbdesk"),
            savedAt: Date(), sourceBytes: 0, checkpointBytes: checkpointBytes, confirmed: false)
        locked { copies.insert(entry, at: 0) }
        return entry
    }
    private func locked<T>(_ body: () throws -> T) rethrows -> T {
        lock.lock(); defer { lock.unlock() }; return try body()
    }
    var service: AtlasDeskRecoveryService {
        AtlasDeskRecoveryService(list: { [self] in
            precondition(!Thread.isMainThread)
            return locked { listed += 1; return copies }
        }, save: { [self] session, state, write in
            precondition(!Thread.isMainThread)
            locked { attempted += 1 }
            let id = UUID(), url = URL(fileURLWithPath: "/private/reading-desk-recovery/\(UUID().uuidString).fcbdesk")
            let receipt = try write(url)
            precondition(receipt.succeeded)
            let entry = AtlasDeskRecoveryEntry(id: id, session: session, url: url, savedAt: Date(),
                sourceBytes: state.retainedBytes, checkpointBytes: receipt.checkpointBytes)
            locked {
                copies.insert(entry, at: 0); acceptedRevisions.append(state.revision)
                copies = Array(copies.prefix(2))
            }
            return entry
        }, read: { [self] entry, reader in
            precondition(!Thread.isMainThread)
            return try locked {
                guard copies.contains(entry) else { throw AtlasDeskRecoveryError.changed }
                read += 1
                let result = try reader(entry.url)
                if failedReadAfterEffect {
                    failedReadAfterEffect = false
                    throw AtlasDeskRecoveryError.unsafeLocation
                }
                return result
            }
        }, remove: { [self] entry in
            precondition(!Thread.isMainThread)
            try locked {
                guard copies.contains(entry) else { throw AtlasDeskRecoveryError.changed }
                copies.removeAll { $0.id == entry.id }; removed += 1
                if failedRemovalAfterEffect {
                    failedRemovalAfterEffect = false
                    throw AtlasDeskRecoveryError.filesystem
                }
            }
        })
    }
}

@main struct AtlasReadingDeskTests {
    @MainActor static func entered(_ gate: DispatchSemaphore) async {
        for _ in 0..<1000 {
            if gate.wait(timeout: .now()) == .success { return }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        fatalError("desk fixture did not enter the expected request")
    }
    @MainActor static func idle(_ model: AtlasReadingDeskModel) async {
        for _ in 0..<1000 {
            if !model.busy { return }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        fatalError("desk operation did not drain")
    }
    @MainActor static func main() async throws {
        let fixture = Fixture(), model = AtlasReadingDeskModel(transport: Fixture().transport)
        // Independent fixture explicitly retained below; the first model checks
        // empty construction and clean retirement without auto-started I/O.
        precondition(model.state == nil && !model.busy)
        model.close()
        await cancellationAdmission()
        await exportLifecycle()
        await checkpointTracking()
        await recoveryLifecycle()
        await recoveryCancellationAndClose()
        await interruptedRecovery()
        await restoredStorageRevalidation()
        let desk = AtlasReadingDeskModel(transport: fixture.transport)
        desk.capture(path: "/chosen/source.rs", accessLease: nil); await idle(desk)
        precondition(desk.state?.retainedSources == 1 && desk.page?.text == fixture.text)
        desk.bookmarkLabel = "Reason to keep the evidence"
        fixture.loseNextMutationReply()
        desk.bookmark(); await idle(desk)
        precondition(desk.state?.bookmarks.count == 1 && desk.state?.revision == 2)
        precondition(fixture.mutationCount == 2, "a lost reply must reconcile, not replay the bookmark")
        desk.bookmarkLabel = "second explicit note"; desk.bookmark(); await idle(desk)
        precondition(desk.state?.bookmarks.count == 2 && fixture.mutationCount == 3)
        let accepted = desk.state?.revision
        desk.restore(path: "/chosen/corrupt.fcbdesk", accessLease: nil); await idle(desk)
        precondition(desk.state?.revision == accepted && desk.state?.bookmarks.count == 2)
        precondition(desk.page?.text == fixture.text)
        fixture.returnCanceledSave()
        let saveEntered = DispatchSemaphore(value: 0), saveResume = DispatchSemaphore(value: 0)
        fixture.gateNextSave(entered: saveEntered, resume: saveResume)
        desk.save(path: "/chosen/new.fcbdesk", accessLease: nil)
        await Self.entered(saveEntered)
        desk.cancel(); saveResume.signal(); await idle(desk)
        precondition(desk.lastSave?.effect == "destination-created-incomplete")
        precondition(desk.lastSave?.written == 8 && desk.lastSave?.succeeded == false)
        precondition(desk.notice.contains("8 bytes written"), "canceled writes retain their effect receipt")
        var foreignTransferred = false
        desk.keepCapture(start: 0, end: 4, sourceDomain: UUID()) { _ in foreignTransferred = true; return nil }
        precondition(!desk.busy && !foreignTransferred && fixture.mutationCount == 3,
            "a foreign numeric reader handle must be refused before crossing the native boundary")
        desk.keepCapture(start: 0, end: 4, sourceDomain: fixture.readerDomain) { work in try work(900) }
        await idle(desk)
        precondition(desk.state?.retainedSources == 1 && fixture.mutationCount == 4)
        desk.close()

        // Close while foreign work is blocked: the grant must outlive both the
        // model's admission and caller's local reference until actual return.
        let gated = Fixture(), draining = AtlasReadingDeskModel(transport: Fixture().transport)
        draining.close()
        let inFlight = AtlasReadingDeskModel(transport: gated.transport)
        let entered = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
        gated.gateNextCapture(entered: entered, resume: resume)
        let flag = Flag()
        var grant: Grant? = Grant(flag)
        inFlight.capture(path: "/chosen/retained.rs", accessLease: grant)
        grant = nil
        for _ in 0..<1000 {
            if entered.wait(timeout: .now()) == .success { break }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        precondition(inFlight.busy && flag.value == 0)
        inFlight.close()
        precondition(flag.value == 0 && gated.closeCount == 0)
        resume.signal(); await idle(inFlight)
        precondition(inFlight.closed && inFlight.state == nil)
        for _ in 0..<1000 {
            if flag.value == 1 && gated.closeCount == 1 { break }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        precondition(flag.value == 1 && gated.closeCount == 1)

        try wireNegativeControls()
        print("Reading desk wire, exact-source model, recovery opt-in and receipts, lost replies, save/export receipts and grant drain tests passed")
    }

    @MainActor static func cancellationAdmission() async {
        for saving in [false, true] {
            let fixture = Fixture()
            let subject = AtlasReadingDeskModel(transport: fixture.transport)
            // Cancel before the scheduled task starts: no registry request is
            // allowed to snapshot a newer reusable native cancellation epoch.
            if saving { subject.save(path: "/chosen/queued.fcbdesk", accessLease: nil) }
            else { subject.capture(path: "/chosen/queued.rs", accessLease: nil) }
            subject.cancel(); await idle(subject)
            precondition(fixture.startCount == 0 && fixture.mutationCount == 0 && fixture.saveCount == 0)

            // Startup is already entered and deliberately accepts despite the
            // cancel request. Its accepted state must not authorize a later
            // capture/save from the canceled operation.
            let started = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
            fixture.gateNextStart(entered: started, resume: resume)
            if saving { subject.save(path: "/chosen/after-start.fcbdesk", accessLease: nil) }
            else { subject.capture(path: "/chosen/after-start.rs", accessLease: nil) }
            await Self.entered(started)
            subject.cancel(); resume.signal(); await idle(subject)
            precondition(fixture.startCount == 1 && fixture.mutationCount == 0 && fixture.saveCount == 0)
            precondition(subject.state?.revision == 0 && subject.lastSave == nil)
            if saving { precondition(subject.notice.contains("No destination was created")) }
            // The cancellation belongs to that operation, not to the reused
            // session: a new explicit capture still works.
            subject.capture(path: "/chosen/next-explicit.rs", accessLease: nil); await idle(subject)
            precondition(fixture.mutationCount == 1 && subject.state?.retainedSources == 1)
            subject.close()
        }

        // Cancellation cannot roll back an entered mutation. A lost reply must
        // still recover its accepted state without replaying the capture.
        let fixture = Fixture()
        let subject = AtlasReadingDeskModel(transport: fixture.transport)
        let started = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
        fixture.loseNextMutationReply(); fixture.gateNextCapture(entered: started, resume: resume)
        subject.capture(path: "/chosen/entered.rs", accessLease: nil)
        await Self.entered(started)
        subject.cancel(); resume.signal(); await idle(subject)
        precondition(fixture.mutationCount == 1 && subject.state?.retainedSources == 1 && subject.page?.text == fixture.text)
        subject.close()
    }

    @MainActor static func exportLifecycle() async {
        let fixture = Fixture()
        let desk = AtlasReadingDeskModel(transport: fixture.transport)
        desk.capture(path: "/chosen/source.rs", accessLease: nil); await idle(desk)
        desk.bookmarkLabel = "Preserve all notes"; desk.bookmark(); await idle(desk)
        let reviewed = AtlasDeskSourcePackTarget(state: desk.state!)
        let limits = AtlasDeskSourcePackLimits(sourceBytes: 65536)
        desk.exportSourcePack(reviewed, limits: limits, path: "/chosen/evidence.json", accessLease: nil)
        await idle(desk)
        precondition(fixture.exportCount == 1 && desk.lastExport?.succeeded == true)
        precondition(desk.hasUnsavedContents, "a source pack export is not a saved reading desk checkpoint")
        precondition(desk.lastExport?.complete == false && desk.lastExport?.omittedNoSelection == 1)
        precondition(desk.notice.contains("All labels and full notes are included"))

        fixture.loseNextExportReply()
        desk.exportSourcePack(reviewed, limits: limits, path: "/chosen/lost-reply.json", accessLease: nil)
        await idle(desk)
        precondition(fixture.exportCount == 2 && desk.lastExport == nil)
        precondition(desk.notice.contains("outcome could not be verified"), "lost receipt must never replay a filesystem write")

        desk.bookmarkLabel = "New note since review"; desk.bookmark(); await idle(desk)
        desk.exportSourcePack(reviewed, limits: limits, path: "/chosen/stale-review.json", accessLease: nil)
        precondition(!desk.busy && fixture.exportCount == 2 && desk.notice.contains("changed after export review"))
        let current = AtlasDeskSourcePackTarget(state: desk.state!)
        desk.exportSourcePack(current, limits: limits, path: "/chosen/queued-cancel.json", accessLease: nil)
        desk.cancel(); await idle(desk)
        precondition(fixture.exportCount == 2 && desk.lastExport == nil && desk.notice.contains("No destination was created"))

        fixture.returnCanceledExport()
        let entered = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
        fixture.gateNextExport(entered: entered, resume: resume)
        let flag = Flag(); var grant: Grant? = Grant(flag)
        desk.exportSourcePack(current, limits: limits, path: "/chosen/partial.json", accessLease: grant)
        grant = nil; await Self.entered(entered)
        desk.close()
        precondition(flag.value == 0 && fixture.closeCount == 0)
        resume.signal(); await idle(desk)
        precondition(desk.lastExport?.written == 8 && desk.lastExport?.succeeded == false)
        precondition(desk.notice.contains("8 bytes written") && fixture.exportCount == 3)
        for _ in 0..<1000 {
            if flag.value == 1 && fixture.closeCount == 1 { break }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        precondition(flag.value == 1 && fixture.closeCount == 1)
    }

    @MainActor static func checkpointTracking() async {
        let fixture = Fixture(), desk = AtlasReadingDeskModel(transport: fixture.transport)
        precondition(!desk.closeSnapshot.requiresReview && !desk.canSaveCheckpoint)
        desk.start(); await idle(desk)
        precondition(!desk.hasUnsavedContents && !desk.hasCheckpointContents)
        desk.capture(path: "/chosen/source.rs", accessLease: nil); await idle(desk)
        precondition(desk.hasUnsavedContents && desk.closeSnapshot.requiresReview)
        desk.save(path: "/chosen/first.fcbdesk", accessLease: nil); await idle(desk)
        let firstSaved = desk.state!.revision
        precondition(desk.lastSave?.succeeded == true && !desk.hasUnsavedContents)
        precondition(!desk.closeSnapshot.requiresReview && desk.hasCheckpointContents && desk.canSaveCheckpoint,
            "a clean saved desk remains available for Save new checkpoint")

        fixture.rejectNextMutation()
        desk.apply(kind: 1, subject: 1, start: 1); await idle(desk)
        precondition(desk.state?.revision == firstSaved && desk.state!.lastAttempt > firstSaved)
        precondition(!desk.hasUnsavedContents, "an attempt-only failure must not dirty saved content")
        desk.apply(kind: 1, subject: 1, start: 2); await idle(desk)
        precondition(desk.hasUnsavedContents, "accepted reading locations are part of the checkpoint")

        fixture.returnCanceledSave()
        desk.save(path: "/chosen/partial.fcbdesk", accessLease: nil); await idle(desk)
        precondition(desk.lastSave?.succeeded == false && desk.hasUnsavedContents)
        fixture.returnSuccessfulSave(); fixture.loseNextSaveReply()
        desk.save(path: "/chosen/missing-receipt.fcbdesk", accessLease: nil); await idle(desk)
        precondition(desk.lastSave == nil && desk.hasUnsavedContents,
            "a possibly written checkpoint is not a verified persistence baseline")
        fixture.returnWrongSaveRevision()
        desk.save(path: "/chosen/wrong-revision.fcbdesk", accessLease: nil); await idle(desk)
        precondition(desk.lastSave == nil && desk.hasUnsavedContents,
            "a success receipt for another revision must not permit closing")

        let entered = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
        fixture.gateNextSave(entered: entered, resume: resume)
        desk.save(path: "/chosen/complete.fcbdesk", accessLease: nil)
        await Self.entered(entered)
        precondition(desk.closeSnapshot.busy && desk.closeSnapshot.requiresReview)
        desk.cancel(); resume.signal(); await idle(desk)
        precondition(desk.lastSave?.succeeded == true && !desk.hasUnsavedContents,
            "cancellation after an entered successful write cannot erase its terminal receipt")

        desk.bookmarkLabel = "Work after checkpoint"; desk.bookmark(); await idle(desk)
        precondition(desk.hasUnsavedContents && desk.state?.bookmarks.count == 1)
        fixture.acceptNextRestore(); fixture.loseNextMutationReply()
        desk.restore(path: "/chosen/complete.fcbdesk", accessLease: nil); await idle(desk)
        precondition(desk.state?.bookmarks.isEmpty == true && !desk.hasUnsavedContents,
            "a reconciled accepted restore establishes its own clean revision")
        precondition(desk.canSaveCheckpoint)

        desk.bookmarkLabel = "Unknown outcome"; fixture.loseNextMutationReply(stateFailures: 1)
        desk.bookmark(); await idle(desk)
        precondition(desk.state == nil && desk.hasUnverifiedChanges && desk.closeSnapshot.requiresReview,
            "losing state delivery must not make possibly committed personal work look empty")
        desk.reload(); await idle(desk)
        precondition(desk.state?.bookmarks.count == 1 && !desk.hasUnverifiedChanges && desk.hasUnsavedContents)
        fixture.acceptNextRestore(); fixture.failNextPages(2)
        desk.restore(path: "/chosen/complete.fcbdesk", accessLease: nil); await idle(desk)
        precondition(desk.state != nil && desk.page == nil && !desk.hasUnverifiedChanges && !desk.hasUnsavedContents,
            "a missing source page must not invalidate the verified checkpoint restore")
        desk.close()
    }

    @MainActor static func waitForSaves(_ expected: Int, fixture: Fixture, desk: AtlasReadingDeskModel) async {
        for _ in 0..<3000 {
            if fixture.saveCount == expected && !desk.busy { return }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        fatalError("automatic recovery did not finish the expected save")
    }

    @MainActor static func recoveryLifecycle() async {
        let fixture = Fixture(), recovery = RecoveryFixture()
        let desk = AtlasReadingDeskModel(transport: fixture.transport, recovery: recovery.service)
        precondition(desk.supportsRecovery && !desk.recoveryEnabled && recovery.listCount == 0 && recovery.attemptCount == 0)
        desk.capture(path: "/chosen/source.rs", accessLease: nil); await idle(desk)
        try? await Task.sleep(nanoseconds: 450_000_000)
        precondition(fixture.saveCount == 0 && recovery.listCount == 0 && desk.hasUnsavedContents,
            "constructing and using a desk must not opt it into source-bearing local recovery")

        desk.enableRecovery(); await idle(desk)
        precondition(desk.recoveryEnabled && desk.lastRecovery != nil && !desk.recoveryFailed)
        precondition(!desk.hasUnsavedContents && desk.lastSave == nil && fixture.saveCount == 1)
        precondition(recovery.revisions == [desk.state!.revision], "only an exact accepted revision can become the recovery baseline")
        let draftID = UUID(), draftRevision = UUID()
        desk.registerNoteDraft(draftID) { draftRevision }
        precondition(desk.closeSnapshot.requiresReview && !desk.hasUnsavedContents,
            "saving accepted work must not dismiss an unsubmitted editor draft")
        desk.unregisterNoteDraft(draftID)
        precondition(!desk.closeSnapshot.requiresReview)

        // Nearby accepted changes coalesce into one write of the final state.
        desk.apply(kind: 1, subject: 1, start: 1); await idle(desk)
        precondition(desk.hasUnsavedContents && desk.closeSnapshot.requiresReview)
        desk.apply(kind: 1, subject: 1, start: 2); await idle(desk)
        let latest = desk.state!.revision
        await waitForSaves(2, fixture: fixture, desk: desk)
        precondition(recovery.revisions.last == latest && !desk.hasUnsavedContents)

        // A manual checkpoint can make close protection clean before the
        // debounce fires, but local recovery must still catch up to that state.
        desk.apply(kind: 1, subject: 1, start: 3); await idle(desk)
        desk.save(path: "/chosen/also-save-manually.fcbdesk", accessLease: nil); await idle(desk)
        precondition(!desk.hasUnsavedContents)
        await waitForSaves(4, fixture: fixture, desk: desk)
        precondition(recovery.revisions.last == desk.state!.revision)
        let previous = desk.lastRecovery!.id

        desk.disableRecovery()
        desk.bookmarkLabel = "Accepted work while paused"; desk.bookmark(); await idle(desk)
        try? await Task.sleep(nanoseconds: 450_000_000)
        precondition(fixture.saveCount == 4 && desk.hasUnsavedContents && desk.lastRecovery?.id == previous)
        fixture.returnCanceledSave(); desk.enableRecovery(); await idle(desk)
        precondition(desk.recoveryFailed && desk.hasUnsavedContents && desk.lastRecovery?.id == previous)
        let failedAttempts = recovery.attemptCount
        desk.reload(); await idle(desk)
        try? await Task.sleep(nanoseconds: 450_000_000)
        precondition(recovery.attemptCount == failedAttempts,
            "an unconfirmed write must not be automatically retried for the same accepted revision")

        fixture.returnSuccessfulSave(); fixture.loseNextSaveReply()
        desk.saveRecoveryNow(); await idle(desk)
        precondition(desk.recoveryFailed && desk.hasUnsavedContents && desk.lastRecovery?.id == previous)
        fixture.returnWrongSaveRevision(); desk.saveRecoveryNow(); await idle(desk)
        precondition(desk.recoveryFailed && desk.hasUnsavedContents && desk.lastRecovery?.id == previous)
        desk.saveRecoveryNow(); await idle(desk)
        precondition(!desk.recoveryFailed && !desk.hasUnsavedContents && desk.lastRecovery?.id != previous)

        // Listing and restoration remain explicit, and replacement approval is
        // bound to the desk/drafts actually reviewed by the user.
        desk.disableRecovery(); desk.refreshRecoveryCopies(); await idle(desk)
        let selected = desk.lastRecovery!, reviewed = desk.closeSnapshot
        precondition(desk.recoveryEntries.contains(selected))
        desk.bookmarkLabel = "Newer work after restore review"; desk.bookmark(); await idle(desk)
        let newerRevision = desk.state!.revision
        desk.restoreRecovery(selected, replacing: reviewed)
        precondition(!desk.busy && recovery.readCount == 0 && desk.state?.revision == newerRevision)
        desk.registerNoteDraft(draftID) { draftRevision }
        desk.restoreRecovery(selected, replacing: desk.closeSnapshot)
        precondition(!desk.busy && recovery.readCount == 0, "a live note editor cannot be replaced by recovery")
        desk.unregisterNoteDraft(draftID)
        fixture.acceptNextRestore(); fixture.loseNextMutationReply()
        desk.restoreRecovery(selected, replacing: desk.closeSnapshot); await idle(desk)
        precondition(recovery.readCount == 1 && desk.state?.bookmarks.count == 1 && !desk.hasUnsavedContents,
            "a lost restore reply is reconciled once against the accepted checkpoint revision")
        desk.removeRecovery(selected); await idle(desk)
        precondition(recovery.removeCount == 1 && !desk.recoveryEntries.contains(selected))
        precondition(desk.lastRecovery == nil && desk.hasUnsavedContents && desk.closeSnapshot.requiresReview,
            "removing this desk's confirmed copy must restore its close protection")

        desk.enableRecovery(); await idle(desk)
        desk.refreshRecoveryCopies(); await idle(desk)
        let uncertainDelete = desk.lastRecovery!
        recovery.failNextRemovalAfterEffect()
        desk.removeRecovery(uncertainDelete); await idle(desk)
        precondition(desk.lastRecovery == nil && desk.hasUnsavedContents,
            "a deletion may have unpublished the checkpoint before reporting failure")
        let attemptsBeforeRefresh = recovery.attemptCount
        desk.refreshRecoveryCopies(); await idle(desk)
        precondition(!desk.recoveryEntries.contains(uncertainDelete))
        try? await Task.sleep(nanoseconds: 450_000_000)
        precondition(recovery.attemptCount == attemptsBeforeRefresh,
            "deleting recovery must not trigger a surprise automatic rewrite of that revision")

        desk.saveRecoveryNow(); await idle(desk)
        let externallyDeleted = desk.lastRecovery!
        recovery.removeExternally(externallyDeleted)
        desk.refreshRecoveryCopies(); await idle(desk)
        precondition(desk.lastRecovery == nil && desk.hasUnsavedContents && desk.closeSnapshot.requiresReview,
            "refresh must revoke a recovery baseline deleted by another window")
        desk.close()

        let other = AtlasReadingDeskModel(transport: Fixture().transport, recovery: recovery.service)
        other.start(); await idle(other)
        precondition(!other.recoveryEnabled, "automatic recovery consent does not transfer to another desk")
        other.close()
    }

    @MainActor static func recoveryCancellationAndClose() async {
        let pausedFixture = Fixture(), pausedRecovery = RecoveryFixture()
        let paused = AtlasReadingDeskModel(transport: pausedFixture.transport, recovery: pausedRecovery.service)
        paused.capture(path: "/chosen/source.rs", accessLease: nil); await idle(paused)
        paused.enableRecovery(); paused.disableRecovery(); await idle(paused)
        precondition(!paused.recoveryEnabled && paused.hasUnsavedContents && !paused.recoveryFailed)
        precondition(pausedFixture.saveCount == 0 && pausedRecovery.attemptCount == 0,
            "pausing recovery must revoke a write that has not entered the worker")
        paused.close()

        let fixture = Fixture(), recovery = RecoveryFixture()
        let desk = AtlasReadingDeskModel(transport: fixture.transport, recovery: recovery.service)
        desk.capture(path: "/chosen/source.rs", accessLease: nil); await idle(desk)
        desk.enableRecovery(); desk.cancel(); await idle(desk)
        precondition(fixture.saveCount == 0 && recovery.attemptCount == 0 && desk.hasUnsavedContents,
            "cancellation before worker admission must not create a recovery destination")

        let started = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
        fixture.gateNextSave(entered: started, resume: resume)
        desk.saveRecoveryNow(); await entered(started)
        precondition(desk.recoverySaving && desk.closeSnapshot.requiresReview)
        desk.cancel(); resume.signal(); await idle(desk)
        precondition(desk.lastRecovery != nil && !desk.hasUnsavedContents,
            "cancellation cannot discard the receipt of an entered successful recovery save")

        desk.apply(kind: 1, subject: 1, start: 2); await idle(desk)
        let closing = DispatchSemaphore(value: 0), drain = DispatchSemaphore(value: 0)
        fixture.gateNextSave(entered: closing, resume: drain)
        desk.saveRecoveryNow(); await entered(closing)
        let previous = desk.lastRecovery!.id
        desk.close()
        precondition(fixture.closeCount == 0, "the recovery worker must retain its native handle through close")
        drain.signal(); await idle(desk)
        precondition(desk.closed && desk.state == nil && desk.lastRecovery?.id != previous)
        for _ in 0..<1000 {
            if fixture.closeCount == 1 { break }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        precondition(fixture.closeCount == 1 && fixture.saveCount == 2)
    }

    @MainActor static func interruptedRecovery() async {
        for lostReply in [false, true] {
            let fixture = Fixture(), recovery = RecoveryFixture()
            let desk = AtlasReadingDeskModel(transport: fixture.transport, recovery: recovery.service)
            desk.capture(path: "/chosen/source.rs", accessLease: nil); await idle(desk)
            // The engine wrote source-bearing bytes, but its success reply was
            // lost before publication. They appear only after explicit listing.
            fixture.loseNextSaveReply(); desk.enableRecovery(); await idle(desk)
            precondition(desk.recoveryFailed && desk.hasUnsavedContents && desk.lastRecovery == nil)
            desk.disableRecovery()
            let interrupted = recovery.addInterruptedCopy()
            let short = recovery.addInterruptedCopy(checkpointBytes: 8)
            desk.refreshRecoveryCopies(); await idle(desk)
            desk.restoreRecovery(short, replacing: desk.closeSnapshot)
            precondition(!desk.busy && recovery.readCount == 0, "an incomplete header is delete-only")
            fixture.acceptNextRestore()
            if lostReply { fixture.loseNextMutationReply() }
            desk.restoreRecovery(interrupted, replacing: desk.closeSnapshot); await idle(desk)
            precondition(recovery.readCount == 1 && desk.state?.retainedSources == 1)
            precondition(desk.lastRecovery?.id == interrupted.id && desk.hasUnsavedContents && desk.closeSnapshot.requiresReview,
                "a validated interrupted checkpoint must not become a confirmed recovery baseline, even after reconciliation")
            desk.enableRecovery(); await idle(desk)
            precondition(desk.lastRecovery?.confirmed == true && !desk.hasUnsavedContents && !desk.recoveryFailed)
            desk.close()
        }
    }

    @MainActor static func restoredStorageRevalidation() async {
        let fixture = Fixture(), recovery = RecoveryFixture()
        let desk = AtlasReadingDeskModel(transport: fixture.transport, recovery: recovery.service)
        desk.capture(path: "/chosen/source.rs", accessLease: nil); await idle(desk)
        desk.enableRecovery(); await idle(desk)
        desk.refreshRecoveryCopies(); await idle(desk)
        let selected = desk.lastRecovery!
        fixture.acceptNextRestore(); recovery.failNextReadAfterEffect()
        desk.restoreRecovery(selected, replacing: desk.closeSnapshot); await idle(desk)
        precondition(desk.state?.retainedSources == 1 && !desk.hasUnverifiedChanges)
        precondition(desk.recoveryFailed && desk.hasUnsavedContents && desk.closeSnapshot.requiresReview,
            "accepted restore data cannot prove persistence when the store's post-read verification failed")
        let before = recovery.attemptCount
        try? await Task.sleep(nanoseconds: 450_000_000)
        precondition(recovery.attemptCount == before, "uncertain restore storage requires an explicit recovery retry")
        desk.saveRecoveryNow(); await idle(desk)
        precondition(!desk.recoveryFailed && !desk.hasUnsavedContents)
        desk.close()
    }

    static func wireNegativeControls() throws {
        let f = Fixture(), transport = f.transport
        let json = transport.run(f.owner, .start)!
        let state = try AtlasDeskState.decode(json, handle: f.owner)
        precondition(state.revision == 0)
        var object = try JSONSerialization.jsonObject(with: Data(json.utf8)) as! [String: Any]
        func rejects(_ object: [String: Any]) {
            let text = String(decoding: try! JSONSerialization.data(withJSONObject: object), as: UTF8.self)
            do { _ = try AtlasDeskState.decode(text, handle: f.owner); fatalError("malformed response was accepted") }
            catch { }
        }
        object["owner"] = "044"; rejects(object)
        object["owner"] = String(f.owner); object["model_revision"] = "18446744073709551616"; rejects(object)
        object["model_revision"] = "0"; object["native_presented"] = true; rejects(object)
        object["native_presented"] = false; object["additional_source_bytes_read"] = "1"; rejects(object)
        object["additional_source_bytes_read"] = "0"; object["retained_source_bytes"] = String(32 * 1024 * 1024 + 1); rejects(object)
        object["retained_source_bytes"] = "0"; object["error"] = ["code": "DESK_ERROR"]; rejects(object)

        let captured = transport.run(f.owner, .capture(revision: 0, attempt: 1, path: "/chosen/source.rs"))!
        let savedState = try AtlasDeskState.decode(captured, handle: f.owner)
        let savedJSON = transport.run(f.owner, .save(revision: 1, path: "/chosen/new.fcbdesk", sourceBytes: savedState.retainedBytes))!
        let saved = try JSONSerialization.jsonObject(with: Data(savedJSON.utf8)) as! [String: Any]
        let acceptedSave = try AtlasDeskSaveReceipt.decode(savedJSON, state: savedState)
        precondition(acceptedSave.succeeded)
        func rejectsSave(_ object: [String: Any]) {
            let text = String(decoding: try! JSONSerialization.data(withJSONObject: object), as: UTF8.self)
            do { _ = try AtlasDeskSaveReceipt.decode(text, state: savedState); fatalError("contradictory write receipt accepted") }
            catch { }
        }
        var invalid = saved; invalid["checkpoint_digest"] = NSNull(); rejectsSave(invalid)
        invalid = saved; invalid["owner"] = "45"; rejectsSave(invalid)
        invalid = saved; invalid["model_revision"] = "0"; rejectsSave(invalid)
        invalid = saved; invalid["last_attempt"] = "0"; rejectsSave(invalid)
        invalid = saved; invalid["checkpoint_digest"] = NSNull(); invalid["checkpoint_bytes"] = "0"; invalid["bytes_written"] = "0"; rejectsSave(invalid)
        invalid = saved; invalid["status"] = "error"; invalid["error"] = ["code": "DESK_CHECKPOINT_IO"]; rejectsSave(invalid)
        invalid["effect"] = "none"; rejectsSave(invalid)
        invalid["effect"] = "destination-created-incomplete"; rejectsSave(invalid)
        invalid["effect"] = "complete-file-sync-unconfirmed"; invalid["bytes_written"] = "8"; rejectsSave(invalid)

        let target = AtlasDeskSourcePackTarget(state: savedState)
        let limits = AtlasDeskSourcePackLimits(sourceBytes: 65536)
        let exportJSON = transport.run(f.owner, .exportSourcePack(revision: 1, path: "/chosen/evidence.json", limits: limits))!
        let exported = try JSONSerialization.jsonObject(with: Data(exportJSON.utf8)) as! [String: Any]
        let acceptedExport = try AtlasDeskSourcePackReceipt.decode(exportJSON, target: target, limits: limits)
        precondition(acceptedExport.succeeded)
        func rejectsExport(_ object: [String: Any]) {
            let text = String(decoding: try! JSONSerialization.data(withJSONObject: object), as: UTF8.self)
            do { _ = try AtlasDeskSourcePackReceipt.decode(text, target: target, limits: limits); fatalError("contradictory export receipt accepted") }
            catch { }
        }
        invalid = exported; invalid["model_revision"] = "2"; rejectsExport(invalid)
        invalid = exported; invalid["export_digest"] = NSNull(); rejectsExport(invalid)
        invalid = exported; invalid["bytes_written"] = "257"; rejectsExport(invalid)
        invalid = exported; invalid["export_scalars"] = "257"; rejectsExport(invalid)
        invalid = exported; invalid["complete"] = false; rejectsExport(invalid)
        invalid = exported; invalid["contains_source_payloads"] = true; rejectsExport(invalid)
        invalid = exported; invalid["live_roots_accessed"] = true; rejectsExport(invalid)
        invalid = exported; invalid["budgets"] = ["source_bytes": "65537", "output_bytes": "16777216", "output_scalars": "16777216"]; rejectsExport(invalid)
        invalid = exported; invalid["coverage"] = ["total_bookmarks": "0", "exported_selections": "18446744073709551615",
            "omitted_no_selection": "0", "omitted_source_limit": "0", "omitted_output_limit": "0", "selected_original_bytes": "0"]; rejectsExport(invalid)
        invalid = exported; invalid["effect"] = "destination-created-incomplete"; rejectsExport(invalid)
        invalid["status"] = "error"; invalid["error"] = ["code": "DESK_CHECKPOINT_CANCELED"]; rejectsExport(invalid)
    }
}
