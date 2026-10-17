// Production coordinator/model fork tests. Only the C transport is injected;
// source-page decoding, independent ownership, bounded queues, cancellation,
// navigation and publication run through the native application's real code.
// This portable suite does not qualify the Swift-to-Rust ABI or native views.
import Foundation
import Dispatch

private func hex(_ bytes: some Sequence<UInt8>) -> String {
    bytes.map { String(format: "%02x", $0) }.joined()
}
private func json(_ value: [String: Any]) -> String {
    String(data: try! JSONSerialization.data(withJSONObject: value), encoding: .utf8)!
}
private struct Capture: Sendable {
    let path: String
    let bytes: [UInt8]
    let encoding: String
    var origin = "regular-file-observation-not-atomic"

    func header(_ owner: UInt64, _ command: String) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": command,
         "owner": String(owner), "file_id": "1", "source_revision": "1",
         "path": ["encoding": "unix-bytes", "hex": hex(path.utf8), "display": path],
         "captured_bytes": String(bytes.count), "encoding": encoding, "capture_origin": origin,
         "additional_source_bytes_read": "0", "native_presented": false]
    }
    func window(_ owner: UInt64, offset: UInt64, limit: UInt64) -> [String: Any] {
        let end = min(UInt64(bytes.count), offset + limit)
        let raw = bytes[Int(offset)..<Int(end)]
        let text: String
        if encoding == "utf16le" || encoding == "utf16be" {
            text = String(data: Data(raw), encoding: .utf16) ?? "�"
        } else { text = String(decoding: raw, as: UTF8.self) }
        var out = header(owner, "window")
        out["text_kind"] = "logical-captured-text-not-shaped"
        out["requested_original_range"] = ["start": String(offset), "end": String(end)]
        out["visible_range"] = ["start": String(offset), "end": String(end)]
        out["range_limited"] = false; out["boundaries_adjusted"] = false
        out["has_replacements"] = encoding == "utf8" && String(bytes: raw, encoding: .utf8) == nil
        out["text"] = text; out["original_hex"] = hex(raw)
        out["first_physical_line"] = NSNull(); out["selection"] = NSNull()
        out["next_offset"] = end < UInt64(bytes.count) ? String(end) as Any : NSNull()
        return out
    }
}

private final class Lease {}
private final class Driver: @unchecked Sendable {
    private let lock = NSLock()
    private var next: UInt64 = 1
    private var live: [UInt8]
    private let encoding: String
    private var captures: [UInt64: Capture] = [:]
    private var queries: [UInt64: (generation: UInt64, needle: String, positions: [Int])] = [:]
    private var calls: [String] = []
    private var blockedFork = false
    private var refusedCreate = false
    private var refusedFork = false
    private var corruption: String?
    let entered = DispatchSemaphore(value: 0)
    let gate = DispatchSemaphore(value: 0)

    init(bytes: [UInt8] = Array("zero\none\ntwo\n".utf8), encoding: String = "utf8") {
        live = bytes; self.encoding = encoding
    }
    var log: [String] { lock.lock(); defer { lock.unlock() }; return calls }
    func setLive(_ text: String) { lock.lock(); live = Array(text.utf8); lock.unlock() }
    func blockFork() { lock.lock(); blockedFork = true; lock.unlock() }
    func refuseCreate() { lock.lock(); refusedCreate = true; lock.unlock() }
    func refuseFork() { lock.lock(); refusedFork = true; lock.unlock() }
    func corrupt(_ kind: String) { lock.lock(); corruption = kind; lock.unlock() }

    private func create() -> UInt64 {
        lock.lock(); defer { lock.unlock() }
        if refusedCreate { refusedCreate = false; calls.append("create-refused"); return 0 }
        let id = next; next += 1; calls.append("create:\(id)"); return id
    }
    private func open(_ owner: UInt64, path: String) -> String {
        precondition(!Thread.isMainThread)
        lock.lock(); defer { lock.unlock() }
        let capture = Capture(path: path, bytes: live, encoding: encoding)
        captures[owner] = capture; calls.append("open:\(owner):\(path)")
        return json(capture.header(owner, "info"))
    }
    private func window(_ owner: UInt64, offset: UInt64, bytes: UInt64) -> String? {
        precondition(!Thread.isMainThread)
        lock.lock(); defer { lock.unlock() }
        guard let capture = captures[owner], offset <= UInt64(capture.bytes.count) else { return nil }
        calls.append("window:\(owner):\(offset)")
        return json(capture.window(owner, offset: offset, limit: bytes))
    }
    private func fork(_ source: UInt64, _ destination: UInt64) -> String? {
        precondition(!Thread.isMainThread)
        lock.lock()
        let capture = captures[source], paused = blockedFork, refused = refusedFork, corrupt = corruption
        blockedFork = false; refusedFork = false; corruption = nil
        calls.append("fork:\(source):\(destination)")
        lock.unlock()
        if paused {
            entered.signal()
            precondition(gate.wait(timeout: .now() + .seconds(5)) == .success)
        }
        guard !refused, var copied = capture else { return nil }
        copied.origin = "host-supplied"
        if corrupt == "bytes" {
            copied = Capture(path: copied.path, bytes: Array(repeating: 120, count: copied.bytes.count),
                encoding: copied.encoding, origin: "host-supplied")
        }
        lock.lock(); captures[destination] = copied; lock.unlock()
        var info = copied.header(destination, "info")
        if corrupt == "owner" { info["owner"] = String(source) }
        if corrupt == "path" { info["path"] = ["encoding": "unix-bytes", "hex": hex("/elsewhere".utf8), "display": "/elsewhere"] }
        if corrupt == "encoding" { info["encoding"] = "utf16le" }
        if corrupt == "origin" { info["capture_origin"] = "regular-file-observation-not-atomic" }
        if corrupt == "length" { info["captured_bytes"] = String(copied.bytes.count + 1) }
        return json(info)
    }
    private func cancel(_ owner: UInt64) -> Bool {
        lock.lock(); calls.append("cancel:\(owner)"); lock.unlock(); return true
    }
    private func close(_ owner: UInt64) -> Bool {
        precondition(!Thread.isMainThread)
        lock.lock(); calls.append("close:\(owner)"); captures[owner] = nil; queries[owner] = nil
        lock.unlock(); return true
    }
    func transport(forkEnabled: Bool = true) -> AtlasReaderTransport {
        let copy: (@Sendable (UInt64, UInt64) -> String?)?
        if forkEnabled {
            copy = { [self] source, destination in fork(source, destination) }
        } else {
            copy = nil
        }
        return AtlasReaderTransport(create: { [self] in create() },
            open: { [self] owner, path, limit in
                precondition(limit == AtlasReaderLimits.captureBytes)
                return open(owner, path: path)
            }, window: { [self] in window($0, offset: $1, bytes: $2) },
            lines: { _, _, _, _ in nil }, cancel: { [self] in cancel($0) },
            close: { [self] in close($0) }, retirementFailed: { fatalError("failed retirement") },
            fork: copy)
    }
    var search: AtlasReaderSearchTransport {
        AtlasReaderSearchTransport(find: { [self] owner, generation, needle, _, _ in
            precondition(!Thread.isMainThread)
            lock.lock(); defer { lock.unlock() }
            guard let capture = captures[owner] else { return nil }
            let bytes = Array(needle.utf8)
            let positions = capture.bytes.indices.filter { start in
                start + bytes.count <= capture.bytes.count
                    && capture.bytes[start..<(start + bytes.count)].elementsEqual(bytes)
            }
            queries[owner] = (generation, needle, positions)
            calls.append("find:\(owner):\(generation)")
            var out = capture.header(owner, "find")
            out["query_generation"] = String(generation); out["mode"] = "exact-decoded-literal"
            out["needle"] = needle; out["state"] = "complete-observed-input"; out["search_complete"] = true
            out["scanned_bytes"] = String(capture.bytes.count)
            out["matches_seen"] = String(positions.count); out["retained_hits"] = String(positions.count)
            out["literal_original_hex"] = positions.isEmpty ? NSNull() as Any : hex(bytes)
            out["unsupported_at"] = NSNull()
            out["hits"] = positions.enumerated().map { index, start in
                ["hit_index": String(index), "occurrence_id": String(index + 1),
                 "original_range": ["start": String(start), "end": String(start + bytes.count)]] as [String: Any]
            }
            return json(out)
        }, hit: { [self] owner, generation, index, _ in
            precondition(!Thread.isMainThread)
            lock.lock(); defer { lock.unlock() }
            guard let capture = captures[owner], let query = queries[owner],
                  query.generation == generation, index < UInt64(query.positions.count) else { return nil }
            let start = query.positions[Int(index)], end = start + query.needle.utf8.count
            var out = capture.window(owner, offset: 0, limit: UInt64(capture.bytes.count))
            out["command"] = "hit"
            out["selection"] = ["query_generation": String(generation),
                "original_range": ["start": String(start), "end": String(end)],
                "window_utf8_range": ["start": String(start), "end": String(end)],
                "original_hex": hex(capture.bytes[start..<end])]
            return json(out)
        })
    }
}

@main private enum AtlasReaderForkTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ value: @autoclosure () -> Bool, _ message: String) {
        precondition(value(), message); checks += 1
    }
    @MainActor private static func wait(_ message: String, _ ready: () -> Bool) {
        let deadline = Date().addingTimeInterval(5)
        var done = ready()
        while !done, Date() < deadline {
            _ = RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.005))
            done = ready()
        }
        check(done, message)
    }
    @MainActor private static func opened(_ driver: Driver, lease: AnyObject? = nil,
                                           forkEnabled: Bool = true) -> AtlasPagedReaderModel {
        let model = AtlasPagedReaderModel(transport: driver.transport(forkEnabled: forkEnabled), search: driver.search)
        model.open(root: "/project", path: "file", accessLease: lease)
        wait("source open did not finish") { !model.busy }
        check(model.page != nil, "source opening failed")
        return model
    }
    @MainActor private static func pin(_ model: AtlasPagedReaderModel) -> AtlasPagedReaderModel {
        var result: AtlasPagedReaderModel?
        Task {
            do { result = try await model.fork() }
            catch { fatalError("fork failed: \(error)") }
        }
        wait("fork did not finish") { result != nil }
        return result!
    }
    @MainActor private static func refused(_ model: AtlasPagedReaderModel, pending: Bool = false) {
        var failed = false
        Task {
            do { _ = try await model.fork(); fatalError("invalid fork published") }
            catch { failed = true }
        }
        wait("fork did not refuse") { failed }
        check(model.pinning == pending, "refusal changed another pin's admission")
    }

    @MainActor static func main() {
        // Inert capability discovery and legacy transports remain supported.
        do {
            let driver = Driver()
            let inert = AtlasPagedReaderModel(transport: driver.transport())
            check(!inert.canPin && driver.log.isEmpty, "pin capability starts a capture")
            let unsupported = opened(driver, forkEnabled: false)
            let before = driver.log
            check(!unsupported.canPin, "missing transport capability advertised")
            refused(unsupported)
            check(driver.log == before, "unsupported fork touched source or queue")
            unsupported.close()
        }

        // Pin at a nonzero window, with independent navigation and a root lease
        // that survives closing the source pane. An explicit child refresh is
        // the only operation that reads changed live bytes.
        do {
            let driver = Driver()
            var lease: Lease? = Lease(); weak var weakLease = lease
            let parent = opened(driver, lease: lease)
            lease = nil
            parent.offsetInput = "5"; parent.goToByte()
            wait("parent navigation") { !parent.busy }
            let original = parent.page!, navigation = parent.navigation
            let before = driver.log
            let child = pin(parent)
            check(parent.navigation == navigation && parent.page?.identity.matches(original.identity) == true,
                  "pin changed parent navigation")
            check(child.page?.start == 5 && child.page?.text == "one\ntwo\n", "pin lost current byte window")
            check(child.page?.originalHex == original.originalHex, "pin changed visible original bytes")
            check(child.page?.identity.owner != original.identity.owner && !child.canGoBack && !child.canGoForward,
                  "pin inherited parent owner/history")
            check(child.findReport == nil && child.outlinePage == nil && child.fileQuery.isEmpty
                  && !child.documentPreview.authorized && child.nativeSelectionRange == nil,
                  "pin inherited selection authority")
            check(!driver.log.dropFirst(before.count).contains(where: { $0.hasPrefix("open:") || $0 == "cancel:1" }),
                  "pin reopened disk or canceled parent")
            driver.setLive("changed file\n")
            parent.close()
            wait("parent did not retire independently") { driver.log.contains("close:1") }
            check(weakLease != nil && child.canFind && child.canRefresh, "closing parent revoked child/grant")
            child.offsetInput = "0"; child.goToByte()
            wait("child navigation after parent close") { !child.busy }
            check(child.page?.text == "zero\none\ntwo\n", "child substituted live bytes")
            child.refreshFromDisk()
            wait("explicit child refresh") { !child.busy }
            check(child.page?.text == "changed file\n" && child.refreshedFromDisk,
                  "explicit refresh did not use authorized live path")
            check(driver.log.contains("open:3:/project/file"), "refresh lost original authorized root/path")
            child.close()
            wait("grant did not retire with last child") { weakLease == nil }
        }

        // Actual model search selection remains in the parent; the child earns
        // its own generation/owner even when both query counters start at one.
        do {
            let driver = Driver()
            let source = opened(driver)
            source.fileQuery = "one"; source.find()
            wait("parent selected search") { !source.busy && source.matchHex != nil }
            let query = source.findReport!.generation, selection = source.nativeSelectionRange
            let child = pin(source)
            check(source.findReport?.generation == query && source.nativeSelectionRange == selection
                  && source.matchHex == "6f6e65", "fork revoked parent's accepted search")
            check(child.fileQuery.isEmpty && child.matchHex == nil && child.findReport == nil,
                  "child borrowed parent hit IDs")
            child.fileQuery = "two"; child.find()
            wait("child independently selected search") { !child.busy && child.matchHex != nil }
            check(child.findReport?.generation == 1 && child.matchHex == "74776f", "child query namespace changed")
            check(source.matchHex == "6f6e65", "child query replaced parent's selection")
            source.close()
            check(child.matchHex == "74776f", "parent close revoked child's selected hit")
            child.close()
        }

        // Denied capacity and malformed/foreign fork responses preserve the
        // entire parent surface and retire every unsuccessfully admitted child.
        do {
            let driver = Driver()
            let source = opened(driver)
            let identity = source.page!.identity, navigation = source.navigation
            let before = driver.log
            driver.refuseCreate(); refused(source)
            check(Array(driver.log.dropFirst(before.count)) == ["create-refused"], "admission refusal touched accepted source")
            driver.refuseFork(); refused(source)
            for corrupt in ["owner", "path", "encoding", "origin", "length", "bytes"] {
                driver.corrupt(corrupt); refused(source)
            }
            check(source.page?.identity.matches(identity) == true && source.navigation == navigation && source.canPin,
                  "refused child replaced or disabled parent")
            check(!driver.log.contains("cancel:1"), "fork refusal canceled parent's capture")
            wait("failed children leaked handles") { driver.log.filter { $0.hasPrefix("close:") }.count == 7 }
            source.close()
        }

        // A pending pin has one admission slot. Caller cancellation must retire
        // the child after the blocked foreign call drains, without canceling
        // the parent or publishing a result from that canceled request.
        do {
            let driver = Driver()
            let parent = opened(driver)
            driver.blockFork()
            var canceled = false
            let task = Task {
                do { _ = try await parent.fork(); fatalError("canceled pin published") }
                catch { canceled = true }
            }
            wait("fork did not enter") { driver.entered.wait(timeout: .now()) == .success }
            check(parent.pinning && !parent.canPin, "overlapping pin admission remained open")
            let before = driver.log
            refused(parent, pending: true)
            check(driver.log == before, "second pin entered queue")
            task.cancel(); driver.gate.signal()
            wait("caller cancellation did not drain") { canceled }
            wait("canceled child did not retire") { driver.log.contains("close:2") }
            check(parent.canPin && parent.page != nil && !driver.log.contains("cancel:1"),
                  "caller canceled source reader")
            let child = pin(parent)
            child.close(); parent.close()
        }

        // Navigation, refresh, close and close/reopen revoke the old pin's
        // publication. Keep the root lease alive until blocked work returns.
        for action in ["navigate", "refresh", "close", "reopen"] {
            let driver = Driver()
            var lease: Lease? = Lease(); weak var weakLease = lease
            let parent = opened(driver, lease: lease)
            lease = nil
            driver.blockFork()
            var refused = false
            Task {
                do { _ = try await parent.fork(); fatalError("obsolete pin published after \(action)") }
                catch { refused = true }
            }
            wait("fork race did not enter") { driver.entered.wait(timeout: .now()) == .success }
            switch action {
            case "navigate": parent.offsetInput = "5"; parent.goToByte()
            case "refresh": driver.setLive("changed file\n"); parent.refreshFromDisk()
            case "close": parent.close()
            default: parent.close(); parent.open(root: "/project", path: "file", accessLease: nil)
            }
            check(weakLease != nil, "grant released during foreign pin drain")
            driver.gate.signal()
            wait("obsolete fork did not terminate") { refused }
            wait("replacement navigation did not finish") { !parent.busy }
            if action == "navigate" { check(parent.page?.start == 5, "canceled pin undid navigation") }
            if action == "refresh" { check(parent.page?.text == "changed file\n", "pin undid explicit refresh") }
            if action == "close" { check(parent.page == nil, "pin revived closed parent") }
            parent.close()
            wait("race leaked root grant") { weakLease == nil }
        }

        // Empty, NUL, malformed UTF-8 and BOM-marked UTF-16 retain raw bytes.
        // Text presentation and original-byte copy remain separate domains.
        for (bytes, encoding) in [([UInt8](), "utf8"), ([97, 0, 98], "utf8"),
                                  ([255, 97], "utf8"), ([255, 254, 97, 0, 13, 0, 10, 0], "utf16le"),
                                  ([254, 255, 0, 97], "utf16be")] {
            let driver = Driver(bytes: bytes, encoding: encoding)
            let source = opened(driver)
            let child = pin(source)
            check(child.page?.originalHex == hex(bytes) && child.page?.identity.encoding == encoding,
                  "fork changed encoded capture")
            source.close(); child.close()
        }
        print("AtlasReaderForkTests: \(checks) assertions passed")
    }
}
