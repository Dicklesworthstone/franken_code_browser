import Foundation
import Dispatch

/// Only the foreign boundary is injected. Production page admission, current
/// row authority, recovery and retained-reader scheduling remain in use.
private final class ProjectBrowserFixture: @unchecked Sendable {
    struct Session { var latest: UInt64 = 1; var paused = false }
    let domain = UUID(), entered = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
    let source = Array("captured\n".utf8)
    let digest = "af9f5d825b4a9e493a23ea438db355287c4320107eb98c3542879fef26b82628"
    let emptyFirst: Bool
    private let lock = NSLock()
    private var sessions: [UInt64: Session] = [:], readers: [UInt64: [UInt8]] = [:]
    private var nextOwner: UInt64 = 40, nextReader: UInt64 = 100
    private var fault: String?, blocked = false
    private var log: [String] = []
    init(emptyFirst: Bool = false) { self.emptyFirst = emptyFirst }
    private func locked<T>(_ work: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return work() }
    var calls: [String] { locked { log } }
    func corrupt(_ value: String) { locked { fault = value } }
    func blockNext() { locked { blocked = true } }
    func json(_ value: [String: Any]) -> String {
        String(decoding: try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self)
    }
    func hex(_ bytes: some Sequence<UInt8>) -> String { bytes.map { String(format: "%02x", $0) }.joined() }
    func path(_ bytes: [UInt8]) -> [String: Any] {
        ["encoding": "unix-bytes", "hex": hex(bytes), "display": String(bytes: bytes, encoding: .utf8) ?? "\\xff.rs"]
    }
    func raw(_ page: UInt64) -> [UInt8] { page == 2 ? [255, 46, 114, 115] : Array("src/p\(page).rs".utf8) }
    func row(_ page: UInt64) -> [String: Any] {
        ["file_id": String(page - (emptyFirst ? 1 : 0)), "path": path(raw(page)), "observed_bytes": "4096"]
    }
    func header(_ owner: UInt64 = 40, command: String = "info", latest: UInt64 = 1, paused: Bool = false) -> [String: Any] {
        let first = latest > 4 ? latest - 3 : 1
        let pages = Array(first...latest), files = pages.filter { !(emptyFirst && $0 == 1) }
        let count = latest - (emptyFirst ? 1 : 0)
        return ["schema": "fcb.project-catalog-session/1", "status": "ok", "command": command, "owner": String(owner),
            "identity_scope": "retained-session", "ordering": "provisional-discovery-order",
            "policy": "product-defaults/no-rule-files-v1", "native_presented": false,
            "discovery_source_bytes_read": "0", "discovery_source_read_calls": "0",
            "state": paused ? "paused" : "discovering", "more": true, "paused": paused, "discovery_complete": false,
            "latest_page": String(latest), "first_retained_page": String(first), "retained_pages": String(pages.count),
            "retained_files": String(files.count), "retained_path_bytes": String(files.reduce(0) { $0 + raw($1).count }),
            "catalogued_files": String(count),
            "limits": ["page_entries": "256", "page_path_bytes": "67584", "max_path_bytes": "2048",
                "retained_pages": "4", "retained_files": "1024", "retained_path_bytes": "270336"],
            "discovery": ["files": String(count), "directories": String(latest), "excluded_entries": "0",
                "symlinks_not_followed": "0", "special_objects": "0", "unavailable": "0", "cycles": "0",
                "path_limited": "0", "depth_limited": "0", "queue_refused": "0"]]
    }
    func page(_ id: UInt64 = 1, owner: UInt64 = 40, latest: UInt64 = 1, paused: Bool = false) -> [String: Any] {
        var out = header(owner, command: "page", latest: latest, paused: paused)
        out["page_id"] = String(id); out["after_page"] = String(id - 1)
        out["files"] = emptyFirst && id == 1 ? [] : [row(id)]
        return out
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
    var transport: AtlasProjectBrowserTransport {
        AtlasProjectBrowserTransport(create: { [self] in locked { let id = nextOwner; nextOwner += 1; return id } },
            run: { [self] owner, request in
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
                    case .open:
                        sessions[owner] = Session(); log.append("open:\(owner)")
                        if fault == "open-lost" { fault = nil; return nil }
                        if fault == "open-recovery-lost" { fault = "info-lost"; return nil }
                        return json(page(owner: owner))
                    case .info:
                        guard let session = sessions[owner] else { return nil }
                        log.append("info:\(owner)")
                        if fault == "info-lost" { fault = nil; return nil }
                        return json(header(owner, latest: session.latest, paused: session.paused))
                    case .page(let id):
                        guard let session = sessions[owner], id > 0, id <= session.latest, session.latest - id < 4 else { return nil }
                        log.append("page:\(owner):\(id)")
                        return json(page(id, owner: owner, latest: session.latest, paused: session.paused))
                    case .advance(let after):
                        guard var session = sessions[owner], after == session.latest else { return nil }
                        session.latest += 1; session.paused = false; sessions[owner] = session
                        log.append("advance:\(owner):\(after)")
                        if fault == "advance-lost" { fault = nil; return nil }
                        return json(page(session.latest, owner: owner, latest: session.latest))
                    case .stop:
                        guard var session = sessions[owner] else { return nil }
                        session.paused = true; sessions[owner] = session; log.append("stop:\(owner)")
                        return json(header(owner, command: "stop", latest: session.latest, paused: true))
                    case .openReader(let reader, let pageID, let file, let bytes):
                        guard let session = sessions[owner], pageID <= session.latest, session.latest - pageID < 4,
                              file == pageID - (emptyFirst ? 1 : 0), bytes == AtlasReaderLimits.captureBytes else { return nil }
                        log.append("capture:\(owner):\(pageID):\(file)")
                        let raw = raw(pageID); readers[reader] = raw
                        var out = header(owner, command: "open-reader", latest: session.latest, paused: session.paused)
                        out["page_id"] = String(pageID); out["file_id"] = String(file); out["path"] = path(raw)
                        out["reader_owner"] = String(reader); out["source_observation"] = "new-capture-after-metadata-selection"
                        out["source_bytes_read"] = String(source.count); out["source_read_calls"] = "1"
                        var nested = readerInfo(reader, raw: raw)
                        if let fault {
                            self.fault = nil
                            switch fault {
                            case "lost": return nil
                            case "page": out["page_id"] = String(pageID + 1)
                            case "file": out["file_id"] = String(file + 1)
                            case "path": out["path"] = path(Array("other.rs".utf8))
                            case "observation": out["source_observation"] = "metadata-snapshot"
                            case "reader-path": nested["path"] = path(Array("other.rs".utf8))
                            case "reader-generation": nested["accepted_query_generation"] = "1"
                            default: preconditionFailure("unknown capture fault")
                            }
                        }
                        out["reader"] = nested; out["window"] = window(reader, raw: raw, start: 0, limit: 16_384)
                        return json(out)
                    }
                }
            }, cancel: { [self] owner in locked { log.append("cancel:\(owner)") }; return true },
            close: { [self] owner in locked { sessions.removeValue(forKey: owner); log.append("close:\(owner)") }; return true },
            retirementFailed: { fatalError("catalog retirement failed") }, readerDomain: domain)
    }
    var readerTransport: AtlasReaderTransport {
        AtlasReaderTransport(create: { [self] in locked { let id = nextReader; nextReader += 1; log.append("reader-create:\(id)"); return id } },
            open: { [self] _, _, _ in locked { log.append("LABEL-OPEN") }; return nil },
            window: { [self] owner, start, limit in locked {
                guard let raw = readers[owner] else { return nil }
                return json(window(owner, raw: raw, start: start, limit: limit))
            } }, lines: { _, _, _, _ in nil }, cancel: { _ in true },
            close: { [self] owner in locked { readers.removeValue(forKey: owner); log.append("reader-close:\(owner)") }; return true },
            retirementFailed: { fatalError("reader retirement failed") }, comparison: .init(domain: domain, compare: { _, _, _, _, _ in nil }))
    }
}

@main private enum AtlasProjectBrowserTests {
    @MainActor private static func wait(_ message: String, _ ready: () -> Bool) async {
        for _ in 0..<2000 { if ready() { return }; try? await Task.sleep(nanoseconds: 1_000_000) }
        preconditionFailure(message)
    }
    private static func reject(_ message: String, _ operation: () throws -> Void) {
        do { try operation(); preconditionFailure(message) } catch { }
    }
    @MainActor private static func refuse(_ message: String, _ operation: @MainActor () async throws -> Void) async {
        do { try await operation(); preconditionFailure(message) } catch { }
    }
    @MainActor static func main() async throws {
        try boundedWireAndCoverage()
        try await emptyPagesLostRepliesPauseAndEviction()
        try await exactRawFileCaptureAndForgedReceipts()
        try await closeDrainsGrantAndLateCapture()
        print("Progressive project pages, beyond-20k admission, recovery, pause/eviction, raw-file capture and grant-lifetime regressions passed.")
    }
    private static func boundedWireAndCoverage() throws {
        let f = ProjectBrowserFixture()
        let first = try AtlasProjectBrowserPage.decode(f.json(f.page()), owner: 40, page: 1)
        precondition(first.files.count == 1 && first.info.more && !first.info.complete)
        var large = f.page(20_001, latest: 20_001)
        let admitted = try AtlasProjectBrowserPage.decode(f.json(large), owner: 40, page: 20_001)
        precondition(admitted.info.discovered == 20_001 && admitted.files[0].id == 20_001 && admitted.info.retainedPages == 4)
        large["state"] = "complete"; large["more"] = false; large["discovery_complete"] = true
        let finished = try AtlasProjectBrowserPage.decode(f.json(large), owner: 40)
        precondition(finished.info.complete && !finished.info.more)
        for field in ["owner", "identity_scope", "retained_pages", "first_retained_page", "more", "after_page",
                      "discovery_source_bytes_read", "catalogued_files", "files"] {
            var bad = f.page()
            switch field {
            case "owner": bad[field] = "41"
            case "identity_scope": bad[field] = "response-local"
            case "retained_pages": bad[field] = "5"
            case "first_retained_page": bad[field] = "0"
            case "more": bad[field] = false
            case "after_page": bad[field] = "1"
            case "discovery_source_bytes_read": bad[field] = "1"
            case "catalogued_files": bad[field] = "2"
            default: bad[field] = Array(repeating: f.row(1), count: 257)
            }
            reject("forged catalog page accepted: \(field)") { _ = try AtlasProjectBrowserPage.decode(f.json(bad), owner: 40, page: 1) }
        }
        var omitted = large, discovery = omitted["discovery"] as! [String: String]
        discovery["unavailable"] = "1"; omitted["discovery"] = discovery
        reject("complete discovery hid an unavailable entry") { _ = try AtlasProjectBrowserPage.decode(f.json(omitted), owner: 40) }
        omitted["state"] = "partial"; omitted["discovery_complete"] = false
        let partial = try AtlasProjectBrowserPage.decode(f.json(omitted), owner: 40)
        precondition(!partial.info.more && !partial.info.complete && partial.info.unavailable == 1)
        var unknownFile = f.page(), row = f.row(1)
        row["file_id"] = "2"; unknownFile["files"] = [row]
        reject("row ID exceeded the discovered identity domain") { _ = try AtlasProjectBrowserPage.decode(f.json(unknownFile), owner: 40) }
        var unordered = f.page(2, latest: 2)
        unordered["files"] = [f.row(2), f.row(1)]
        reject("non-increasing file IDs were accepted") { _ = try AtlasProjectBrowserPage.decode(f.json(unordered), owner: 40) }
    }
    @MainActor private static func emptyPagesLostRepliesPauseAndEviction() async throws {
        let failed = ProjectBrowserFixture(), recovered = AtlasProjectBrowserModel(transport: failed.transport)
        failed.corrupt("open-recovery-lost")
        await refuse("an unverifiable initial page was accepted") { try await recovered.open(root: "/repo", accessLease: nil) }
        precondition(recovered.info == nil && recovered.page == nil)
        recovered.reload(); await wait("manual initial-page recovery did not finish") { !recovered.busy }
        precondition(recovered.currentPage && recovered.canContinue && failed.calls.filter { $0 == "open:40" }.count == 1)
        recovered.close()
        let f = ProjectBrowserFixture(emptyFirst: true), model = AtlasProjectBrowserModel(transport: f.transport)
        f.corrupt("open-lost"); try await model.open(root: "/repo", accessLease: nil)
        precondition(model.page?.files.isEmpty == true && model.canContinue)
        precondition(f.calls.filter { $0 == "open:40" }.count == 1 && f.calls.contains("page:40:1"))
        f.corrupt("advance-lost"); model.advance()
        await wait("lost consumed page did not recover") { !model.busy }
        precondition(model.page?.id == 2 && model.info?.discovered == 1)
        precondition(f.calls.filter { $0 == "advance:40:1" }.count == 1 && f.calls.contains("page:40:2"))
        let old = model.page!
        f.blockNext(); model.advance()
        await wait("advance did not enter") { f.entered.wait(timeout: .now()) == .success }
        model.stop(); precondition(model.busy)
        f.resume.signal(); await wait("paused advance did not drain/reconcile") { !model.busy }
        precondition(model.info?.paused == true && model.page?.id == 3 && model.canContinue)
        for _ in 0..<3 { model.advance(); await wait("discovery did not continue") { !model.busy } }
        precondition(model.page?.id == 6 && model.info?.firstRetained == 3 && model.page?.files.count == 1)
        await refuse("an evicted row authorized a capture") { _ = try await model.openFile(old.files[0], from: old, readerTransport: f.readerTransport) }
        precondition(!f.calls.contains(where: { $0.hasPrefix("reader-create:") }))
        model.load(page: 3); await wait("retained page replay did not finish") { !model.busy }
        precondition(model.page?.id == 3 && model.info?.latest == 6)
        model.close()
    }
    @MainActor private static func exactRawFileCaptureAndForgedReceipts() async throws {
        let f = ProjectBrowserFixture(), model = AtlasProjectBrowserModel(transport: f.transport)
        try await model.open(root: "/repo", accessLease: nil); model.advance()
        await wait("raw filename page did not arrive") { !model.busy }
        let page = model.page!, file = page.files[0]
        precondition(String(bytes: file.rawPath, encoding: .utf8) == nil)
        let capture = try await model.openFile(file, from: page, readerTransport: f.readerTransport)
        precondition(capture.reader.page?.identity.pathBytes == file.rawPath && capture.reader.page?.text == "captured\n")
        precondition(capture.reader.page?.identity.capturedBytes == 9 && file.observedBytes == 4096)
        model.close(); precondition(capture.reader.canPin && !capture.reader.canRefresh)
        capture.reader.offsetInput = "4"; capture.reader.goToByte()
        await wait("captured reader stopped working after project close") { !capture.reader.busy }
        precondition(capture.reader.page?.text == "ured\n" && !f.calls.contains("LABEL-OPEN"))
        capture.reader.close()
        for fault in ["page", "file", "path", "observation", "reader-path", "reader-generation", "lost"] {
            let f = ProjectBrowserFixture(), model = AtlasProjectBrowserModel(transport: f.transport)
            try await model.open(root: "/repo", accessLease: nil)
            let page = model.page!; f.corrupt(fault)
            await refuse("corrupt file capture was published: \(fault)") {
                _ = try await model.openFile(page.files[0], from: page, readerTransport: f.readerTransport)
            }
            await wait("corrupt reader did not retire") { f.calls.contains("reader-close:100") }
            precondition(f.calls.filter { $0 == "capture:40:1:1" }.count == 1 && !f.calls.contains("LABEL-OPEN"))
            model.close()
        }
    }
    @MainActor private static func closeDrainsGrantAndLateCapture() async throws {
        final class Grant { }
        let f = ProjectBrowserFixture(), model = AtlasProjectBrowserModel(transport: f.transport)
        var grant: Grant? = Grant(); weak var observed = grant
        try await model.open(root: "/repo", accessLease: grant); grant = nil
        let page = model.page!; f.blockNext()
        let pending = Task { try await model.openFile(page.files[0], from: page, readerTransport: f.readerTransport) }
        await wait("capture did not enter") { f.entered.wait(timeout: .now()) == .success }
        model.close(); precondition(observed != nil && !f.calls.contains("close:40"))
        f.resume.signal()
        await refuse("closed project published a late reader") { _ = try await pending.value }
        await wait("closed project/capture failed to retire") { f.calls.contains("close:40") && f.calls.contains("reader-close:100") && observed == nil }
        precondition(model.info == nil && model.page == nil && !f.calls.contains("LABEL-OPEN"))
    }
}
