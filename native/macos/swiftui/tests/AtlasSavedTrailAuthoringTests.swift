import Foundation
import Dispatch

/// Inject only the publication boundary. Real archive pinning and canonical
/// FCBT encoding are covered by the host fixtures, not reconstructed in Swift.
private final class SavedTrailWriteFixture: @unchecked Sendable {
    let trail = SavedTrailFixture()
    let resume = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var attempts: [UInt64: UInt64] = [:]
    private var receipts: [UInt64: (UInt64, String)] = [:]
    private var writes: [AtlasSavedTrailWriteRequest] = []
    private var lookups: [UInt64] = []
    private var fault: String?
    private var loseReceipts = false
    private var block = false
    private var entered = false
    private func locked<T>(_ work: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return work() }
    var requests: [AtlasSavedTrailWriteRequest] { locked { writes } }
    var receiptReads: [UInt64] { locked { lookups } }
    var didEnter: Bool { locked { entered } }
    func next(_ value: String) { locked { fault = value } }
    func restoreReceipts() { locked { loseReceipts = false } }
    func blockNext() { locked { block = true; entered = false } }

    var saved: AtlasSavedTransport {
        let inner = trail.saved
        return AtlasSavedTransport(create: inner.create, run: { [self] owner, request in
            let response = inner.run(owner, request)
            switch request {
            case .open, .info:
                guard let response else { return nil }
                return locked {
                    if case .open = request { attempts[owner] = 0; receipts[owner] = nil }
                    var value = trail.object(response)
                    value["last_trail_write_attempt"] = String(attempts[owner] ?? 0)
                    value["last_trail_write_operation"] = receipts[owner].map { String($0.0) as Any } ?? NSNull()
                    return trail.base.encode(value)
                }
            default: return response
            }
        }, cancel: inner.cancel, close: inner.close, retirementFailed: inner.retirementFailed, readerDomain: inner.readerDomain)
    }
    func reply(_ request: AtlasSavedTrailWriteRequest, failure: String? = nil) -> [String: Any] {
        let member = request.selection.member
        let effect = failure == "exists" || failure == "verify" ? "none" : failure == "partial"
            ? "destination-created-incomplete" : "complete-file-and-parent-sync-requested"
        let encoded = failure != "verify", complete = failure != "exists" && failure != "partial" && failure != "verify"
        var value = trail.base.header(request.identity.owner, "trail-write")
        value.merge(["operation": String(request.operation), "last_trail_write_attempt": String(request.operation),
            "destination": trail.base.path(Array(request.destination.utf8), request.destination),
            "mode": request.parent == nil ? "new" : "append",
            "parent_generation": request.parent.map { String($0.generation) as Any } ?? NSNull(),
            "parent_digest": request.parent.map { $0.digest as Any } ?? NSNull(),
            "parent_items": String(request.parent?.items ?? 0), "member": String(member.id), "file_id": String(member.file),
            "source_revision": String(member.file), "path": trail.base.path(member.path.bytes, member.path.display),
            "source_digest": member.digest!, "source_bytes": String(member.observedBytes),
            "selection": ["start": String(request.selection.range.start), "end": String(request.selection.range.end)],
            "title": request.title, "rationale": request.rationale, "source_verified": encoded,
            "member_bytes_read": String(member.observedBytes), "member_read_calls": member.observedBytes == 0 ? "0" : "1",
            "trail_bytes": encoded ? "8192" : "0", "trail_digest": encoded ? String(repeating: "c", count: 64) as Any : NSNull(),
            "total_items": encoded ? String(request.totalItems) as Any : NSNull(),
            "reference_bytes": encoded ? String(request.referenceBytes) as Any : NSNull(),
            "effect": effect, "bytes_written": complete ? "8192" : failure == "partial" ? "77" : "0",
            "complete_artifact": complete, "attach_eligible": complete, "status": failure == nil ? "ok" : "error",
            "error": failure == nil ? NSNull() : ["code": "TRAIL_\(failure!)", "next_action": "inspect-and-choose-new-destination"],
            "archive_path_reopened": false, "live_source_reopened": false, "source_payload_embedded": false,
            "trail_attached": false, "accepted_trail_unchanged": true, "accepted_query_unchanged": true,
            "source_derived_sensitive": true, "encrypted": false, "overwritten": false,
            "power_loss_qualified": false, "truncated": false]) { _, new in new }
        return value
    }
    var transport: AtlasSavedTrailTransport {
        AtlasSavedTrailTransport(run: trail.trails.run, write: { [self] owner, request in
            precondition(!Thread.isMainThread && owner == request.identity.owner)
            let current = locked { () -> (String?, Bool) in
                precondition(request.operation > attempts[owner, default: 0])
                attempts[owner] = request.operation; writes.append(request)
                let pause = block; block = false; entered = pause
                let current = fault; fault = nil
                return (current, pause)
            }
            if current.1 { precondition(resume.wait(timeout: .now() + .seconds(5)) == .success) }
            let failure = ["exists", "partial", "verify"].contains(current.0 ?? "") ? current.0 : nil
            let json = trail.base.encode(reply(request, failure: failure))
            return locked {
                receipts[owner] = (request.operation, json)
                if current.0 == "unknown" { loseReceipts = true }
                return current.0 == "lost" || current.0 == "unknown" ? nil : json
            }
        }, writeReceipt: { [self] owner, operation in
            precondition(!Thread.isMainThread)
            return locked {
                lookups.append(operation)
                guard !loseReceipts, let receipt = receipts[owner], receipt.0 == operation else { return nil }
                return receipt.1
            }
        })
    }
    /// Fixed source spelling proves the draft consumes an engine-qualified
    /// selected range rather than treating native string indices as bytes.
    var reader: AtlasReaderTransport {
        let inner = trail.base.readerTransport(), base = trail.base
        return AtlasReaderTransport(create: inner.create, open: inner.open, window: inner.window, lines: inner.lines,
            cancel: inner.cancel, close: inner.close, retirementFailed: inner.retirementFailed, fork: inner.fork,
            comparison: inner.comparison, textSelection: .init(resolve: { owner, start, end, first, last in
                precondition(!Thread.isMainThread && start == 0 && end == 209 && first == 202 && last == 208)
                let member = base.members[1]
                return base.encode(["schema": "fcb.reader-session/1", "status": "ok", "command": "resolve-selection",
                    "owner": String(owner), "file_id": "1", "source_revision": "1", "captured_bytes": "209",
                    "encoding": "utf8", "path": base.path(member.path, member.display),
                    "additional_source_bytes_read": "0", "native_presented": false, "selection_namespace": "native-text",
                    "window_original_range": ["start": "0", "end": "209"],
                    "window_utf8_range": ["start": "202", "end": "208"],
                    "original_range": ["start": "202", "end": "208"], "text": "banana",
                    "original_hex": "62616e616e61", "contains_replacements": false])
            }))
    }
}

enum AtlasSavedTrailAuthoringTests {
    @MainActor private static func wait(_ message: String, _ ready: () -> Bool) async {
        for _ in 0..<2000 { if ready() { return }; try? await Task.sleep(nanoseconds: 1_000_000) }
        preconditionFailure(message)
    }
    private static func reject(_ work: () throws -> Void) {
        do { try work(); preconditionFailure("invalid trail write admitted") } catch { }
    }
    @MainActor static func run() async throws {
        let fixture = SavedTrailWriteFixture(), base = fixture.trail.base
        let model = AtlasSavedRepositoryModel(transport: fixture.saved, trails: fixture.transport)
        model.open(path: "/saved/repository.fcbs", accessLease: nil)
        await wait("authoring archive") { !model.busy }
        let identity = model.info!.identity, member = model.members!.items[1]
        let capture = try await model.openMember(member, from: identity, readerTransport: fixture.reader)
        precondition(model.canAuthorTrail(from: capture))
        let draft = try model.trailDraft(from: capture)
        precondition(draft.parent == nil && !draft.append && draft.range?.end == member.observedBytes)
        precondition(draft.presets.contains { $0.id == "file" && $0.range.start == 0 && $0.range.end == member.observedBytes })
        draft.choose(draft.presets.first { $0.id == "point" }!)
        precondition(draft.range?.start == draft.range?.end && draft.closeProtectionRevision != nil)
        draft.startInput = "01"; precondition(draft.validation(in: model) != nil)
        draft.startInput = "0"; draft.endInput = String(member.observedBytes + 1)
        precondition(draft.validation(in: model) != nil)
        draft.choose(draft.presets.first { $0.id == "file" }!)
        draft.rationale = String(repeating: "é", count: 2049)
        precondition(draft.validation(in: model) != nil && draft.rationale.count == 2049)
        draft.title = "e\u{301}\0title"; draft.rationale = "Exact e\u{301}\0note\nwith line endings."
        let selection = try draft.selection()
        let request = try AtlasSavedTrailWriteRequest(selection: selection, operation: UInt64.max,
            parent: nil, title: draft.title, rationale: draft.rationale, destination: "/saved/guide-e\u{301}.fcbt")
        let decode: ([String: Any]) throws -> AtlasSavedTrailWriteReceipt = {
            try AtlasSavedTrailWriteReceipt.decode(base.encode($0), request: request)
        }
        let receipt = try decode(fixture.reply(request))
        precondition(receipt.succeeded && receipt.completeArtifact && receipt.totalItems == 1 && receipt.referenceBytes == 209)
        for (key, value) in [("operation", "01" as Any), ("owner", "999"), ("last_trail_write_attempt", "1"),
            ("parent_generation", "1"), ("parent_items", "1"), ("source_revision", "99"),
            ("selection", ["start": "0", "end": "208"]), ("title", "é\0title"),
            ("rationale", "changed note"), ("source_verified", false), ("member_bytes_read", "208"),
            ("member_read_calls", "0"), ("trail_bytes", "4194305"), ("bytes_written", "8193"),
            ("reference_bytes", "208"), ("total_items", "2"), ("complete_artifact", false),
            ("attach_eligible", false), ("archive_path_reopened", true), ("source_payload_embedded", true),
            ("trail_attached", true), ("overwritten", true), ("accepted_trail_unchanged", false), ("truncated", true)] {
            var invalid = fixture.reply(request); invalid[key] = value; reject { _ = try decode(invalid) }
        }
        for failure in ["exists", "partial", "verify"] {
            let result = try decode(fixture.reply(request, failure: failure))
            precondition(!result.succeeded && !result.completeArtifact)
            if failure == "exists" { precondition(result.digest != nil && result.bytesWritten == 0) }
        }
        reject { _ = try AtlasSavedTrailWriteRequest(selection: selection, operation: 0, parent: nil,
            title: "", rationale: "", destination: "/saved/new.fcbt") }
        reject { _ = try AtlasSavedTrailWriteRequest(selection: selection, operation: 1, parent: nil,
            title: String(repeating: "x", count: 257), rationale: "", destination: "/saved/new.fcbt") }
        reject { _ = try AtlasSavedTrailWriteRequest(selection: selection, operation: 1, parent: nil,
            title: "", rationale: "", destination: "/saved/../new.fcbt") }

        fixture.next("lost")
        let submitted = model.writeTrail(from: capture, selection: selection, parent: nil,
            title: draft.title, rationale: draft.rationale, destination: "/saved/new.fcbt", accessLease: nil)!
        draft.submitted(submitted)
        await wait("lost write receipt recovered") { !model.busy }
        draft.accept(model.trailWriteReceipt)
        precondition(model.trailWriteOutcomeKnown && draft.receipt?.succeeded == true && draft.closeProtectionRevision == nil)
        precondition(fixture.requests.count == 1 && fixture.receiptReads == [1] && model.trailPage == nil)
        precondition(fixture.requests[0].rationale.utf8.elementsEqual(draft.rationale.utf8))
        draft.rationale += " edited"; precondition(draft.closeProtectionRevision != nil)

        fixture.next("unknown")
        _ = model.writeTrail(from: capture, selection: selection, parent: nil, title: "Second", rationale: "Keep me",
            destination: "/saved/uncertain.fcbt", accessLease: nil)
        await wait("unknown write outcome") { !model.busy }
        precondition(!model.trailWriteOutcomeKnown && fixture.requests.count == 2)
        precondition(model.writeTrail(from: capture, selection: selection, parent: nil, title: "Second", rationale: "Keep me",
            destination: "/saved/uncertain.fcbt", accessLease: nil) == nil)
        model.reloadTrailWriteReceipt(); await wait("still unknown receipt") { !model.busy }
        precondition(!model.trailWriteOutcomeKnown && fixture.requests.count == 2)
        fixture.restoreReceipts(); model.reloadTrailWriteReceipt(); await wait("recovered receipt") { !model.busy }
        precondition(model.trailWriteOutcomeKnown && fixture.requests.count == 2)

        model.attachTrail(path: "/saved/parent.fcbt", accessLease: nil)
        await wait("append parent") { !model.busy }
        let appended = try model.trailDraft(from: capture), parent = appended.parent!
        precondition(appended.append && appended.effectiveTitle == parent.title && parent.items == 34)
        appended.choose(appended.presets.first { $0.id == "point" }!)
        let point = try appended.selection()
        fixture.next("exists")
        let failed = model.writeTrail(from: capture, selection: point, parent: parent, title: parent.title,
            rationale: "Repeat this visit.", destination: "/saved/existing.fcbt", accessLease: nil)!
        await wait("create-only refusal") { !model.busy }
        precondition(model.trailWriteReceipt?.effect == .none && model.trailWriteReceipt?.succeeded == false)
        precondition(model.trailWriteReceipt?.totalItems == 35 && model.trailWriteReceipt?.referenceBytes == parent.referenceBytes)
        precondition(model.trailPage?.generation == parent.generation && failed.parent?.digest == parent.digest)
        model.attachTrail(path: "/saved/replaced-parent.fcbt", accessLease: nil)
        await wait("replace append parent") { !model.busy }
        precondition(appended.validation(in: model) != nil)
        let before = fixture.requests.count
        precondition(model.writeTrail(from: capture, selection: point, parent: parent, title: parent.title,
            rationale: "stale", destination: "/saved/stale.fcbt", accessLease: nil) == nil)
        precondition(fixture.requests.count == before)
        appended.append = false; precondition(appended.validation(in: model) == nil)

        // A selected native character range is independently mapped by the
        // reader service. The initial é occupies two bytes, one UTF-16 unit.
        capture.reader.updateTextSelection(source: capture.reader.source!, navigation: capture.reader.navigation,
            range: NSRange(location: 201, length: 6))
        await wait("native source selection") { capture.reader.textSelection != nil }
        let selected = try model.trailDraft(from: capture)
        precondition(selected.presets[0].id == "text" && selected.range?.start == 202 && selected.range?.end == 208)
        capture.reader.updateTextSelection(source: capture.reader.source!, navigation: capture.reader.navigation, range: nil)

        // A cancellation after writer entry retains the destination grant
        // until drain and then recovers the actual publication effect.
        final class Grant { }
        var grant: Grant? = Grant(); weak var weakGrant = grant
        fixture.blockNext()
        _ = model.writeTrail(from: capture, selection: point, parent: nil, title: "Cancellation", rationale: "Retain this note",
            destination: "/saved/canceled.fcbt", accessLease: grant)
        grant = nil
        await wait("trail writer entered") { fixture.didEnter }
        model.cancel(); precondition(model.busy && model.trailWriteCancellationRequested && weakGrant != nil)
        fixture.resume.signal(); await wait("trail writer drained") { !model.busy }
        await wait("trail output grant drained") { weakGrant == nil }
        precondition(model.trailWriteOutcomeKnown && model.trailWriteReceipt?.completeArtifact == true)
        precondition(fixture.requests.count == before + 1)

        model.searchInput = "ana"; model.search(); await wait("saved match") { !model.busy }
        let hit = try await model.openHit(model.searchPage!.hits[0], from: model.searchPage!, readerTransport: fixture.reader)
        let matchDraft = try model.trailDraft(from: hit)
        precondition(matchDraft.presets[0].id == "saved-match" && matchDraft.range?.start == hit.match?.start &&
            matchDraft.range?.end == hit.match?.end)
        model.closeArchive()
        precondition(!model.canAuthorTrail(from: capture) && selected.validation(in: model) != nil)
        precondition(selected.rationale.isEmpty && !base.calls.contains("LIVE-OPEN"))
        capture.reader.close(); hit.reader.close(); model.close()
        print("Saved trail authoring bounds, exact ranges, immutable parents, publication effects, recovery and grant-drain fixtures passed")
    }
}
