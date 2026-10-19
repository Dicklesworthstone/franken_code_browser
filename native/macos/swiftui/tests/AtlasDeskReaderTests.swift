import Foundation
import Dispatch

/// Only the foreign boundary is injected. The actual desk/reader coordinators,
/// receipt decoders, source navigation, cancellation and lifetime paths execute.
/// This suite does not substitute for native ABI or physical-Mac verification.
private final class DeskReaderFixture: @unchecked Sendable {
    private let lock = NSLock()
    let domain = UUID()
    let owner: UInt64 = 19
    let label = "../offline/README.md"
    let bytes: [UInt8]
    private var revision: UInt64 = 1
    private var file: UInt64 = 1
    private var offset: UInt64
    private var selection: (UInt64, UInt64)?
    private var next: UInt64 = 100
    private var readers: Set<UInt64> = []
    private var log: [String] = []
    private var corrupt: String?
    private var blocked = false
    let entered = DispatchSemaphore(value: 0)
    let resume = DispatchSemaphore(value: 0)

    init(bytes: [UInt8] = Array("# Saved\nneedle\r\nexact\0bytes\n".utf8), offset: UInt64 = 8) {
        self.bytes = bytes; self.offset = offset
        selection = (offset, min(offset + 6, UInt64(bytes.count)))
    }
    private func locked<T>(_ body: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return body() }
    var calls: [String] { locked { log } }
    func corruptNext(_ value: String) { locked { corrupt = value } }
    func blockNext() { locked { blocked = true } }
    func replacePane() { locked { file += 1; revision += 1; offset = 0; selection = nil } }
    private func encode(_ value: [String: Any]) -> String {
        String(decoding: try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self)
    }
    private func hex(_ source: some Sequence<UInt8>) -> String { source.map { String(format: "%02x", $0) }.joined() }
    private func header(_ command: String) -> [String: Any] {
        ["schema": "fcb.reading-desk/1", "status": "ok", "command": command, "owner": String(owner),
         "model_revision": String(revision), "last_attempt": String(revision),
         "additional_source_bytes_read": "0", "native_presented": false]
    }
    private func location() -> [String: Any] {
        ["file_id": String(file), "source_revision": String(file), "offset": String(offset),
         "selection": selection.map { ["start": String($0.0), "end": String($0.1)] } as Any? ?? NSNull()]
    }
    private func state() -> String {
        var out = header("state"), pane = location()
        pane["pane"] = "1"; pane["label"] = label; pane["pinned"] = false
        out["panes"] = [pane]; out["bookmarks"] = []; out["active_pane"] = "1"
        out["retained_sources"] = "1"; out["retained_source_bytes"] = String(bytes.count)
        out["can_go_back"] = false; out["can_go_forward"] = false
        out["persistence"] = "explicit-checkpoint"; out["autosave"] = false
        return encode(out)
    }
    private func readerHeader(_ reader: UInt64, _ command: String) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": command, "owner": String(reader),
         "file_id": "1", "source_revision": "1", "captured_bytes": String(bytes.count), "encoding": "utf8",
         "path": ["encoding": "unix-bytes", "hex": hex(label.utf8), "display": label],
         "capture_origin": "host-supplied", "capture_sha256": String(repeating: "a", count: 64),
         "additional_source_bytes_read": "0", "native_presented": false]
    }
    private func readerInfo(_ reader: UInt64) -> [String: Any] {
        var out = readerHeader(reader, "info")
        out["initial_source_bytes_read"] = "0"; out["initial_read_calls"] = "0"
        out["accepted_query_generation"] = NSNull(); out["accepted_outline_generation"] = NSNull()
        out["accepted_document_generation"] = NSNull()
        return out
    }
    private func page(_ reader: UInt64, offset: UInt64, limit: UInt64) -> String? {
        guard offset <= UInt64(bytes.count) else { return nil }
        let end = min(UInt64(bytes.count), offset + limit), raw = bytes[Int(offset)..<Int(min(UInt64(bytes.count), offset + limit))]
        var out = readerHeader(reader, "window")
        out["requested_original_range"] = ["start": String(offset), "end": String(end)]
        out["visible_range"] = ["start": String(offset), "end": String(end)]
        out["text_kind"] = "logical-captured-text-not-shaped"
        out["text"] = String(decoding: raw, as: UTF8.self); out["original_hex"] = hex(raw)
        out["range_limited"] = false; out["boundaries_adjusted"] = false
        out["has_replacements"] = String(bytes: raw, encoding: .utf8) == nil
        out["first_physical_line"] = NSNull(); out["selection"] = NSNull()
        out["next_offset"] = end < UInt64(bytes.count) ? String(end) as Any : NSNull()
        return encode(out)
    }
    var deskTransport: AtlasReadingDeskTransport {
        AtlasReadingDeskTransport(create: { [self] in owner }, run: { [self] handle, request in
            precondition(handle == owner && !Thread.isMainThread)
            if case .openReader = request {
                let blocked = locked { let value = self.blocked; self.blocked = false; return value }
                if blocked { entered.signal(); precondition(resume.wait(timeout: .now() + .seconds(5)) == .success) }
            }
            return locked {
                switch request {
                case .start, .state: return state()
                case .openReader(let expected, let pane, let destination):
                    guard expected == revision, pane == 1 else { return nil }
                    readers.insert(destination); log.append("handoff:\(destination)")
                    var out = header("open-reader"), nested = readerInfo(destination)
                    out.merge(location()) { _, new in new }
                    out["pane"] = "1"; out["captured_bytes"] = String(bytes.count)
                    out["label"] = label; out["source_reopened"] = false
                    if let corrupt {
                        switch corrupt {
                        case "owner": out["owner"] = "20"
                        case "revision": out["model_revision"] = "2"
                        case "file": out["file_id"] = "2"
                        case "pane": out["pane"] = "2"
                        case "offset": out["offset"] = "0"
                        case "selection": out["selection"] = NSNull()
                        case "label": out["label"] = "other.md"
                        case "length": out["captured_bytes"] = String(bytes.count + 1)
                        case "source-reopened": out["source_reopened"] = true
                        case "nested-owner": nested["owner"] = "999"
                        case "digest": nested["capture_sha256"] = String(repeating: "G", count: 64)
                        case "query": nested["accepted_query_generation"] = "1"
                        case "missing-query": nested.removeValue(forKey: "accepted_query_generation")
                        case "io": nested["initial_read_calls"] = "1"
                        default: preconditionFailure("unknown corruption")
                        }
                        self.corrupt = nil
                    }
                    out["reader"] = nested
                    return encode(out)
                case .window(let expected, _, let offset):
                    guard expected == revision, offset <= UInt64(bytes.count) else { return nil }
                    var out = header("window")
                    out.merge(location()) { _, new in new }; out["pane"] = "1"
                    out["original_range"] = ["start": String(offset), "end": String(bytes.count)]
                    out["text"] = String(decoding: bytes.dropFirst(Int(offset)), as: UTF8.self)
                    out["text_kind"] = "logical-captured-text-not-shaped"; out["line_semantics"] = "editor-rows"
                    out["has_replacements"] = false; out["reaches_eof"] = true
                    out["next_offset"] = NSNull(); out["rows"] = [["line": "1"]]
                    return encode(out)
                case .apply(let expected, let attempt, let kind, let pane, let start, let end, _):
                    guard expected == revision, attempt > revision, kind == 2, pane == 1 else { return nil }
                    revision = attempt; offset = start; selection = (start, end); log.append("update")
                    return state()
                default: return nil
                }
            }
        }, cancel: { [self] _ in locked { log.append("desk-cancel") }; return true }, close: { [self] _ in
            locked { log.append("desk-close") }; return true
        }, retirementFailed: { fatalError("desk retirement failed") }, readerDomain: domain)
    }
    func readerTransport(domain: UUID? = nil) -> AtlasReaderTransport {
        AtlasReaderTransport(create: { [self] in locked {
            let id = next; next += 1; log.append("create:\(id)"); return id
        } }, open: { [self] _, _, _ in locked { log.append("LIVE-OPEN") }; return nil },
        window: { [self] reader, offset, count in
            precondition(!Thread.isMainThread)
            return locked { guard readers.contains(reader) else { return nil }
                log.append("read:\(reader):\(offset)"); return page(reader, offset: offset, limit: count) }
        }, lines: { _, _, _, _ in nil }, cancel: { [self] reader in
            locked { log.append("reader-cancel:\(reader)") }; return true
        }, close: { [self] reader in
            precondition(!Thread.isMainThread)
            locked { readers.remove(reader); log.append("reader-close:\(reader)") }; return true
        }, retirementFailed: { fatalError("reader retirement failed") },
        fork: { [self] source, destination in locked {
            guard readers.contains(source) else { return nil }
            readers.insert(destination); log.append("fork:\(destination)"); return encode(readerInfo(destination))
        } }, comparison: AtlasReaderComparisonTransport(domain: domain ?? self.domain, compare: { _, _, _, _, _ in nil }))
    }
}

@main private enum AtlasDeskReaderTests {
    @MainActor private static func wait(_ message: String, _ ready: () -> Bool) async {
        for _ in 0..<1000 {
            if ready() { return }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        preconditionFailure(message)
    }
    @MainActor private static func started(_ fixture: DeskReaderFixture) async -> AtlasReadingDeskModel {
        let model = AtlasReadingDeskModel(transport: fixture.deskTransport)
        model.start(); await wait("desk did not start") { !model.busy }
        precondition(model.state?.owner == fixture.owner)
        return model
    }
    @MainActor static func main() async throws {
        let fixture = DeskReaderFixture(), desk = await started(fixture)
        let original = desk.state!.active!.location
        let opened = try await desk.openReader(pane: 1, transport: fixture.readerTransport())
        precondition(opened.reader.page?.start == original.offset && opened.pane.location == original)
        precondition(opened.reader.page?.text == "needle\r\nexact\0bytes\n")
        precondition(desk.state!.active!.location == original && desk.state?.revision == 1)
        precondition(!opened.reader.canRefresh && opened.reader.canPin && opened.reader.canCompare)
        opened.reader.refreshFromDisk()
        precondition(!fixture.calls.contains("LIVE-OPEN"), "a retained label granted a disk-open route")
        opened.reader.offsetInput = "0"; opened.reader.goToByte()
        await wait("reader did not navigate") { !opened.reader.busy }
        precondition(opened.keepCurrentPage(in: desk))
        await wait("desk location did not update") { !desk.busy }
        precondition(desk.state?.active?.location.selection?.end == UInt64(fixture.bytes.count))
        precondition(fixture.calls.filter { $0 == "update" }.count == 1)
        fixture.replacePane(); desk.reload(); await wait("desk replacement did not reload") { !desk.busy }
        precondition(!opened.canUpdate(desk) && !opened.keepCurrentPage(in: desk))
        precondition(fixture.calls.filter { $0 == "update" }.count == 1, "an old reader rebound offsets after restore")
        let pinned = try await opened.reader.fork()
        precondition(!pinned.canRefresh && pinned.page?.originalHex == opened.reader.page?.originalHex)
        desk.close()
        opened.reader.offsetInput = "8"; opened.reader.goToByte()
        await wait("independent reader did not survive desk close") { !opened.reader.busy }
        precondition(opened.reader.page?.text == "needle\r\nexact\0bytes\n")
        opened.reader.close(); pinned.close()

        let foreignFixture = DeskReaderFixture(), foreignDesk = await started(foreignFixture)
        do {
            _ = try await foreignDesk.openReader(pane: 1, transport: foreignFixture.readerTransport(domain: UUID()))
            preconditionFailure("foreign registry accepted")
        } catch { }
        precondition(!foreignFixture.calls.contains(where: { $0.hasPrefix("create:") }))
        foreignDesk.close()

        for mutation in ["owner", "revision", "file", "pane", "offset", "selection", "label", "length",
                         "source-reopened", "nested-owner", "digest", "query", "missing-query", "io"] {
            let fixture = DeskReaderFixture(), desk = await started(fixture)
            fixture.corruptNext(mutation)
            do {
                _ = try await desk.openReader(pane: 1, transport: fixture.readerTransport())
                preconditionFailure("corrupt transfer accepted: \(mutation)")
            } catch { }
            precondition(desk.state?.revision == 1 && !desk.busy)
            await wait("rejected destination not retired") { fixture.calls.contains("reader-close:100") }
            precondition(!fixture.calls.contains("LIVE-OPEN"))
            desk.close()
        }

        let emptyFixture = DeskReaderFixture(bytes: [], offset: 0), emptyDesk = await started(emptyFixture)
        let empty = try await emptyDesk.openReader(pane: 1, transport: emptyFixture.readerTransport())
        precondition(empty.reader.page?.originalHex == "" && empty.reader.page?.identity.capturedBytes == 0)
        empty.reader.close(); emptyDesk.close()

        let canceledFixture = DeskReaderFixture(), canceledDesk = await started(canceledFixture)
        canceledFixture.blockNext()
        let pending = Task { try await canceledDesk.openReader(pane: 1, transport: canceledFixture.readerTransport()) }
        await wait("handoff did not enter") { canceledFixture.entered.wait(timeout: .now()) == .success }
        canceledDesk.close()
        precondition(!canceledFixture.calls.contains("desk-close"), "desk retired before its export drained")
        canceledFixture.resume.signal()
        do { _ = try await pending.value; preconditionFailure("closed desk published a late transfer") }
        catch { }
        await wait("late initialized destination did not retire") {
            canceledFixture.calls.contains("reader-close:100") && canceledFixture.calls.contains("desk-close")
        }
        print("Desk reader transfer, source authority, exact navigation, stale update and cancellation regressions passed.")
    }
}
