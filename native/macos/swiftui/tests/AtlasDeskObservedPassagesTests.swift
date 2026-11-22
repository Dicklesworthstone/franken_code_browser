import Foundation
import Dispatch

/// Exercises the real native model/decoders and observed-reader lifetime
/// adapter. This injected transport does not qualify Rust persistence or Mac UI.
private final class ObservedDeskFixture: @unchecked Sendable {
    let owner: UInt64 = 0x8000000000000011
    let domain = UUID()
    private let lock = NSLock()
    private var revision: UInt64 = 0, attempt: UInt64 = 0, nextPassage: UInt64 = 1
    private var observation: [String: Any]?
    private var input: AtlasObservedReaderSelection?
    private var passages: [[String: Any]] = []
    private var checkpoint: [[String: Any]]?
    private var lost = false, failedReads = 0, calls = 0
    private var gate: ObservedReaderGate?
    var mutationCount: Int { locked { calls } }
    func loseReply(stateFailures: Int = 0) { locked { lost = true; failedReads = stateFailures } }
    func blockNextKeep(_ gate: ObservedReaderGate) { locked { self.gate = gate } }
    func configure(_ selected: AtlasObservedReaderSelection) {
        locked {
            input = selected
            let o = selected.observation
            observation = ["observation": "1", "origin_owner": String(o.identity.owner),
                "origin_file": String(o.identity.file), "origin_observation": String(o.id),
                "path": o.identity.displayPath, "path_hex": ObservedReaderFixture.hex(o.identity.path),
                "encoding": o.request.encoding.rawValue, "consistency": o.consistency,
                "extent_sha256": o.extentSHA256, "captured_range": ObservedReaderFixture.range(o.capturedRange.start, o.capturedRange.end),
                "observed_length": String(o.observedLength), "final_observed_length": o.finalObservedLength.map(String.init) as Any? ?? NSNull(),
                "capture_kind": "observed-extent", "whole_file_immutable": false, "source_reopened": false,
                "requested_capture_range": ObservedReaderFixture.range(o.requestedCapture.start, o.requestedCapture.end),
                "requested_capture_filled": o.captureFilled]
        }
    }
    private func locked<T>(_ body: () throws -> T) rethrows -> T {
        lock.lock(); defer { lock.unlock() }; return try body()
    }
    private func encode(_ object: [String: Any]) -> String { try! ObservedReaderFixture.encode(object) }
    private func header(_ command: String) -> [String: Any] {
        ["schema": "fcb.reading-desk/1", "status": "ok", "command": command, "owner": String(owner),
         "model_revision": String(revision), "last_attempt": String(attempt),
         "additional_source_bytes_read": "0", "native_presented": false]
    }
    func snapshot() -> String { locked { encode(state()) } }
    private func state() -> [String: Any] {
        var out = header("state")
        out["panes"] = [String](); out["bookmarks"] = [String](); out["active_pane"] = NSNull()
        out["retained_sources"] = passages.isEmpty ? "0" : "1"
        out["retained_source_bytes"] = passages.isEmpty ? "0" : String(input!.observation.capturedRange.count)
        out["can_go_back"] = false; out["can_go_forward"] = false
        out["persistence"] = "explicit-checkpoint"; out["autosave"] = false
        out["observations"] = passages.isEmpty ? [] : [observation!]
        out["observed_passages"] = passages
        return out
    }
    private func mutationReply() -> String? {
        calls += 1
        if lost { lost = false; return nil }
        return encode(state())
    }
    private func admits(_ h: UInt64, _ revision: UInt64, _ attempt: UInt64) -> Bool {
        guard h == owner, revision == self.revision, attempt > self.attempt else { return false }
        self.attempt = attempt
        return true
    }
    var transport: AtlasReadingDeskTransport {
        AtlasReadingDeskTransport(create: { [self] in owner }, run: { [self] handle, request in
            locked {
                precondition(handle == owner)
                switch request {
                case .start: return encode(state())
                case .state:
                    if failedReads > 0 { failedReads -= 1; return nil }
                    return encode(state())
                case .save(let expected, _, let bytes):
                    precondition(expected == revision && bytes == input!.observation.capturedRange.count)
                    checkpoint = passages
                    var out = header("save")
                    out["effect"] = "complete-file-and-parent-sync-requested"
                    out["bytes_written"] = "1024"; out["checkpoint_bytes"] = "1024"
                    out["source_bytes"] = String(bytes); out["checkpoint_digest"] = String(repeating: "b", count: 64)
                    out["contains_source_payloads"] = true; out["contains_personal_labels"] = true
                    out["encrypted"] = false; out["overwritten"] = false; out["error"] = NSNull()
                    return encode(out)
                case .restore(let expected, let next, _):
                    guard admits(handle, expected, next), let checkpoint else { return nil }
                    passages = checkpoint.map { old in
                        var fresh = old; fresh["passage"] = String(nextPassage); nextPassage += 1
                        return fresh
                    }
                    revision = next
                    return mutationReply()
                default: return nil
                }
            }
        }, cancel: { _ in true }, close: { _ in true }, retirementFailed: { },
        observed: .init(domain: domain, keep: { [self] h, expected, next, reader, id, start, end in
            let block = locked { () -> ObservedReaderGate? in
                let current = gate; gate = nil; return current
            }
            block?.wait()
            return locked {
                guard admits(h, expected, next), let input, reader == input.observation.identity.owner,
                      id == input.observation.id, start == input.originalRange.start, end == input.originalRange.end else { return nil }
                passages.append(["passage": String(nextPassage), "observation": "1", "update_sequence": "1",
                    "label": "Kept passage", "body": "", "selection": ObservedReaderFixture.range(start, end)])
                nextPassage += 1; revision = next
                return mutationReply()
            }
        }, read: { [self] h, expected, id in
            locked {
                guard h == owner, expected == revision, let input,
                      let passage = passages.first(where: { $0["passage"] as? String == String(id) }) else { return nil }
                var out = header("observed-passage")
                out["observation"] = observation!; out["passage"] = passage
                out["text"] = input.text; out["original_hex"] = input.originalHex
                out["contains_replacements"] = input.containsReplacements
                out["capture_kind"] = "observed-extent"; out["whole_file_immutable"] = false; out["source_reopened"] = false
                return encode(out)
            }
        }, edit: { [self] h, expected, next, id, sequence, label, body in
            locked {
                guard admits(h, expected, next),
                      let index = passages.firstIndex(where: { $0["passage"] as? String == String(id) }),
                      passages[index]["update_sequence"] as? String == String(sequence) else { return nil }
                passages[index]["label"] = label; passages[index]["body"] = body
                passages[index]["update_sequence"] = String(sequence + 1); revision = next
                return mutationReply()
            }
        }, forget: { [self] h, expected, next, id, sequence in
            locked {
                guard admits(h, expected, next),
                      let index = passages.firstIndex(where: { $0["passage"] as? String == String(id) }),
                      passages[index]["update_sequence"] as? String == String(sequence) else { return nil }
                passages.remove(at: index); revision = next
                return mutationReply()
            }
        }))
    }
}

@main private enum AtlasDeskObservedPassagesTests {
    @MainActor private static func wait(_ label: String, _ ready: () -> Bool) async {
        for _ in 0..<4000 {
            if ready() { return }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        preconditionFailure("Timed out: " + label)
    }
    private static func rejects(_ work: () throws -> Void) {
        do { try work(); preconditionFailure("Accepted invalid observed passage") }
        catch { }
    }
    @MainActor static func main() async throws {
        let probe = ObservedReaderProbe(), fixture = ObservedDeskFixture()
        var sourceTransport = probe.transport; sourceTransport.domain = fixture.domain
        let source = AtlasObservedReaderModel(transport: sourceTransport); source.pageBytes = 4
        let input = try AtlasObservedReaderInput(root: "/repo", relativePath: Array("large.rs".utf8))
        source.open(.init(input: input, project: UUID(), access: nil))
        await wait("open") { source.page != nil && !source.busy }
        source.observe(offset: (1 << 33) + 4)
        await wait("large offset") { source.page?.visibleRange.start == (1 << 33) + 4 && !source.busy }
        source.updateTextSelection(source: source.source!, navigation: source.navigation, range: NSRange(location: 0, length: 3))
        await wait("resolved source selection") { source.selectionForRetention != nil }
        fixture.configure(source.selectionForRetention!)
        let desk = AtlasReadingDeskModel(transport: fixture.transport)
        let foreignDesk = AtlasReadingDeskModel(transport: ObservedDeskFixture().transport)
        precondition(!foreignDesk.canKeepObserved(reader: source), "Handle numbers from another transport are not capabilities")
        foreignDesk.close()
        fixture.loseReply()
        let reads = probe.windowOffsets.count
        precondition(desk.keepObserved(reader: source))
        await wait("lost import receipt reconciled") { !desk.busy && desk.state?.observedPassages.count == 1 }
        precondition(fixture.mutationCount == 1 && desk.state!.panes.isEmpty && desk.page == nil)
        precondition(desk.hasUnsavedContents && desk.canSaveCheckpoint && desk.closeSnapshot.requiresReview)
        precondition(desk.state!.retainedSources == 1 && probe.windowOffsets.count == reads)
        let state = desk.state!, passage = state.observedPassages[0]
        let target = AtlasDeskObservedTarget(state: state, passage: passage)!
        let page = try await desk.readObservedPassage(target)
        precondition(page.text == "old" && page.originalHex == "6f6c64")
        precondition(page.target.passage.selection.start > UInt64(UInt32.max))
        precondition(page.withLocationText.contains("\"whole_file_immutable\":false"))
        precondition(page.withLocationText.contains(String(passage.selection.start)))

        // Saved provenance and labels are counted bytes, not fresh C path or
        // label arguments. Embedded NUL from a valid checkpoint survives read.
        var savedNUL = try JSONSerialization.jsonObject(with: Data(fixture.snapshot().utf8)) as! [String: Any]
        var savedObservations = savedNUL["observations"] as! [[String: Any]]
        savedObservations[0]["path_hex"] = "6f726967696e616c2fff002e7273"
        savedObservations[0]["path"] = "original/\\xff\\0.rs"
        savedNUL["observations"] = savedObservations
        var savedPassages = savedNUL["observed_passages"] as! [[String: Any]]
        savedPassages[0]["label"] = "Question\0"
        savedNUL["observed_passages"] = savedPassages
        let admittedNUL = try AtlasDeskState.decode(ObservedReaderFixture.encode(savedNUL), handle: fixture.owner)
        precondition(admittedNUL.observedPassages[0].label.utf8.last == 0)
        precondition(admittedNUL.observations[0].pathHex == "6f726967696e616c2fff002e7273")

        var wrong = try JSONSerialization.jsonObject(with: Data(fixture.snapshot().utf8)) as! [String: Any]
        var changed = wrong["observed_passages"] as! [[String: Any]]
        changed[0]["selection"] = ObservedReaderFixture.range(0, 3); wrong["observed_passages"] = changed
        rejects { _ = try AtlasDeskState.decode(ObservedReaderFixture.encode(wrong), handle: fixture.owner) }
        wrong = try JSONSerialization.jsonObject(with: Data(fixture.snapshot().utf8)) as! [String: Any]
        wrong["observations"] = NSNull()
        rejects { _ = try AtlasDeskState.decode(ObservedReaderFixture.encode(wrong), handle: fixture.owner) }
        wrong.removeValue(forKey: "observations")
        rejects { _ = try AtlasDeskState.decode(ObservedReaderFixture.encode(wrong), handle: fixture.owner) }
        // Legal full desks can exceed the old 4 MiB JSON framing allowance
        // after escaped paths and full notes are encoded. Non-state receipts
        // still use the smaller bound, and origin ordinals may repeat after restore.
        var large = try JSONSerialization.jsonObject(with: Data(fixture.snapshot().utf8)) as! [String: Any]
        let originalObservation = (large["observations"] as! [[String: Any]])[0]
        let originalPassage = (large["observed_passages"] as! [[String: Any]])[0]
        large["observations"] = (1...32).map { id -> [String: Any] in
            var row = originalObservation; row["observation"] = String(id)
            row["path_hex"] = String(repeating: "ff", count: 16_384)
            row["path"] = String(repeating: "\\xff", count: 16_384)
            return row
        }
        large["observed_passages"] = (1...32).map { id -> [String: Any] in
            var row = originalPassage; row["passage"] = String(id); row["observation"] = String(id)
            row["body"] = String(repeating: "\0", count: 16_384)
            return row
        }
        large["retained_sources"] = "32"
        large["retained_source_bytes"] = String(state.retainedBytes * 32)
        let largeJSON = try ObservedReaderFixture.encode(large)
        precondition(largeJSON.utf8.count > AtlasDeskWire.maximumNonStateResponseBytes
            && largeJSON.utf8.count < AtlasDeskWire.maximumResponseBytes)
        let fullDesk = try AtlasDeskState.decode(largeJSON, handle: fixture.owner)
        precondition(fullDesk.annotationCount == 32 && fullDesk.observations.count == 32)
        large["command"] = "observed-passage"
        rejects { _ = try AtlasDeskWire.admit(ObservedReaderFixture.encode(large), handle: fixture.owner, command: "observed-passage") }

        // A saved passage remains readable after the originating reader closes.
        source.close()
        await wait("source retired") { probe.closedCount == probe.createCount }
        let reread = try await desk.readObservedPassage(target)
        precondition(reread.text == page.text && reread.originalHex == page.originalHex)
        let draft = AtlasDeskObservedNoteDraft(target: target)
        desk.registerNoteDraft(draft.id) { [weak draft] in draft?.closeProtectionRevision }
        draft.label = "Investigate original bytes"; draft.body = "First line\r\nNUL:\0 emoji:😀 e\u{301}"
        precondition(!desk.closeSnapshot.drafts.isEmpty)
        fixture.loseReply()
        let savedFirst = await draft.save(in: desk)
        precondition(savedFirst, "A lost edit reply resolves from accepted state without replay")
        precondition(fixture.mutationCount == 2 && draft.pending == nil && !draft.hasUnsavedChanges)
        precondition(draft.target.passage.updateSequence == 2 && draft.target.passage.body.utf8.elementsEqual(draft.body.utf8))
        precondition(!target.isCurrent(in: desk.state), "Old note versions cannot authorize edits, copies or forgetting")
        desk.forgetObservedPassage(target)
        precondition(fixture.mutationCount == 2)

        // An ambiguous edit preserves draft text until state can be read again.
        draft.body += "\nNew evidence"
        fixture.loseReply(stateFailures: 1)
        let savedUnconfirmed = await draft.save(in: desk)
        precondition(!savedUnconfirmed)
        precondition(draft.pending != nil && draft.hasUnsavedChanges && desk.hasUnverifiedChanges)
        let edits = fixture.mutationCount
        let resolved = await draft.checkSavedState(in: desk)
        precondition(resolved)
        precondition(fixture.mutationCount == edits && draft.pending == nil)
        desk.unregisterNoteDraft(draft.id)

        desk.save(path: "/chosen/observed.fcbdesk", accessLease: nil)
        await wait("checkpoint saved") { !desk.busy }
        precondition(desk.lastSave?.succeeded == true && !desk.hasUnsavedContents)
        let savedTarget = draft.target
        fixture.loseReply()
        desk.forgetObservedPassage(savedTarget)
        await wait("forgot passage") { !desk.busy }
        precondition(desk.state?.observedPassages.isEmpty == true && desk.state?.retainedSources == 0)
        precondition(desk.hasUnsavedContents)
        desk.restore(path: "/chosen/observed.fcbdesk", accessLease: nil)
        await wait("restored observed checkpoint") { !desk.busy }
        precondition(!desk.hasUnsavedContents && desk.hasCheckpointContents && desk.state?.panes.isEmpty == true)
        let restored = AtlasDeskObservedTarget(state: desk.state!, passage: desk.state!.observedPassages[0])!
        precondition(restored.id != savedTarget.id && !savedTarget.isCurrent(in: desk.state))
        let restoredPage = try await desk.readObservedPassage(restored)
        precondition(restoredPage.text == page.text && restored.passage.body == draft.body)
        desk.close()

        // Closing the source while its transfer is in flight cannot retire its
        // handle before return. Desk reconciles the possibly committed import.
        let racingProbe = ObservedReaderProbe(), racingFixture = ObservedDeskFixture()
        var racingTransport = racingProbe.transport; racingTransport.domain = racingFixture.domain
        let racingSource = AtlasObservedReaderModel(transport: racingTransport); racingSource.pageBytes = 4
        racingSource.open(.init(input: input, project: UUID(), access: nil))
        await wait("racing open") { racingSource.page != nil && !racingSource.busy }
        racingSource.updateTextSelection(source: racingSource.source!, navigation: racingSource.navigation,
            range: NSRange(location: 0, length: 3))
        await wait("racing selection") { racingSource.selectionForRetention != nil }
        racingFixture.configure(racingSource.selectionForRetention!)
        let racingDesk = AtlasReadingDeskModel(transport: racingFixture.transport), gate = ObservedReaderGate()
        racingFixture.blockNextKeep(gate)
        precondition(racingDesk.keepObserved(reader: racingSource))
        await wait("entered import") { gate.entered }
        racingSource.close()
        precondition(racingProbe.closedCount == 0, "The observation owner survives an entered foreign call")
        gate.release()
        await wait("closed-source import reconciled") { !racingDesk.busy && racingDesk.state?.observedPassages.count == 1 }
        precondition(racingFixture.mutationCount == 1)
        await wait("racing source retired") { racingProbe.closedCount == 1 }
        racingDesk.close()
        print("Observed passage native model, provenance, note and lifecycle regressions passed")
    }
}
