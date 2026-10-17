import Foundation
import Dispatch

/// Fixed ASCII capture fixtures at the marshaling boundary. The coordinator,
/// bounded queue, cancellation, decoder and session retirement are production
/// code. This does not emulate Rust capture/search correctness or native grants.
final class AtlasReaderRefreshFixture: @unchecked Sendable {
    enum Failure: Equatable { case none, open, page, identity }
    struct Capture {
        let text: String
        let path: String
        let failure: Failure
    }
    struct State {
        var nextHandle: UInt64 = 1
        var text = "old capture\n"
        var failure = Failure.none
        var refuseCreate = false
        var gate: Gate?
        var captures: [UInt64: Capture] = [:]
        var queries: [UInt64: (UInt64, String)] = [:]
        var opens: [UInt64] = []
        var windows: [UInt64] = []
        var hits: [UInt64] = []
        var canceled: [UInt64] = []
        var closed: [UInt64: Int] = [:]
        var retirementFailures = 0
        var releasedLeases = 0
    }
    final class Gate: @unchecked Sendable {
        private let lock = NSLock()
        private let semaphore = DispatchSemaphore(value: 0)
        private var entered = false
        var didEnter: Bool { lock.lock(); defer { lock.unlock() }; return entered }
        func wait() {
            lock.lock(); entered = true; lock.unlock()
            // Wait only on the production I/O worker, never the main actor.
            precondition(semaphore.wait(timeout: .now() + 10) == .success, "fixture gate timed out")
        }
        func release() { semaphore.signal() }
    }
    final class Lease {
        let fixture: AtlasReaderRefreshFixture
        init(_ fixture: AtlasReaderRefreshFixture) { self.fixture = fixture }
        deinit { fixture.change { $0.releasedLeases += 1 } }
    }
    private let lock = NSLock()
    private var state = State()
    func change<T>(_ body: (inout State) -> T) -> T {
        lock.lock(); defer { lock.unlock() }
        return body(&state)
    }
    var snapshot: State { change { $0 } }
    var transport: AtlasReaderTransport {
        .init(create: { [self] in change { state in
            if state.refuseCreate { return 0 }
            let handle = state.nextHandle; state.nextHandle += 1
            return handle
        } }, open: { [self] handle, path, _ in open(handle, path: path) },
        window: { [self] handle, offset, bytes in window(handle, offset: offset, bytes: bytes) },
        lines: { _, _, _, _ in nil },
        cancel: { [self] handle in change { $0.canceled.append(handle) }; return true },
        close: { [self] handle in change { $0.closed[handle, default: 0] += 1 }; return true },
        retirementFailed: { [self] in change { $0.retirementFailures += 1 } })
    }
    var search: AtlasReaderSearchTransport {
        .init(find: { [self] handle, generation, needle, _, _ in
            guard let capture = snapshot.captures[handle], capture.text.hasPrefix(needle) else { return nil }
            change { $0.queries[handle] = (generation, needle) }
            var wire = header(handle, capture: capture, command: "find")
            wire["query_generation"] = String(generation)
            wire["mode"] = "exact-decoded-literal"; wire["needle"] = needle
            wire["state"] = "complete-observed-input"; wire["search_complete"] = true
            wire["scanned_bytes"] = String(capture.text.utf8.count)
            wire["matches_seen"] = "1"; wire["retained_hits"] = "1"
            wire["literal_original_hex"] = Self.hex(needle.utf8)
            wire["hits"] = [["hit_index": "0", "occurrence_id": "1",
                "original_range": Self.range(0, UInt64(needle.utf8.count))]]
            return Self.json(wire)
        }, hit: { [self] handle, generation, index, _ in
            change { $0.hits.append(handle) }
            guard let capture = snapshot.captures[handle], let query = snapshot.queries[handle],
                  generation == query.0, index == 0 else { return nil }
            var wire = page(handle, capture: capture, offset: 0, bytes: UInt64(capture.text.utf8.count))
            wire["command"] = "hit"
            let range = Self.range(0, UInt64(query.1.utf8.count))
            wire["selection"] = ["query_generation": String(generation),
                "original_range": range, "window_utf8_range": range,
                "original_hex": Self.hex(query.1.utf8)]
            return Self.json(wire)
        })
    }
    private func open(_ handle: UInt64, path: String) -> String? {
        let (capture, gate): (Capture, Gate?) = change { state in
            let capture = Capture(text: state.text, path: path, failure: state.failure)
            state.captures[handle] = capture; state.opens.append(handle)
            let gate = state.gate; state.gate = nil
            return (capture, gate)
        }
        gate?.wait() // Deliberately ignore cancellation to exercise late delivery.
        if capture.failure == .open { return nil }
        var wire = header(handle, capture: capture, command: "info")
        wire["capture_origin"] = "regular-file-observation-not-atomic"
        return Self.json(wire)
    }
    private func window(_ handle: UInt64, offset: UInt64, bytes: UInt64) -> String? {
        change { $0.windows.append(handle) }
        guard let capture = snapshot.captures[handle], capture.failure != .page,
              offset <= UInt64(capture.text.utf8.count) else { return nil }
        var wire = page(handle, capture: capture, offset: offset, bytes: bytes)
        if capture.failure == .identity { wire["source_revision"] = "999" }
        return Self.json(wire)
    }
    private func page(_ handle: UInt64, capture: Capture, offset: UInt64, bytes: UInt64) -> [String: Any] {
        var wire = header(handle, capture: capture, command: "window")
        let raw = Array(capture.text.utf8)
        let end = offset + min(bytes, UInt64(raw.count) - offset)
        let visible = raw[Int(offset)..<Int(end)]
        wire["text_kind"] = "logical-captured-text-not-shaped"
        wire["text"] = String(decoding: visible, as: UTF8.self)
        wire["original_hex"] = Self.hex(visible)
        wire["requested_original_range"] = Self.range(offset, end)
        wire["visible_range"] = Self.range(offset, end)
        wire["range_limited"] = false; wire["boundaries_adjusted"] = false; wire["has_replacements"] = false
        if end < UInt64(raw.count) { wire["next_offset"] = String(end) }
        return wire
    }
    private func header(_ handle: UInt64, capture: Capture, command: String) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": command,
         "owner": String(handle), "file_id": "1", "source_revision": "1",
         "captured_bytes": String(capture.text.utf8.count), "encoding": "utf8",
         "additional_source_bytes_read": "0", "native_presented": false,
         "path": ["encoding": "unix-bytes", "hex": Self.hex(capture.path.utf8), "display": capture.path]]
    }
    private static func range(_ start: UInt64, _ end: UInt64) -> [String: String] {
        ["start": String(start), "end": String(end)]
    }
    private static func hex<S: Sequence>(_ bytes: S) -> String where S.Element == UInt8 {
        bytes.map { String(format: "%02x", Int($0)) }.joined()
    }
    private static func json(_ value: [String: Any]) -> String {
        String(decoding: try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self)
    }
}
