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
    private var closes = 0
    private var lostMutationResponse = false
    private var canceledSave = false
    private var openedGate: DispatchSemaphore?
    private var enteredGate: DispatchSemaphore?
    private enum Gate: Equatable { case start, capture, save }
    private var gatedRequest: Gate?
    private(set) var owner: UInt64 = 44
    let readerDomain = UUID()
    let text = "captured source\n"
    var transport: AtlasReadingDeskTransport {
        AtlasReadingDeskTransport(create: { [self] in owner }, run: { [self] in response($0, $1) },
            cancel: { _ in true }, close: { [self] _ in locked { closes += 1 }; return true },
            retirementFailed: { fatalError("retirement refused") }, readerDomain: readerDomain)
    }
    func loseNextMutationReply() { locked { lostMutationResponse = true } }
    func returnCanceledSave() { locked { canceledSave = true } }
    func gateNextStart(entered: DispatchSemaphore, resume: DispatchSemaphore) { gate(.start, entered, resume) }
    func gateNextCapture(entered: DispatchSemaphore, resume: DispatchSemaphore) { gate(.capture, entered, resume) }
    func gateNextSave(entered: DispatchSemaphore, resume: DispatchSemaphore) { gate(.save, entered, resume) }
    private func gate(_ request: Gate, _ entered: DispatchSemaphore, _ resume: DispatchSemaphore) {
        locked { gatedRequest = request; enteredGate = entered; openedGate = resume }
    }
    var mutationCount: Int { locked { mutations } }
    var startCount: Int { locked { starts } }
    var saveCount: Int { locked { saves } }
    var closeCount: Int { locked { closes } }
    private func locked<T>(_ body: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return body() }
    private func response(_ handle: UInt64, _ request: AtlasDeskRequest) -> String? {
        let kind: Gate?
        switch request {
        case .start: kind = .start
        case .capture: kind = .capture
        case .save: kind = .save
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
            case .state: return state()
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
                revision = attempt; lastAttempt = attempt; mutations += 1
                if kind == 3 { marks.append((UInt64(marks.count + 1), label, position)) }
                if kind == 1 { position = start }
                if lostMutationResponse { lostMutationResponse = false; return nil }
                return state()
            case .restore:
                return error("DESK_CHECKPOINT_INTEGRITY")
            case .window(let expected, let pane, let offset):
                guard expected == revision, pane == 1, offset <= UInt64(text.utf8.count) else { return error("DESK_INVALID_LOCATION") }
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
                var w = header("save")
                w["status"] = canceledSave ? "error" : "ok"
                w["effect"] = canceledSave ? "destination-created-incomplete" : "complete-file-and-parent-sync-requested"
                w["bytes_written"] = canceledSave ? "8" : "256"; w["checkpoint_bytes"] = "256"
                w["source_bytes"] = String(bytes); w["checkpoint_digest"] = String(repeating: "a", count: 64)
                w["contains_source_payloads"] = true; w["contains_personal_labels"] = true
                w["encrypted"] = false; w["overwritten"] = false
                w["error"] = canceledSave ? ["code": "DESK_CHECKPOINT_CANCELED"] : NSNull()
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
            var bookmark = location(mark.2); bookmark["bookmark"] = String(mark.0); bookmark["label"] = mark.1; return bookmark
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
        print("Reading desk wire, exact-source model, lost-reply reconciliation, canceled-save receipts and grant drain tests passed")
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
        invalid = saved; invalid["checkpoint_digest"] = NSNull(); invalid["checkpoint_bytes"] = "0"; invalid["bytes_written"] = "0"; rejectsSave(invalid)
        invalid = saved; invalid["status"] = "error"; invalid["error"] = ["code": "DESK_CHECKPOINT_IO"]; rejectsSave(invalid)
        invalid["effect"] = "none"; rejectsSave(invalid)
        invalid["effect"] = "destination-created-incomplete"; rejectsSave(invalid)
        invalid["effect"] = "complete-file-sync-unconfirmed"; invalid["bytes_written"] = "8"; rejectsSave(invalid)
    }
}
