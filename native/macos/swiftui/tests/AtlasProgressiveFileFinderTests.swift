import Foundation
import Dispatch

/// Only foreign calls are injected. The production bounded wire decoder,
/// latest-input scheduling, recovery and retained-reader handoff are exercised.
private final class ProgressiveFinderFixture: @unchecked Sendable {
    struct Session {
        let generation: UInt64
        let query: AtlasFileQuery
        var revision: UInt64 = 1
        var replay: UInt64?
        var paused = false
    }
    let domain = UUID(), entered = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
    let firstMatchStep: UInt64
    let completeStep: UInt64
    let source = Array("captured\n".utf8)
    let digest = "af9f5d825b4a9e493a23ea438db355287c4320107eb98c3542879fef26b82628"
    private let lock = NSLock()
    private var sessions: [UInt64: Session] = [:], readers: [UInt64: [UInt8]] = [:]
    private var nextOwner: UInt64 = 40, nextReader: UInt64 = 100
    private var fault: String?, blocked = false, evicts = false
    private var log: [String] = []
    init(firstMatchStep: UInt64 = 1, completeStep: UInt64 = 8) {
        self.firstMatchStep = firstMatchStep; self.completeStep = completeStep
    }
    private func locked<T>(_ work: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return work() }
    var calls: [String] { locked { log } }
    func corrupt(_ value: String) { locked { fault = value } }
    func blockNext() { locked { blocked = true } }
    func evictSelected() { locked { evicts = true } }
    func json(_ value: [String: Any]) -> String {
        String(decoding: try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self)
    }
    func hex(_ bytes: some Sequence<UInt8>) -> String { bytes.map { String(format: "%02x", $0) }.joined() }
    func path(_ bytes: [UInt8]) -> [String: Any] {
        ["encoding": "unix-bytes", "hex": hex(bytes), "display": String(bytes: bytes, encoding: .utf8) ?? "\\xff.rs"]
    }
    func raw(_ id: UInt64) -> [UInt8] {
        id == 3 || id == 20_001 ? [255, 46, 114, 115] : Array("src/file-\(id).rs".utf8)
    }
    func fileID(_ session: Session) -> UInt64 { (firstMatchStep >= 6 ? 20_001 : 3) + (evicts ? 1 : 0) }
    func row(_ id: UInt64) -> [String: Any] {
        ["file_id": String(id), "path": path(raw(id)), "match_kind": "filename-subsequence"]
    }
    func header(_ session: Session, owner: UInt64 = 40, command: String = "page") -> [String: Any] {
        let processed = session.revision - 1, step = processed / 16
        let searched = processed * 256, catalogued = searched + (processed == 0 ? 256 : 0)
        let terminal = step >= completeStep, found = step >= firstMatchStep
        let ids: [UInt64] = found ? (evicts ? Array(fileID(session)...(fileID(session) + 255)) : [fileID(session)]) : []
        let hits = ids.map(row)
        let matches = found ? UInt64(evicts ? 257 : 1) : 0
        return ["schema": "fcb.file-finder-session/1", "status": "ok", "command": command, "owner": String(owner),
            "query_generation": String(session.generation), "revision": String(session.revision),
            "query_hex": hex(session.query.text.utf8), "mode": session.query.mode.wire,
            "case": session.query.matchCase ? "sensitive" : "unicode-lowercase", "key_version": "1",
            "identity_scope": "retained-query-session", "ordering": "ranked-observed-paths",
            "policy": "product-defaults/no-rule-files-v1", "source_payload_read": command == "open-reader",
            "discovery_source_bytes_read": "0", "discovery_source_read_calls": "0",
            "state": terminal ? "complete" : (session.paused ? "paused" : "searching"),
            "more": !terminal, "paused": !terminal && session.paused,
            "discovery_complete": terminal, "search_complete": terminal, "pending_batch": processed == 0,
            "truncated": matches > UInt64(hits.count), "catalogued_files": String(catalogued),
            "searched_files": String(searched), "matches_seen": String(matches),
            "files_examined": String(searched), "candidates_examined": String(searched),
            "work_units": String(searched * 10), "retained_hits": String(hits.count),
            "retained_path_bytes": String(ids.reduce(0) { $0 + raw($1).count }),
            "processed_batches": String(processed),
            "replay_after_revision": session.replay.map { String($0) as Any } ?? NSNull(),
            "limits": ["max_results": "256", "max_path_bytes": "2048", "max_retained_path_bytes": "524288",
                "max_batches_per_advance": "16", "response_bytes": "4194304"],
            "discovery": ["files": String(catalogued), "directories": String(processed + 1),
                "excluded_entries": "0", "symlinks_not_followed": "0", "special_objects": "0", "unavailable": "0",
                "cycles": "0", "path_limited": "0", "depth_limited": "0", "queue_refused": "0"],
            "hits": hits]
    }
    private func readerHeader(_ owner: UInt64, raw: [UInt8], command: String) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": command, "owner": String(owner),
            "file_id": "1", "source_revision": "1", "captured_bytes": String(source.count), "encoding": "utf8",
            "path": path(raw), "capture_origin": "host-supplied", "capture_sha256": digest,
            "additional_source_bytes_read": "0", "native_presented": false]
    }
    private func readerInfo(_ owner: UInt64, raw: [UInt8]) -> [String: Any] {
        var out = readerHeader(owner, raw: raw, command: "info")
        out["initial_source_bytes_read"] = "0"; out["initial_read_calls"] = "0"
        out["accepted_query_generation"] = NSNull(); out["accepted_outline_generation"] = NSNull()
        out["accepted_document_generation"] = NSNull()
        return out
    }
    private func window(_ owner: UInt64, raw: [UInt8], start: UInt64, limit: UInt64) -> [String: Any] {
        let end = min(UInt64(source.count), start + limit), bytes = source[Int(start)..<Int(end)]
        var out = readerHeader(owner, raw: raw, command: "window")
        out["requested_original_range"] = ["start": String(start), "end": String(end)]
        out["visible_range"] = ["start": String(start), "end": String(end)]
        out["text_kind"] = "logical-captured-text-not-shaped"; out["text"] = String(decoding: bytes, as: UTF8.self)
        out["original_hex"] = hex(bytes); out["range_limited"] = false; out["boundaries_adjusted"] = false
        out["has_replacements"] = false; out["first_physical_line"] = NSNull(); out["selection"] = NSNull()
        out["next_offset"] = end < UInt64(source.count) ? String(end) as Any : NSNull()
        return out
    }
    var transport: AtlasProgressiveFileTransport {
        AtlasProgressiveFileTransport(create: { [self] in locked {
            let id = nextOwner; nextOwner += 1; log.append("create:\(id)"); return id
        } }, run: { [self] owner, request in
            precondition(!Thread.isMainThread)
            switch request {
            case .advance, .openReader:
                if locked({ let value = blocked; blocked = false; return value }) {
                    entered.signal(); precondition(resume.wait(timeout: .now() + .seconds(5)) == .success)
                }
            default: break
            }
            return locked {
                switch request {
                case .open(_, let generation, let query):
                    let session = Session(generation: generation, query: query); sessions[owner] = session
                    log.append("open:\(owner):\(query.text):\(query.mode.wire):\(query.matchCase)")
                    if fault == "open-lost" { fault = nil; return nil }
                    if fault == "open-recovery-lost" { fault = "info-lost"; return nil }
                    return json(header(session, owner: owner))
                case .info:
                    guard let session = sessions[owner] else { return nil }
                    log.append("info:\(owner)")
                    if fault == "info-lost" { fault = nil; return nil }
                    var out = header(session, owner: owner, command: "info")
                    if fault == "info-query" { fault = nil; out["query_hex"] = hex("other".utf8) }
                    if fault == "info-regress" {
                        fault = nil
                        var old = session; old.revision = 1; old.replay = nil
                        out = header(old, owner: owner, command: "info")
                    }
                    return json(out)
                case .page(let generation, let revision):
                    guard let session = sessions[owner], session.generation == generation, session.revision == revision else { return nil }
                    log.append("page:\(owner):\(revision)")
                    return json(header(session, owner: owner))
                case .advance(let generation, let after, let batches):
                    guard var session = sessions[owner], session.generation == generation, batches == 16 else { return nil }
                    if session.replay == after, after < session.revision {
                        log.append("replay:\(owner):\(after)"); return json(header(session, owner: owner))
                    }
                    guard after == session.revision else { return nil }
                    session.revision += 16; session.replay = after; session.paused = false; sessions[owner] = session
                    log.append("advance:\(owner):\(after)")
                    if fault == "advance-lost" { fault = nil; return nil }
                    return json(header(session, owner: owner))
                case .stop(let generation):
                    guard var session = sessions[owner], session.generation == generation else { return nil }
                    session.paused = true; sessions[owner] = session; log.append("stop:\(owner)")
                    return json(header(session, owner: owner, command: "stop"))
                case .openReader(let reader, let generation, let revision, let file, let bytes):
                    guard let session = sessions[owner], session.generation == generation, session.revision == revision,
                          file == fileID(session), bytes == AtlasReaderLimits.captureBytes else { return nil }
                    log.append("capture:\(owner):\(revision):\(file)")
                    let raw = raw(file); readers[reader] = raw
                    var out = header(session, owner: owner, command: "open-reader")
                    out.removeValue(forKey: "hits")
                    out["selection"] = row(file); out["reader_owner"] = String(reader)
                    out["source_observation"] = "new-capture-after-metadata-selection"
                    out["source_bytes_read"] = String(source.count); out["source_read_calls"] = "1"
                    var nested = readerInfo(reader, raw: raw)
                    if let fault {
                        self.fault = nil
                        switch fault {
                        case "lost": return nil
                        case "revision": out["revision"] = String(revision + 1)
                        case "generation": out["query_generation"] = String(generation + 1)
                        case "query": out["query_hex"] = hex("other".utf8)
                        case "mode": out["mode"] = "exact"
                        case "case": out["case"] = "sensitive"
                        case "selection": out["selection"] = row(file + 1)
                        case "source-read": out["source_payload_read"] = false
                        case "observation": out["source_observation"] = "metadata-snapshot"
                        case "reader-path": nested["path"] = path(Array("other.rs".utf8))
                        case "reader-generation": nested["accepted_query_generation"] = "1"
                        default: preconditionFailure("unknown finder capture fault")
                        }
                    }
                    out["reader"] = nested; out["window"] = window(reader, raw: raw, start: 0, limit: 16_384)
                    return json(out)
                }
            }
        }, cancel: { [self] owner in locked { log.append("cancel:\(owner)") }; return true },
        close: { [self] owner in locked { sessions.removeValue(forKey: owner); log.append("close:\(owner)") }; return true },
        retirementFailed: { fatalError("finder retirement failed") }, readerDomain: domain)
    }
    var readerTransport: AtlasReaderTransport {
        AtlasReaderTransport(create: { [self] in locked {
            let id = nextReader; nextReader += 1; log.append("reader-create:\(id)"); return id
        } }, open: { [self] _, _, _ in locked { log.append("LABEL-OPEN") }; return nil },
        window: { [self] owner, start, limit in locked {
            guard let raw = readers[owner] else { return nil }
            return json(window(owner, raw: raw, start: start, limit: limit))
        } }, lines: { _, _, _, _ in nil }, cancel: { _ in true },
        close: { [self] owner in locked { readers.removeValue(forKey: owner); log.append("reader-close:\(owner)") }; return true },
        retirementFailed: { fatalError("reader retirement failed") },
        comparison: .init(domain: domain, compare: { _, _, _, _, _ in nil }))
    }
}

@main private enum AtlasProgressiveFileFinderTests {
    @MainActor private static func wait(_ message: String, _ ready: () -> Bool) async {
        for _ in 0..<3000 { if ready() { return }; try? await Task.sleep(nanoseconds: 1_000_000) }
        preconditionFailure(message)
    }
    private static func reject(_ message: String, _ operation: () throws -> Void) {
        do { try operation(); preconditionFailure(message) } catch { }
    }
    @MainActor private static func started(_ fixture: ProgressiveFinderFixture) async -> AtlasProgressiveQuickOpenModel {
        let model = AtlasProgressiveQuickOpenModel(transport: fixture.transport)
        model.bind(root: "/repo", accessLease: nil); model.text = "rs"; model.submit()
        await wait("finder did not produce its first usable results") { !model.busy }
        precondition(model.isCurrent && model.canOpen)
        return model
    }
    @MainActor static func main() async throws {
        try boundedWireAndCoverage()
        try await autoProgressStopRecoveryAndSelection()
        await editsCoalesceAndRevokeOldRows()
        await exactRawCaptureAndForgedReceipts()
        await closeDrainsGrantAndCapture()
        print("Progressive Quick Open wire, late results, selection, recovery, raw-reader and grant-lifetime regressions passed.")
    }
    private static func boundedWireAndCoverage() throws {
        let fixture = ProgressiveFinderFixture()
        let query = try AtlasFileQuery(text: "rs")
        let session = ProgressiveFinderFixture.Session(generation: 1, query: query)
        var initial = fixture.header(session)
        // Discovery can finish before the last batch has been ranked.
        initial["discovery_complete"] = true
        let pending = try AtlasProgressiveFileReport.decode(fixture.json(initial), owner: 40,
            generation: 1, query: query, command: "page", revision: 1)
        precondition(pending.pending && pending.more && pending.discoveryComplete && !pending.complete && pending.rows.isEmpty)
        for key in ["owner", "query_generation", "query_hex", "mode", "case", "revision",
                    "processed_batches", "source_payload_read", "searched_files", "retained_hits", "hits"] {
            var bad = initial
            switch key {
            case "owner": bad[key] = "41"
            case "query_generation": bad[key] = "2"
            case "query_hex": bad[key] = fixture.hex("other".utf8)
            case "mode": bad[key] = "exact"
            case "case": bad[key] = "sensitive"
            case "revision": bad[key] = "0"
            case "processed_batches": bad[key] = "1"
            case "source_payload_read": bad[key] = true
            case "searched_files": bad[key] = "257"
            case "retained_hits": bad[key] = "1"
            default: bad[key] = Array(repeating: fixture.row(3), count: 257)
            }
            reject("forged finder snapshot admitted: \(key)") {
                _ = try AtlasProgressiveFileReport.decode(fixture.json(bad), owner: 40, generation: 1, query: query, command: "page")
            }
        }
        var terminal = session; terminal.revision = 129; terminal.replay = 113
        var complete = fixture.header(terminal)
        var discovery = complete["discovery"] as! [String: String]; discovery["unavailable"] = "1"
        complete["discovery"] = discovery
        reject("complete search hid omitted paths") {
            _ = try AtlasProgressiveFileReport.decode(fixture.json(complete), owner: 40, generation: 1, query: query, command: "page")
        }
        complete["state"] = "partial"; complete["search_complete"] = false; complete["discovery_complete"] = false
        let partial = try AtlasProgressiveFileReport.decode(fixture.json(complete), owner: 40, generation: 1, query: query, command: "page")
        precondition(!partial.more && !partial.complete && partial.omissionCount == 1)
        let failure = fixture.json(["schema": "fcb.file-finder-session/1", "status": "error", "owner": "40",
            "error": ["code": "FILE_FINDER_HANDLE_STALE"]])
        do {
            _ = try AtlasProgressiveFileWire.admit(failure, owner: 40, command: "page")
            preconditionFailure("engine error accepted as a success")
        } catch AtlasProgressiveFileError.engine(let code) { precondition(code == "FILE_FINDER_HANDLE_STALE") }
    }
    @MainActor private static func autoProgressStopRecoveryAndSelection() async throws {
        let late = ProgressiveFinderFixture(firstMatchStep: 6)
        let model = await started(late)
        precondition(model.report!.searched > 20_000 && model.report?.rows.first?.id == 20_001)
        precondition(late.calls.filter { $0.hasPrefix("advance:") }.count == 6)
        let selected = model.selected!
        late.corrupt("advance-lost"); model.continueSearch()
        await wait("lost advanced result did not recover") { !model.busy }
        precondition(model.report?.revision == 113 && model.selected == selected)
        precondition(late.calls.filter { $0 == "advance:40:97" }.count == 1 && late.calls.contains("page:40:113"))
        model.close()

        let stopped = ProgressiveFinderFixture(firstMatchStep: 3)
        let paused = AtlasProgressiveQuickOpenModel(transport: stopped.transport)
        paused.bind(root: "/repo", accessLease: nil); stopped.blockNext(); paused.text = "rs"; paused.submit()
        await wait("automatic advance did not enter") { stopped.entered.wait(timeout: .now()) == .success }
        paused.stop(); stopped.resume.signal()
        await wait("stopped entered work did not drain") { !paused.busy }
        precondition(paused.report?.paused == true && paused.report?.revision == 17 && paused.canContinue)
        paused.continueSearch()
        await wait("resumed empty search did not reach useful matches") { !paused.busy }
        precondition(paused.report?.revision == 49 && paused.canOpen)
        paused.close()

        let evicting = ProgressiveFinderFixture(), selectedModel = await started(evicting)
        let old = selectedModel.selected!
        evicting.evictSelected(); selectedModel.continueSearch()
        await wait("new ranking did not arrive") { !selectedModel.busy }
        precondition(selectedModel.report?.rows.first?.id != old.file && selectedModel.selected == nil && !selectedModel.canOpen)
        precondition(selectedModel.notice.contains("Choose a result"))
        selectedModel.close()

        let stale = ProgressiveFinderFixture(), staleModel = await started(stale)
        stale.corrupt("info-regress"); staleModel.reload()
        await wait("stale progress recovery did not finish") { !staleModel.busy }
        precondition(staleModel.report == nil && !staleModel.canOpen && !stale.calls.contains("page:40:1"))
        staleModel.close()

        let failed = ProgressiveFinderFixture(), recovered = AtlasProgressiveQuickOpenModel(transport: failed.transport)
        recovered.bind(root: "/repo", accessLease: nil); failed.corrupt("open-recovery-lost"); recovered.text = "rs"; recovered.submit()
        await wait("failed initial reply did not drain") { !recovered.busy }
        precondition(recovered.report == nil && recovered.canReload)
        recovered.reload(); await wait("manual initial recovery failed") { !recovered.busy }
        precondition(recovered.report?.revision == 1 && recovered.canContinue)
        precondition(failed.calls.filter { $0.hasPrefix("open:") }.count == 1 && !failed.calls.contains(where: { $0.hasPrefix("advance:") }))
        failed.corrupt("info-query"); recovered.reload()
        await wait("wrong-query recovery did not drain") { !recovered.busy }
        precondition(recovered.report == nil && !recovered.canOpen)
        recovered.close()
    }
    @MainActor private static func editsCoalesceAndRevokeOldRows() async {
        let fixture = ProgressiveFinderFixture(), model = await started(fixture)
        let old = model.selected!
        fixture.blockNext(); model.continueSearch()
        await wait("old search did not enter") { fixture.entered.wait(timeout: .now()) == .success }
        model.text = "old"; model.mode = .exact; model.matchCase = true; model.text = "new"
        precondition(model.report == nil && model.selected == nil && !model.canOpen)
        fixture.resume.signal()
        await wait("latest replacement did not finish") { !model.busy }
        precondition(model.report?.query.text == "new" && model.report?.query.mode == .exact && model.report?.query.matchCase == true)
        precondition(fixture.calls.filter { $0.hasPrefix("open:") }.count == 2)
        precondition(model.selected?.owner != old.owner && model.selected?.generation == old.generation)
        model.selected = old
        model.openSelected(readerTransport: fixture.readerTransport) { _ in preconditionFailure("stale owner opened a reader") }
        precondition(!model.canOpen && !fixture.calls.contains(where: { $0.hasPrefix("reader-create:") }))
        model.text = ""
        precondition(model.report == nil && !model.canOpen)
        model.close()
    }
    @MainActor private static func exactRawCaptureAndForgedReceipts() async {
        let fixture = ProgressiveFinderFixture(), model = await started(fixture)
        var capture: AtlasProgressiveFileCapture?
        model.openSelected(readerTransport: fixture.readerTransport) { capture = $0 }
        await wait("raw path capture did not finish") { !model.busy }
        precondition(capture != nil && capture?.file.path == nil && capture?.reader.page?.identity.pathBytes == [255, 46, 114, 115])
        model.close()
        precondition(capture?.reader.canPin == true && capture?.reader.canRefresh == false)
        capture?.reader.offsetInput = "4"; capture?.reader.goToByte()
        await wait("reader could not navigate after finder closure") { capture?.reader.busy == false }
        precondition(capture?.reader.page?.text == "ured\n" && !fixture.calls.contains("LABEL-OPEN"))
        capture?.reader.close()
        for fault in ["revision", "generation", "query", "mode", "case", "selection", "source-read",
                      "observation", "reader-path", "reader-generation", "lost"] {
            let fixture = ProgressiveFinderFixture(), model = await started(fixture)
            fixture.corrupt(fault)
            model.openSelected(readerTransport: fixture.readerTransport) { _ in preconditionFailure("forged reader receipt admitted: \(fault)") }
            await wait("forged reader did not retire") { !model.busy && fixture.calls.contains("reader-close:100") }
            precondition(!fixture.calls.contains("LABEL-OPEN"))
            model.close()
        }
    }
    @MainActor private static func closeDrainsGrantAndCapture() async {
        final class Grant { }
        let fixture = ProgressiveFinderFixture(), model = AtlasProgressiveQuickOpenModel(transport: fixture.transport)
        var grant: Grant? = Grant(); weak var observed = grant
        model.bind(root: "/repo", accessLease: grant); grant = nil
        model.text = "rs"; model.submit()
        await wait("grant-backed search did not finish") { !model.busy }
        fixture.blockNext()
        model.openSelected(readerTransport: fixture.readerTransport) { _ in preconditionFailure("closed finder published a late reader") }
        await wait("reader handoff did not enter") { fixture.entered.wait(timeout: .now()) == .success }
        model.close()
        precondition(observed != nil && !fixture.calls.contains("close:40"))
        fixture.resume.signal()
        await wait("closed finder/reader did not retire") {
            fixture.calls.contains("close:40") && fixture.calls.contains("reader-close:100") && observed == nil
        }
        precondition(model.closed && model.report == nil && !fixture.calls.contains("LABEL-OPEN"))
    }
}
