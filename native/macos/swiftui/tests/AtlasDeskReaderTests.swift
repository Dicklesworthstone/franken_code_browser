import Foundation
import Dispatch

/// Only the foreign boundary is injected. The actual desk/reader coordinators,
/// receipt decoders, source navigation, cancellation and lifetime paths execute.
/// This suite does not substitute for native ABI or physical-Mac verification.
private final class DeskReaderFixture: @unchecked Sendable {
    private let lock = NSLock()
    let domain = UUID()
    let owner: UInt64 = 19
    let label: String
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

    init(bytes: [UInt8] = Array("# Saved\nneedle\r\nexact\0bytes\n".utf8), offset: UInt64 = 8,
         label: String = "../offline/README.md") {
        self.bytes = bytes; self.offset = offset; self.label = label
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
                case .keep(let expected, let attempt, let reader, let start, let end):
                    guard expected == revision, attempt > revision, readers.contains(reader),
                          start <= end, end <= UInt64(bytes.count) else { return nil }
                    revision = attempt; offset = start; selection = (start, end)
                    log.append("keep:\(start):\(end)")
                    return state()
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
    func initializeRetained(_ reader: UInt64) -> String? {
        precondition(!Thread.isMainThread)
        return locked { readers.insert(reader); return encode(readerInfo(reader)) }
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
        } }, comparison: AtlasReaderComparisonTransport(domain: domain ?? self.domain, compare: { _, _, _, _, _ in nil }),
        textSelection: AtlasReaderTextSelectionTransport(resolve: { [self] reader, windowStart, windowEnd, first, last in
            precondition(!Thread.isMainThread)
            return locked {
                guard readers.contains(reader), windowStart <= windowEnd, windowEnd <= UInt64(bytes.count),
                      first < last, last <= windowEnd - windowStart else { return nil }
                let start = windowStart + first, end = windowStart + last
                let raw = bytes[Int(start)..<Int(end)]
                guard let text = String(bytes: raw, encoding: .utf8) else { return nil }
                var out = readerHeader(reader, "resolve-selection")
                out["selection_namespace"] = "native-text"
                out["window_original_range"] = ["start": String(windowStart), "end": String(windowEnd)]
                out["window_utf8_range"] = ["start": String(first), "end": String(last)]
                out["original_range"] = ["start": String(start), "end": String(end)]
                out["text"] = text; out["original_hex"] = hex(raw)
                out["contains_replacements"] = false
                log.append("selection:\(start):\(end)")
                return encode(out)
            }
        }))

    }
}

/// Only the bounded transfer ABI is injected. The actual reader/desk models,
/// original-range selection and committed-but-undelivered reconciliation run.
private final class RetainedPassageDeskFixture: @unchecked Sendable {
    let owner: UInt64 = 59
    let domain: UUID
    let target: AtlasDeskRetainedPassageImport
    private let identity: AtlasReaderIdentity
    private let lock = NSLock()
    private var revision: UInt64 = 0
    private var accepted = false, lostReply = false, failingReads = 0
    private var keeps = 0, wholeTransfers = 0
    private let corruption: String?

    init(identity: AtlasReaderIdentity, domain: UUID, start: UInt64, corruption: String? = nil) {
        self.identity = identity; self.domain = domain; self.corruption = corruption
        target = AtlasDeskRetainedPassageImport(originOwner: identity.owner, originFile: identity.file,
            originRevision: identity.revision, capturedBytes: identity.capturedBytes,
            pathHex: identity.pathBytes.map { String(format: "%02x", $0) }.joined(), encoding: identity.encoding,
            selection: try! AtlasDeskObservedRange(start: start, end: start + 5))
    }
    private func locked<T>(_ body: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return body() }
    var counts: (keeps: Int, wholeTransfers: Int) { locked { (keeps, wholeTransfers) } }
    func loseReply(stateFailures: Int = 0) { locked { lostReply = true; failingReads = stateFailures } }
    private func encode(_ value: [String: Any]) -> String {
        String(decoding: try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self)
    }
    private func range(_ start: UInt64, _ end: UInt64) -> [String: String] {
        ["start": String(start), "end": String(end)]
    }
    private func header(_ command: String) -> [String: Any] {
        ["schema": "fcb.reading-desk/1", "status": "ok", "command": command, "owner": String(owner),
         "model_revision": String(revision), "last_attempt": String(revision),
         "additional_source_bytes_read": "0", "native_presented": false]
    }
    private func observation() -> [String: Any] {
        let context = target.capturedRange!
        var out: [String: Any] = ["observation": "1", "origin_owner": String(target.originOwner),
            "origin_file": String(target.originFile), "origin_observation": String(target.originRevision),
            "path": identity.displayPath, "path_hex": target.pathHex, "encoding": target.encoding,
            "consistency": "host-supplied-sequence", "extent_sha256": String(repeating: "a", count: 64),
            "captured_range": range(context.start, context.end), "observed_length": String(target.capturedBytes),
            "final_observed_length": String(target.capturedBytes), "capture_kind": "observed-extent",
            "whole_file_immutable": false, "source_reopened": false]
        switch corruption {
        case "owner": out["origin_owner"] = String(target.originOwner + 1)
        case "file": out["origin_file"] = String(target.originFile + 1)
        case "revision": out["origin_observation"] = String(target.originRevision + 1)
        case "path": out["path_hex"] = "6f74686572"
        case "encoding": out["encoding"] = "utf16le"
        case "length": out["observed_length"] = String(target.capturedBytes + 1)
        case "final-length": out["final_observed_length"] = String(target.capturedBytes + 1)
        case "context": out["captured_range"] = range(context.start + 1, context.end)
        case "consistency": out["consistency"] = "unchanged-metadata-not-atomic"
        default: break
        }
        return out
    }
    private func passage() -> [String: Any] {
        ["passage": "1", "observation": "1", "update_sequence": "1", "label": "Retained passage", "body": "",
         "selection": range(target.selection.start + (corruption == "selection" ? 1 : 0), target.selection.end)]
    }
    private func state() -> String {
        var out = header("state")
        out["panes"] = [String](); out["bookmarks"] = [String](); out["active_pane"] = NSNull()
        out["retained_sources"] = accepted ? "1" : "0"
        out["retained_source_bytes"] = accepted ? String(target.capturedRange!.count) : "0"
        out["can_go_back"] = false; out["can_go_forward"] = false
        out["persistence"] = "explicit-checkpoint"; out["autosave"] = false
        out["observations"] = accepted ? [observation()] : []
        out["observed_passages"] = accepted ? [passage()] : []
        return encode(out)
    }
    func transport(retainedEnabled: Bool = true, registryDomain: UUID? = nil) -> AtlasReadingDeskTransport {
        let admittedDomain = registryDomain ?? domain
        return AtlasReadingDeskTransport(create: { [self] in owner }, run: { [self] h, request in
            precondition(!Thread.isMainThread)
            return locked {
                guard h == owner else { return nil }
                switch request {
                case .start: return state()
                case .state:
                    if failingReads > 0 { failingReads -= 1; return nil }
                    return state()
                case .keep: wholeTransfers += 1; return nil
                default: return nil
                }
            }
        }, cancel: { _ in true }, close: { _ in true }, retirementFailed: { }, readerDomain: admittedDomain,
        observed: AtlasDeskObservedTransport(domain: UUID(), keep: { _, _, _, _, _, _, _ in nil },
            read: { [self] h, expected, id in locked {
                guard h == owner, expected == revision, id == 1, accepted else { return nil }
                var out = header("observed-passage")
                out["observation"] = observation(); out["passage"] = passage()
                out["text"] = "exact"; out["original_hex"] = "6578616374"; out["contains_replacements"] = false
                out["capture_kind"] = "observed-extent"; out["whole_file_immutable"] = false; out["source_reopened"] = false
                return encode(out)
            } }, edit: { _, _, _, _, _, _, _ in nil }, forget: { _, _, _, _, _ in nil }),
        retainedPassage: retainedEnabled ? AtlasDeskRetainedPassageTransport(domain: admittedDomain,
            keep: { [self] h, expected, attempt, reader, start, end in
                precondition(!Thread.isMainThread)
                return locked {
                    guard h == owner, expected == revision, attempt > revision,
                          reader == target.originOwner, start == target.selection.start,
                          end == target.selection.end else { return nil }
                    keeps += 1; accepted = true; revision = attempt
                    if lostReply { lostReply = false; return nil }
                    return state()
                }
            }) : nil)
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
        precondition(opened.reader.currentFileForExternalEditor() == nil,
            "a retained-only desk label became current-file editor authority")
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
        precondition(pinned.currentFileForExternalEditor() == nil,
            "pinning retained source invented an editor grant")
        desk.close()
        opened.reader.offsetInput = "8"; opened.reader.goToByte()
        await wait("independent reader did not survive desk close") { !opened.reader.busy }
        precondition(opened.reader.page?.text == "needle\r\nexact\0bytes\n")
        opened.reader.close(); pinned.close()

        // A plausible absolute filename is still a retained label, not proof
        // of current native access. Neither importing nor pinning may infer it.
        let namedFixture = DeskReaderFixture(label: "/project/README.md")
        let namedDesk = await started(namedFixture)
        let named = try await namedDesk.openReader(pane: 1, transport: namedFixture.readerTransport())
        precondition(named.reader.page?.identity.pathBytes == Array(namedFixture.label.utf8))
        precondition(named.reader.currentFileForExternalEditor() == nil)
        let namedPin = try await named.reader.fork()
        named.reader.close(); namedDesk.close()
        precondition(namedPin.page != nil && namedPin.currentFileForExternalEditor() == nil,
            "an absolute retained label acquired a live capability after its parent closed")
        precondition(!namedFixture.calls.contains("LIVE-OPEN"))
        namedPin.close()

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
        try await nativeSelectionUpdatesExactDeskRange()
        try await comparedRangeKeepsFullTargetAndPoint()
        try await firstKeepUsesSelectedRangeAndRejectsChangedIntent()
        try await largeRetainedCapturePassages()
        print("Desk reader transfer, exact native selections, source authority, navigation, stale update and cancellation regressions passed.")
    }

    @MainActor private static func comparedRangeKeepsFullTargetAndPoint() async throws {
        let count = AtlasReaderLimits.pageBytes * 2 + 31
        let fixture = DeskReaderFixture(bytes: Array(repeating: 120, count: Int(count)))
        let desk = await started(fixture)
        let capture = try await desk.openReader(pane: 1, transport: fixture.readerTransport())
        defer { capture.reader.close(); desk.close() }
        let reader = capture.reader, identity = reader.page!.identity
        let full = AtlasReaderComparisonReport.Range(start: 17, end: count - 12)
        precondition(reader.goToComparedRange(full, identity: identity))
        await wait("compared desk range did not open") { !reader.busy }
        precondition(reader.currentComparisonSelection?.showsWholeRange == false
            && reader.rangeForReadingDesk?.start == full.start && reader.rangeForReadingDesk?.end == full.end,
            "clipped comparison fell back to the current page's range")
        precondition(capture.keepCurrentPage(in: desk))
        await wait("full compared range was not kept") { !desk.busy }
        precondition(desk.state?.active?.location.selection?.start == full.start
            && desk.state?.active?.location.selection?.end == full.end,
            "Keep discarded compared bytes beyond the displayed page")

        let point = AtlasReaderComparisonReport.Range(start: count, end: count)
        precondition(reader.goToComparedRange(point, identity: identity))
        await wait("compared point did not open") { !reader.busy }
        precondition(capture.keepCurrentPage(in: desk))
        await wait("compared point was not kept") { !desk.busy }
        precondition(desk.state?.active?.location.selection?.start == count
            && desk.state?.active?.location.selection?.end == count,
            "Keep widened an insertion/deletion point into a source passage")
    }

    @MainActor private static func nativeSelectionUpdatesExactDeskRange() async throws {
        let fixture = DeskReaderFixture(), desk = await started(fixture)
        let capture = try await desk.openReader(pane: 1, transport: fixture.readerTransport())
        defer { capture.reader.close(); desk.close() }
        let reader = capture.reader
        let range = (reader.source!.text as NSString).range(of: "exact\0bytes")
        reader.updateTextSelection(source: reader.source!, navigation: reader.navigation, range: range)
        precondition(reader.hasTextSelectionIntent && !capture.canUpdate(desk)
            && !capture.keepCurrentPage(in: desk), "pending selection fell back to the full page")
        await wait("selected text did not resolve") { reader.textSelection != nil }
        precondition(reader.rangeForReadingDesk?.start == 16 && reader.rangeForReadingDesk?.end == 27)
        precondition(capture.keepCurrentPage(in: desk))
        await wait("selected desk range did not update") { !desk.busy }
        precondition(desk.state?.active?.location.selection?.start == 16
            && desk.state?.active?.location.selection?.end == 27,
            "Keep stored the page instead of the selected passage")
        let updates = fixture.calls.filter { $0 == "update" }.count
        reader.updateTextSelection(source: reader.source!, navigation: reader.navigation,
            range: NSRange(location: NSNotFound, length: 1))
        await wait("invalid native range did not refuse") { !reader.resolvingTextSelection }
        precondition(reader.hasTextSelectionIntent && reader.rangeForReadingDesk?.start == nil
            && !capture.keepCurrentPage(in: desk))
        precondition(fixture.calls.filter { $0 == "update" }.count == updates,
            "a refused native selection changed the saved range")
    }

    @MainActor private static func firstKeepUsesSelectedRangeAndRejectsChangedIntent() async throws {
        let fixture = DeskReaderFixture(), desk = await started(fixture)
        let capture = try await desk.openReader(pane: 1, transport: fixture.readerTransport())
        defer { capture.reader.close(); desk.close() }
        let reader = capture.reader
        reader.updateTextSelection(source: reader.source!, navigation: reader.navigation,
            range: NSRange(location: 0, length: 6))
        await wait("first Keep selection did not resolve") { reader.textSelection != nil }
        desk.keep(reader: reader)
        await wait("first Keep did not finish") { !desk.busy }
        precondition(fixture.calls.contains("keep:8:14"),
            "the ordinary Keep entry point did not import the exact selected passage")
        precondition(desk.state?.active?.location.selection?.start == 8
            && desk.state?.active?.location.selection?.end == 14)

        let previous = desk.state!.active!.location
        let keeps = fixture.calls.filter { $0.hasPrefix("keep:") }.count
        desk.keep(reader: reader)
        // Change intent before the desk's asynchronous transfer gets its turn.
        // Equal source identity alone must not authorize the obsolete action.
        reader.updateTextSelection(source: reader.source!, navigation: reader.navigation, range: nil)
        await wait("obsolete Keep did not finish") { !desk.busy }
        precondition(fixture.calls.filter { $0.hasPrefix("keep:") }.count == keeps,
            "an obsolete selected passage crossed the retained-reader handoff")
        precondition(desk.state!.active!.location == previous,
            "selection revocation changed already accepted personal state")
        precondition(!fixture.calls.contains("LIVE-OPEN"))
    }

    @MainActor private static func largeRetainedCapturePassages() async throws {
        let start: UInt64 = 5 * 1024 * 1024
        let source = DeskReaderFixture(bytes: Array(repeating: 120, count: Int(start))
            + Array("exact retained evidence".utf8))
        let reader = AtlasPagedReaderModel(transport: source.readerTransport())
        try await reader.openRetained(label: source.label, offset: start, registryDomain: source.domain) { handle in
            source.initializeRetained(handle)
        }
        defer { reader.close() }
        precondition(reader.canKeep && !reader.canRefresh)
        reader.updateTextSelection(source: reader.source!, navigation: reader.navigation,
            range: NSRange(location: 0, length: 5))
        precondition(!reader.canKeep, "pending large-source selection fell back to its page")
        await wait("large retained selection did not resolve") { reader.textSelection != nil }
        let identity = reader.page!.identity
        let fixture = RetainedPassageDeskFixture(identity: identity, domain: source.domain, start: start)
        let desk = AtlasReadingDeskModel(transport: fixture.transport())
        defer { desk.close() }
        fixture.loseReply()
        desk.keep(reader: reader)
        await wait("lost retained passage reply did not reconcile") { !desk.busy && desk.state != nil }
        precondition(fixture.counts.keeps == 1 && fixture.counts.wholeTransfers == 0)
        precondition(desk.lastKeptObservedPassage == 1 && !desk.hasUnverifiedChanges)
        precondition(desk.state!.panes.isEmpty && desk.state!.retainedBytes == 21)
        precondition(desk.state!.observedPassages[0].selection.start == start)
        precondition(desk.state!.observations[0].consistency == "host-supplied-sequence")
        precondition(desk.canSaveCheckpoint && desk.hasUnsavedContents)

        for missing in [false, true] {
            let rejected = RetainedPassageDeskFixture(identity: identity, domain: source.domain, start: start)
            let model = AtlasReadingDeskModel(transport: rejected.transport(retainedEnabled: !missing,
                registryDomain: missing ? source.domain : UUID()))
            model.keep(reader: reader)
            precondition(!model.busy && rejected.counts.keeps == 0 && rejected.counts.wholeTransfers == 0)
            if missing { precondition(model.notice.contains("cannot save a bounded passage")) }
            model.close()
        }

        for corruption in ["owner", "file", "revision", "path", "encoding", "length", "final-length",
                           "context", "consistency", "selection"] {
            let rejected = RetainedPassageDeskFixture(identity: identity, domain: source.domain,
                start: start, corruption: corruption)
            let model = AtlasReadingDeskModel(transport: rejected.transport())
            model.keep(reader: reader)
            await wait("corrupt retained passage reply did not finish") { !model.busy }
            precondition(rejected.counts.keeps == 1 && model.lastKeptObservedPassage == nil
                && model.hasUnverifiedChanges, "unrelated retained passage was confirmed: \(corruption)")
            model.close()
        }

        let unknown = RetainedPassageDeskFixture(identity: identity, domain: source.domain, start: start)
        let unknownDesk = AtlasReadingDeskModel(transport: unknown.transport())
        unknown.loseReply(stateFailures: 1)
        unknownDesk.keep(reader: reader)
        await wait("unverifiable saved passage did not finish") { !unknownDesk.busy }
        precondition(unknown.counts.keeps == 1 && unknownDesk.hasUnverifiedChanges)
        unknownDesk.keep(reader: reader)
        precondition(unknown.counts.keeps == 1 && !unknownDesk.busy,
            "uncertain saved passage was imported a second time")
        unknownDesk.close()

        let stale = RetainedPassageDeskFixture(identity: identity, domain: source.domain, start: start)
        let staleDesk = AtlasReadingDeskModel(transport: stale.transport())
        staleDesk.keep(reader: reader)
        reader.updateTextSelection(source: reader.source!, navigation: reader.navigation, range: nil)
        await wait("revoked large-source selection did not finish") { !staleDesk.busy }
        precondition(stale.counts.keeps == 0 && stale.counts.wholeTransfers == 0
            && staleDesk.state?.revision == 0, "changed intent transferred a wider source passage")
        staleDesk.close()

        // A larger logical comparison range cannot quietly become its clipped
        // page. Point selections remain points, not invented evidence bytes.
        let wide = AtlasReaderComparisonReport.Range(start: 0, end: AtlasReaderLimits.maximumPageBytes + 1)
        precondition(reader.goToComparedRange(wide, identity: identity))
        await wait("large compared selection did not open") { !reader.busy }
        precondition(!reader.canKeep && reader.rangeForReadingDesk?.end == wide.end)
        let point = AtlasReaderComparisonReport.Range(start: start, end: start)
        precondition(reader.goToComparedRange(point, identity: identity))
        await wait("large compared point did not open") { !reader.busy }
        precondition(!reader.canKeep)

        reader.close()
        let state = desk.state!, target = AtlasDeskObservedTarget(state: state, passage: state.observedPassages[0])!
        let saved = try await desk.readObservedPassage(target)
        precondition(saved.text == "exact" && saved.originalHex == "6578616374")
        precondition(saved.target.passage.selection.start == start && !source.calls.contains("LIVE-OPEN"))
    }

}
