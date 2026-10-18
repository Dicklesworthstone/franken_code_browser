import Foundation
import Dispatch

/// Fixed independent source/digest vectors at the transport boundary. The
/// fixture checks submitted bytes before returning a known SHA; it does not
/// replace production hashing, reader decoding, scheduling or publication.
struct AtlasSuppliedReaderVector: Sendable {
    let text: String
    let sha256: String
    static let abc = Self(text: "abc", sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    static let changed = Self(text: "changed bytes\n", sha256: "7c60358cb724c1b671469c5f167cea45a4f70df4e3ac87b79340692e6f17f574")
    static let exactCases: [Self] = [
        .init(text: "", sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
        .abc,
        .init(text: "\u{feff}abc\0e\u{301}\r\n", sha256: "6a54fa7ae81fb568919d1e4b5e3c5a987679c865cb1eae6da14f2258f3f0bafc"),
        .init(text: "é", sha256: "4a99557e4033c3539de2eb65472017cad5f9557f7a0625a09f1c3f6e2ba69c4c"),
        .init(text: "e\u{301}", sha256: "bf12767b0f2a56b2190075bae8169f656e3ce8d6357d4aff184bc6c7ea48f9f6"),
        .init(text: "🎵\tשלום\r\n", sha256: "6c46fc967c7ce5d33115c07e74dc2e911b5c3f9d2f4943de0107b7d518560ef6")
    ]
}

final class AtlasSuppliedReaderFixture: @unchecked Sendable {
    enum Fault: CaseIterable, Sendable {
        case none, refused, digest, missingDigest, malformedExpectedDigest, length, rawPath,
             origin, encoding, sourceReads, readCalls, additionalReads, query, outline,
             document, missingGeneration, firstPageIdentity
    }
    struct Capture {
        let path: String
        let bytes: Data
        let sha256: String
        var supplied: Bool
    }
    struct State {
        var next: UInt64 = 1
        var expected: AtlasSuppliedReaderVector
        var live = AtlasSuppliedReaderVector.changed
        var fault = Fault.none
        var failLive = false
        var gate: Gate?
        var captures: [UInt64: Capture] = [:]
        var supplied: [UInt64] = []
        var liveOpens: [UInt64] = []
        var windows: [UInt64] = []
        var forks: [UInt64] = []
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
            precondition(!Thread.isMainThread)
            lock.lock(); entered = true; lock.unlock()
            precondition(semaphore.wait(timeout: .now() + 10) == .success, "supplied fixture gate timed out")
        }
        func release() { semaphore.signal() }
    }
    final class Lease {
        let fixture: AtlasSuppliedReaderFixture
        init(_ fixture: AtlasSuppliedReaderFixture) { self.fixture = fixture }
        deinit { fixture.change { $0.releasedLeases += 1 } }
    }
    private let lock = NSLock()
    private var state: State
    init(_ expected: AtlasSuppliedReaderVector = .abc) { state = State(expected: expected) }
    func change<T>(_ body: (inout State) -> T) -> T {
        lock.lock(); defer { lock.unlock() }; return body(&state)
    }
    var snapshot: State { change { $0 } }

    func transport(supplied: Bool = true) -> AtlasReaderTransport {
        let openBytes: (@Sendable (UInt64, String, Data) -> AtlasReaderTransport.SuppliedResponse?)?
        if supplied { openBytes = { [self] in supply($0, path: $1, bytes: $2) } }
        else { openBytes = nil }
        return .init(create: { [self] in change { state in
            let handle = state.next; state.next += 1; return handle
        } }, open: { [self] handle, path, _ in open(handle, path: path) },
        window: { [self] handle, offset, count in window(handle, offset: offset, count: count) },
        lines: { _, _, _, _ in nil },
        cancel: { [self] handle in change { $0.canceled.append(handle) }; return true },
        close: { [self] handle in
            precondition(!Thread.isMainThread)
            change { $0.closed[handle, default: 0] += 1; $0.captures[handle] = nil }; return true
        }, retirementFailed: { [self] in change { $0.retirementFailures += 1 } },
        fork: { [self] source, destination in
            precondition(!Thread.isMainThread)
            guard var capture = snapshot.captures[source] else { return nil }
            capture.supplied = true
            change { $0.forks.append(destination); $0.captures[destination] = capture }
            return Self.json(Self.header(destination, capture, command: "info"))
        }, openBytes: openBytes)
    }

    private func supply(_ handle: UInt64, path: String, bytes: Data) -> AtlasReaderTransport.SuppliedResponse? {
        precondition(!Thread.isMainThread)
        let (capture, fault, gate): (Capture, Fault, Gate?) = change { state in
            // A same-looking normalized string is an intentional failing control.
            precondition(bytes == Data(state.expected.text.utf8), "supplied source bytes changed")
            let capture = Capture(path: path, bytes: bytes, sha256: state.expected.sha256, supplied: true)
            state.captures[handle] = capture; state.supplied.append(handle)
            let gate = state.gate; state.gate = nil
            return (capture, state.fault, gate)
        }
        gate?.wait() // Deliberately ignore late cancellation, like an in-flight foreign call.
        if fault == .refused { return nil }
        var info = Self.header(handle, capture, command: "info")
        switch fault {
        case .digest: info["capture_sha256"] = String(repeating: "0", count: 64)
        case .missingDigest: info["capture_sha256"] = nil
        case .length: info["captured_bytes"] = String(bytes.count + 1)
        case .rawPath:
            info["path"] = ["encoding": "unix-bytes", "hex": Self.hex("/project/e\u{301}.md".utf8), "display": path]
        case .origin: info["capture_origin"] = "regular-file-observation-not-atomic"
        case .encoding: info["encoding"] = "utf16le"
        case .sourceReads: info["initial_source_bytes_read"] = "1"
        case .readCalls: info["initial_read_calls"] = "1"
        case .additionalReads: info["additional_source_bytes_read"] = "1"
        case .query: info["accepted_query_generation"] = "7"
        case .outline: info["accepted_outline_generation"] = "7"
        case .document: info["accepted_document_generation"] = "7"
        case .missingGeneration: info["accepted_document_generation"] = nil
        default: break
        }
        return .init(json: Self.json(info), sha256: fault == .malformedExpectedDigest ? "ABC" : capture.sha256)
    }

    private func open(_ handle: UInt64, path: String) -> String? {
        precondition(!Thread.isMainThread)
        let capture: Capture? = change { state in
            state.liveOpens.append(handle)
            guard !state.failLive else { return nil }
            let capture = Capture(path: path, bytes: Data(state.live.text.utf8), sha256: state.live.sha256, supplied: false)
            state.captures[handle] = capture
            return capture
        }
        return capture.map { Self.json(Self.header(handle, $0, command: "info")) }
    }

    private func window(_ handle: UInt64, offset: UInt64, count: UInt64) -> String? {
        precondition(!Thread.isMainThread)
        change { $0.windows.append(handle) }
        guard let capture = snapshot.captures[handle], offset <= UInt64(capture.bytes.count) else { return nil }
        let end = offset + min(count, UInt64(capture.bytes.count) - offset)
        let bytes = capture.bytes[Int(offset)..<Int(end)]
        var wire = Self.header(handle, capture, command: "window")
        wire["text_kind"] = "logical-captured-text-not-shaped"
        wire["text"] = String(decoding: bytes, as: UTF8.self)
        wire["original_hex"] = Self.hex(bytes)
        wire["requested_original_range"] = ["start": String(offset), "end": String(end)]
        wire["visible_range"] = ["start": String(offset), "end": String(end)]
        wire["range_limited"] = false; wire["boundaries_adjusted"] = false; wire["has_replacements"] = false
        wire["first_physical_line"] = NSNull(); wire["selection"] = NSNull()
        wire["next_offset"] = end < UInt64(capture.bytes.count) ? String(end) as Any : NSNull()
        if snapshot.fault == .firstPageIdentity { wire["source_revision"] = "2" }
        return Self.json(wire)
    }

    private static func header(_ handle: UInt64, _ capture: Capture, command: String) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": command,
         "owner": String(handle), "file_id": "1", "source_revision": "1",
         "captured_bytes": String(capture.bytes.count), "encoding": "utf8",
         "path": ["encoding": "unix-bytes", "hex": hex(capture.path.utf8), "display": capture.path],
         "capture_origin": capture.supplied ? "host-supplied" : "regular-file-observation-not-atomic",
         "capture_sha256": capture.sha256,
         "initial_source_bytes_read": capture.supplied ? "0" : String(capture.bytes.count),
         "initial_read_calls": capture.supplied ? "0" : "1",
         "accepted_query_generation": NSNull(), "accepted_outline_generation": NSNull(),
         "accepted_document_generation": NSNull(), "additional_source_bytes_read": "0", "native_presented": false]
    }
    static func hex(_ bytes: some Sequence<UInt8>) -> String {
        bytes.map { String(format: "%02x", $0) }.joined()
    }
    private static func json(_ value: [String: Any]) -> String {
        String(decoding: try! JSONSerialization.data(withJSONObject: value), as: UTF8.self)
    }
}
