// Portable production decoder/coordinator tests. Link the existing search
// models, cancellation primitive and AtlasProjectIO; no Apple UI or C bridge.
// swiftc -swift-version 6 -warnings-as-errors AtlasSearch.swift \
//   AtlasSearchCoordinator.swift AtlasProjectIO.swift AtlasReader.swift AtlasReaderSearch.swift \
//   AtlasReaderCoordinator.swift AtlasSource.swift AtlasPagedReaderModel.swift \
//   tests/AtlasReaderTests.swift -o /tmp/fcb-reader-tests
import Foundation
import Dispatch

private func hex(_ bytes: some Sequence<UInt8>) -> String {
    bytes.map { String(format: "%02x", $0) }.joined()
}
private func json(_ object: [String: Any]) -> String {
    String(data: try! JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]), encoding: .utf8)!
}
private func header(owner: UInt64 = 2, path: String = "/project/file", length: UInt64 = 13,
                    encoding: String = "utf8", command: String = "info") -> [String: Any] {
    ["schema": "fcb.reader-session/1", "status": "ok", "command": command,
     "owner": String(owner), "file_id": "1", "source_revision": "1",
     "path": ["encoding": "unix-bytes", "hex": hex(path.utf8), "display": path],
     "captured_bytes": String(length), "additional_source_bytes_read": "0", "native_presented": false,
     "encoding": encoding, "capture_origin": "regular-file-observation-not-atomic"]
}
private func window(owner: UInt64 = 2, path: String = "/project/file", raw: [UInt8] = Array("zero\none\ntwo\n".utf8),
                    start: UInt64 = 0, end: UInt64? = nil, command: String = "window", first: UInt64? = nil,
                    encoding: String = "utf8", text: String? = nil) -> [String: Any] {
    let end = end ?? UInt64(raw.count)
    let bytes = raw[Int(start)..<Int(end)]
    var wire = header(owner: owner, path: path, length: UInt64(raw.count), encoding: encoding, command: command)
    wire["text_kind"] = "logical-captured-text-not-shaped"
    wire["requested_original_range"] = ["start": String(start), "end": String(end)]
    wire["visible_range"] = ["start": String(start), "end": String(end)]
    wire["range_limited"] = false
    wire["boundaries_adjusted"] = false
    wire["has_replacements"] = false
    wire["text"] = text ?? String(decoding: bytes, as: UTF8.self)
    wire["original_hex"] = hex(bytes)
    wire["first_physical_line"] = first.map { String($0) as Any } ?? NSNull()
    wire["next_offset"] = end < UInt64(raw.count) ? String(end) as Any : NSNull()
    wire["selection"] = NSNull()
    return wire
}

private final class Lease {}
private final class Driver: @unchecked Sendable {
    private let lock = NSLock()
    private var next: UInt64 = 2
    private var calls: [String] = []
    private var closeAttempts = 0
    private var closed = 0
    private var closeOnMain = false
    private var failedRetirements = 0
    private var blockNextWindow = false
    private var blockNextOpen = false
    private var closeFailures = 0
    let entered = DispatchSemaphore(value: 0)
    let gate = DispatchSemaphore(value: 0)

    func blockWindow() { lock.lock(); blockNextWindow = true; lock.unlock() }
    func blockOpen() { lock.lock(); blockNextOpen = true; lock.unlock() }
    func refuseClose(_ count: Int) { lock.lock(); closeFailures = count; lock.unlock() }
    var snapshot: (calls: [String], closed: Int, attempts: Int, closeOnMain: Bool, failures: Int) {
        lock.lock(); defer { lock.unlock() }
        return (calls, closed, closeAttempts, closeOnMain, failedRetirements)
    }
    private func record(_ name: String) { lock.lock(); calls.append(name); lock.unlock() }
    private func pause(open: Bool) {
        lock.lock()
        let pause = open ? blockNextOpen : blockNextWindow
        if open { blockNextOpen = false } else { blockNextWindow = false }
        lock.unlock()
        if pause { entered.signal(); precondition(gate.wait(timeout: .now() + .seconds(5)) == .success) }
    }
    var transport: AtlasReaderTransport {
        AtlasReaderTransport(create: { [self] in
            lock.lock(); defer { lock.unlock() }
            let value = next; next += 1; calls.append("create:\(value)"); return value
        }, open: { [self] owner, path, limit in
            precondition(!Thread.isMainThread)
            precondition(limit == AtlasReaderLimits.captureBytes)
            record("open:\(owner)"); pause(open: true)
            return json(header(owner: owner, path: path))
        }, window: { [self] owner, offset, bytes in
            precondition(!Thread.isMainThread)
            record("window:\(owner):\(offset)"); pause(open: false)
            return json(window(owner: owner, start: offset, end: min(13, offset + bytes)))
        }, lines: { [self] owner, first, _, _ in
            precondition(!Thread.isMainThread)
            record("lines:\(owner):\(first)")
            if first == 999 {
                return json(["schema": "fcb.reader-session/1", "owner": String(owner), "status": "error",
                             "error": ["code": "READER_SESSION_MISSING_LINE"]])
            }
            return json(window(owner: owner, start: 5, end: 9, command: "lines", first: first))
        }, cancel: { [self] owner in record("cancel:\(owner)"); return true }, close: { [self] owner in
            lock.lock(); defer { lock.unlock() }
            calls.append("close:\(owner)"); closeAttempts += 1
            closeOnMain = closeOnMain || Thread.isMainThread
            if closeFailures > 0 { closeFailures -= 1; return false }
            closed += 1; return true
        }, retirementFailed: { [self] in lock.lock(); failedRetirements += 1; lock.unlock() })
    }
}

@main struct AtlasReaderTests {
    @MainActor private static var assertions = 0
    @MainActor private static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        assertions += 1
        precondition(condition(), message)
    }
    @MainActor private static func rejects(_ message: String, _ body: () throws -> Void) {
        do { try body(); fatalError(message) } catch { assertions += 1 }
    }
    @MainActor private static func pump() {
        _ = RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.005))
    }
    @MainActor private static func wait(_ message: String, _ condition: () -> Bool) {
        let deadline = Date().addingTimeInterval(5)
        var satisfied = condition()
        while !satisfied, Date() < deadline { pump(); satisfied = condition() }
        check(satisfied, message)
    }
    @MainActor static func main() throws {
        // Lexical admission never trims/normalizes native identities or treats
        // catalog display labels as authority for an escaping relative path.
        let input = try AtlasReaderInput(root: "/project", path: "file")
        check(input.fullPath == "/project/file", "join")
        let rootInput = try AtlasReaderInput(root: "/", path: "file")
        check(rootInput.fullPath == "/file", "root join")
        for path in ["", "/outside", "../outside", "a/../b", "a//b", "./b", "a/", "a\0b"] {
            rejects("unsafe relative path admitted") { _ = try AtlasReaderInput(root: "/project", path: path) }
        }
        rejects("oversized path admitted") { _ = try AtlasReaderInput(root: "/project", path: String(repeating: "a", count: 16_384)) }
        let info = try AtlasReaderInfo.decode(json(header()), handle: 2, path: input.fullPath)
        check(info.identity.capturedBytes == 13, "capture size")
        rejects("foreign owner accepted") { _ = try AtlasReaderInfo.decode(json(header()), handle: 3, path: input.fullPath) }
        rejects("foreign path accepted") { _ = try AtlasReaderInfo.decode(json(header()), handle: 2, path: "/other/file") }
        rejects("Unicode path alias accepted") {
            _ = try AtlasReaderInfo.decode(json(header(path: "/project/é")), handle: 2, path: "/project/e\u{301}")
        }
        for change in [["captured_bytes": "18446744073709551615"], ["captured_bytes": "013"],
                       ["additional_source_bytes_read": "1"], ["source_revision": "0"]] {
            var value = header(); value.merge(change) { _, new in new }
            rejects("bad identity admitted") { _ = try AtlasReaderInfo.decode(json(value), handle: 2, path: input.fullPath) }
        }
        let first = try AtlasReaderPage.decode(json(window()), info: info, request: .firstPage)
        check(first.text == "zero\none\ntwo\n" && first.nextOffset == nil, "whole small page")
        let page = try AtlasReaderPage.decode(json(window(start: 5, end: 9)), info: info, request: .window(offset: 5, bytes: 4))
        check(page.text == "one\n" && page.nextOffset == 9 && page.originalHex == "6f6e650a", "byte page")
        let line = try AtlasReaderPage.decode(json(window(start: 5, end: 9, command: "lines", first: 2)),
            info: info, request: .lines(first: 2, count: 1, bytes: 64))
        check(line.firstPhysicalLine == 2 && line.start == 5, "physical-line page")
        var fragment = window(start: 0, end: 4); fragment["next_offset"] = NSNull()
        let fragmentPage = try AtlasReaderPage.decode(json(fragment), info: info, request: .window(offset: 0, bytes: 4))
        check(fragmentPage.nextOffset == 4,
              "completed fragment must not strand the rest of the capture")
        for (key, value) in [("owner", "3"), ("source_revision", "2"), ("original_hex", "ff"),
                             ("next_offset", "1"), ("command", "hit"), ("encoding", "utf16le")] {
            var bad = window(); bad[key] = value
            rejects("unbound page accepted: \(key)") { _ = try AtlasReaderPage.decode(json(bad), info: info, request: .firstPage) }
        }
        var selected = window(); selected["selection"] = ["original_range": ["start": "0", "end": "1"]]
        rejects("search selection borrowed by a page") { _ = try AtlasReaderPage.decode(json(selected), info: info, request: .firstPage) }
        rejects("wrong requested page accepted") { _ = try AtlasReaderPage.decode(json(window()), info: info, request: .window(offset: 1, bytes: 64)) }
        for request in [AtlasReaderRequest.window(offset: UInt64.max, bytes: 64), .window(offset: 0, bytes: 3),
                        .window(offset: 0, bytes: UInt64.max), .lines(first: 0, count: 1, bytes: 64),
                        .lines(first: 1, count: 1025, bytes: 64)] {
            rejects("invalid navigation admitted") { try request.validate(capturedBytes: 13) }
        }
        let emptyInfo = try AtlasReaderInfo.decode(json(header(length: 0)), handle: 2, path: input.fullPath)
        let empty = try AtlasReaderPage.decode(json(window(raw: [])), info: emptyInfo, request: .firstPage)
        check(empty.start == 0 && empty.end == 0 && empty.text.isEmpty && empty.nextOffset == nil, "empty file")
        let nul: [UInt8] = [97, 0, 98]
        let nulInfo = try AtlasReaderInfo.decode(json(header(length: 3)), handle: 2, path: input.fullPath)
        let nulPage = try AtlasReaderPage.decode(json(window(raw: nul)), info: nulInfo, request: .firstPage)
        check(Array(nulPage.text.utf8) == nul && nulPage.originalHex == "610062", "embedded NUL lost")
        let utf16: [UInt8] = [255, 254, 61, 216, 0, 222, 13, 0, 10, 0]
        let utf16Info = try AtlasReaderInfo.decode(json(header(length: 10, encoding: "utf16le")), handle: 2, path: input.fullPath)
        let utf16Page = try AtlasReaderPage.decode(json(window(raw: utf16, encoding: "utf16le", text: "😀\r\n")), info: utf16Info, request: .firstPage)
        check(utf16Page.text == "😀\r\n" && utf16Page.originalHex == "fffe3dd800de0d000a00", "UTF-16 text/byte domains mixed")
        var adjusted = window(raw: utf16, start: 2, end: 10, encoding: "utf16le", text: "😀\r\n")
        adjusted["requested_original_range"] = ["start": "3", "end": "7"]
        adjusted["boundaries_adjusted"] = true
        let aligned = try AtlasReaderPage.decode(json(adjusted), info: utf16Info, request: .window(offset: 3, bytes: 4))
        check(aligned.start == 2 && aligned.end == 10 && aligned.boundariesAdjusted, "scalar/CRLF adjustment lost")
        var limited = window(); limited["visible_range"] = ["start": "0", "end": "4"]
        limited["range_limited"] = true; limited["original_hex"] = "7a65726f"; limited["text"] = "zero"
        limited["command"] = "lines"; limited["first_physical_line"] = "1"; limited["next_offset"] = "4"
        let cut = try AtlasReaderPage.decode(json(limited), info: info, request: .lines(first: 1, count: 3, bytes: 4))
        check(cut.rangeLimited && cut.nextOffset == 4, "line byte limit disguised as complete")
        var replacement = window(raw: [255], text: "�"); replacement["has_replacements"] = true
        let replacementInfo = try AtlasReaderInfo.decode(json(header(length: 1)), handle: 2, path: input.fullPath)
        let repaired = try AtlasReaderPage.decode(json(replacement), info: replacementInfo, request: .firstPage)
        check(repaired.hasReplacements && repaired.originalHex == "ff", "replacement destroyed exact-byte copy")
        let engineError = json(["schema": "fcb.reader-session/1", "owner": "2", "status": "error", "error": ["code": "READER_SESSION_MISSING_LINE"]])
        do {
            _ = try AtlasReaderPage.decode(engineError, info: info, request: .firstPage)
            fatalError("engine failure became an empty page")
        } catch AtlasReaderError.engine(let code) { check(code == "READER_SESSION_MISSING_LINE", "engine code lost") }
        rejects("nil became an empty page") { _ = try AtlasReaderPage.decode(nil, info: info, request: .firstPage) }

        // Real production queue/coordinator: one open, retained windows, and
        // foreign work off the interaction thread. Failures retain the source.
        do {
            let driver = Driver()
            let coordinator = AtlasReaderCoordinator(transport: driver.transport)
            var done = false
            Task {
                do {
                    let page = try await coordinator.open(input: input)
                    check(page.end == 13, "initial retained page")
                    let line = try await coordinator.read(.lines(first: 2, count: 1, bytes: 64))
                    check(line.text == "one\n", "retained line navigation")
                    do { _ = try await coordinator.read(.window(offset: UInt64.max, bytes: 64)); fatalError("bad offset admitted") }
                    catch AtlasReaderError.invalidInput { }
                    let page2 = try await coordinator.read(.window(offset: 9, bytes: 4))
                    check(page2.text == "two\n", "bad input destroyed the retained capture")
                    coordinator.close(); done = true
                } catch { fatalError("reader lifecycle failed: \(error)") }
            }
            wait("retained navigation did not finish") { done }
            wait("capture did not retire") { driver.snapshot.closed == 1 }
            check(driver.snapshot.calls.filter { $0.hasPrefix("open:") }.count == 1, "navigation reopened source")
            check(!driver.snapshot.closeOnMain && driver.snapshot.failures == 0, "retired on interaction thread")
        }

        // Rapid navigation coalesces the pending descriptor and suppresses the
        // completed old page, rather than queuing every click or moving twice.
        do {
            let driver = Driver()
            let coordinator = AtlasReaderCoordinator(transport: driver.transport)
            var opened = false
            Task { _ = try! await coordinator.open(input: input); opened = true }
            wait("open for coalescing") { opened }
            driver.blockWindow()
            var canceled = 0, delivered: [UInt64] = []
            Task {
                do { let page = try await coordinator.read(.window(offset: 0, bytes: 4)); delivered.append(page.start) }
                catch AtlasReaderError.canceled { canceled += 1 } catch { fatalError("unexpected failure: \(error)") }
            }
            wait("active window did not begin") { driver.entered.wait(timeout: .now()) == .success }
            Task {
                do { let page = try await coordinator.read(.window(offset: 4, bytes: 4)); delivered.append(page.start) }
                catch AtlasReaderError.canceled { canceled += 1 } catch { fatalError("unexpected failure: \(error)") }
            }
            wait("middle request not admitted") { driver.snapshot.calls.filter { $0 == "cancel:2" }.count >= 2 }
            Task {
                do { let page = try await coordinator.read(.window(offset: 8, bytes: 4)); delivered.append(page.start) }
                catch AtlasReaderError.canceled { canceled += 1 } catch { fatalError("unexpected failure: \(error)") }
            }
            wait("pending request not coalesced") { canceled == 1 }
            driver.gate.signal()
            wait("newest window lost") { delivered == [8] && canceled == 2 }
            check(!driver.snapshot.calls.contains("window:2:4"), "superseded page reached the bridge")
            coordinator.close()
            wait("coalescing capture not retired") { driver.snapshot.closed == 1 }
        }

        // Closing during capture revokes delivery now, but holds the access
        // lease and slot until foreign work returns. Brief close contention is
        // retried off-main without duplicating a successful close.
        do {
            let driver = Driver(); driver.blockOpen(); driver.refuseClose(2)
            let coordinator = AtlasReaderCoordinator(transport: driver.transport)
            var lease: Lease? = Lease()
            weak var weakLease = lease
            var stopped = false
            Task { [access = lease] in
                do { _ = try await coordinator.open(input: input, accessLease: access); fatalError("closed source published") }
                catch AtlasReaderError.canceled { stopped = true } catch { fatalError("wrong close result: \(error)") }
            }
            lease = nil
            wait("capture did not enter foreign work") { driver.entered.wait(timeout: .now()) == .success }
            coordinator.close()
            check(weakLease != nil && driver.snapshot.closed == 0, "in-flight access/handle retired early")
            driver.gate.signal()
            wait("closed capture did not drain") { stopped && driver.snapshot.closed == 1 && weakLease == nil }
            check(!driver.snapshot.calls.contains("window:2:0"), "canceled open continued into source presentation")
            check(driver.snapshot.attempts == 3 && !driver.snapshot.closeOnMain, "close contention/lane handling")
        }

        // The actual observable presentation model commits history only after
        // accepted navigation, preserves pages on errors, and clears on close.
        do {
            let driver = Driver()
            let model = AtlasPagedReaderModel(transport: driver.transport)
            model.open(root: "/project", path: "file", accessLease: nil)
            wait("model failed to open") { !model.busy && model.page != nil }
            check(model.source?.text == "zero\none\ntwo\n" && !model.canGoBack, "initial model source")
            model.lineInput = "2"; model.goToLine()
            wait("model line navigation") { !model.busy && model.page?.firstPhysicalLine == 2 }
            let accepted = model.source
            check(model.canGoBack && model.canGoNext, "line page navigation controls")
            model.lineInput = "999"; model.goToLine()
            wait("model missing-line refusal") { !model.busy }
            check(model.source === accepted && model.page?.start == 5 && model.canGoBack,
                  "failed line request replaced source/history")
            check(model.notice.contains("READER_SESSION_MISSING_LINE"), "missing-line error hidden")
            model.next()
            wait("model next") { !model.busy && model.page?.start == 9 }
            check(model.source?.text == "two\n" && !model.canGoNext, "EOF page")
            model.back()
            wait("model back to physical lines") { !model.busy && model.page?.firstPhysicalLine == 2 }
            check(model.page?.end == 9, "Back widened the previous line request into a byte window")
            model.back()
            wait("model back to beginning") { !model.busy && model.page?.start == 0 }
            check(!model.canGoBack, "failed request polluted navigation history")
            let original = model.source
            model.offsetInput = "18446744073709551615"; model.goToByte()
            check(model.source === original && !model.busy, "wide invalid offset revoked source")
            model.close()
            check(model.page == nil && model.source == nil && !model.canGoBack, "close retained visible source")
            wait("model handle did not retire") { driver.snapshot.closed == 1 }
        }

        // Window/view revocation is synchronous even while native work remains
        // active. A later completion cannot reinstall closed-reader content.
        do {
            let driver = Driver()
            let model = AtlasPagedReaderModel(transport: driver.transport)
            model.open(root: "/project", path: "file", accessLease: nil)
            wait("model revocation open") { !model.busy && model.page != nil }
            driver.blockWindow()
            model.offsetInput = "5"; model.goToByte()
            wait("model active page") { driver.entered.wait(timeout: .now()) == .success }
            model.close()
            check(model.page == nil && model.source == nil && !model.busy, "old page visible after revocation")
            driver.gate.signal()
            wait("model revoked work did not drain") { driver.snapshot.closed == 1 }
            pump()
            check(model.page == nil && model.source == nil, "stale page republished after close")
        }
        print("PASS: \(assertions) retained-reader decoder, paging, cancellation and lifetime checks")
    }
}
