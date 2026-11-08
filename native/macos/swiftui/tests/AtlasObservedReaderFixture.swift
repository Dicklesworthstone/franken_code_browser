import Foundation
import Dispatch

/// Injected receipts model the production wire contract. They do not execute
/// the Rust decoder, filesystem policy, hashing or native text presentation.
enum ObservedReaderFixture {
    static let length: UInt64 = (1 << 33) + 32
    static func hex(_ bytes: [UInt8]) -> String { bytes.map { String(format: "%02x", $0) }.joined() }
    static func encode(_ object: [String: Any]) throws -> String {
        String(decoding: try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]), as: UTF8.self)
    }
    static func range(_ start: UInt64, _ end: UInt64) -> [String: String] { ["start": String(start), "end": String(end)] }
    static func common(_ owner: UInt64, _ input: AtlasObservedReaderInput, _ command: String) -> [String: Any] {
        ["schema": "fcb.observed-reader/1", "status": "ok", "command": command,
         "owner": String(owner), "file_id": "1", "capture_kind": "observed-extent",
         "source_observation": "open-file-ranges-not-atomic", "whole_file_immutable": false, "native_presented": false,
         "path": ["encoding": "unix-bytes", "hex": hex(input.absolutePath), "display": "escaped presentation label"]]
    }
    static func info(_ owner: UInt64, _ input: AtlasObservedReaderInput) -> [String: Any] {
        var out = common(owner, input, "info")
        out["initial_observed_length"] = String(length)
        out["payload_bytes_read"] = "0"; out["read_calls"] = "0"
        out["cache_capacity"] = "8"; out["retained_payload_bytes"] = "0"
        out["retained_observations"] = [String]()
        return out
    }
    static func page(_ owner: UInt64, _ input: AtlasObservedReaderInput, _ id: UInt64,
                     _ request: AtlasObservedReaderRequest, length: UInt64 = ObservedReaderFixture.length,
                     text: String = "old\n", originalHex: String? = nil) -> [String: Any] {
        var out = common(owner, input, "window")
        let end = min(length, request.offset + request.bytes)
        let captureStart = request.offset > 8 ? request.offset - 8 : 0
        let captureEnd = min(length, end + 8)
        let count = captureEnd - captureStart
        out["observation"] = String(id); out["encoding"] = request.encoding.rawValue
        out["encoding_basis"] = "explicit-for-this-observation"
        out["requested_offset"] = String(request.offset); out["requested_bytes"] = String(request.bytes)
        out["requested_visible_range"] = range(request.offset, end)
        out["requested_capture_range"] = range(captureStart, captureEnd)
        out["observed_length"] = String(length); out["final_observed_length"] = String(length)
        out["captured_range"] = range(captureStart, captureEnd); out["captured_bytes"] = String(count)
        out["extent_sha256"] = String(repeating: "a", count: 64)
        out["consistency"] = "unchanged-metadata-not-atomic"; out["requested_capture_filled"] = true
        out["whole_file_observed"] = captureStart == 0 && captureEnd == length
        out["observation_source_bytes_read"] = String(count); out["observation_read_calls"] = count == 0 ? "0" : "1"
        out["additional_source_bytes_read"] = String(count); out["additional_read_calls"] = count == 0 ? "0" : "1"
        out["text_kind"] = "logical-observed-text-not-shaped"; out["visible_range"] = range(request.offset, end)
        out["boundaries_adjusted"] = false; out["first_physical_line"] = NSNull()
        if request.offset == 0 && end > 0 { out["first_physical_line"] = "1" }
        out["text"] = text; out["original_hex"] = originalHex ?? hex(Array(text.utf8))
        out["has_replacements"] = false; out["next_offset"] = NSNull()
        if end < length { out["next_offset"] = String(end) }
        return out
    }
    static func retained(_ page: [String: Any]) -> [String: Any] {
        var out = page; out["command"] = "retained-window"
        out["additional_source_bytes_read"] = "0"; out["additional_read_calls"] = "0"
        return out
    }
    static func copy(_ page: [String: Any]) -> [String: Any] {
        var out = retained(page); out["command"] = "copy-range"
        out["copy_domain"] = "original-bytes"; out["original_range"] = out["visible_range"]
        for key in ["text_kind", "visible_range", "boundaries_adjusted", "first_physical_line", "text", "has_replacements", "next_offset"] {
            out.removeValue(forKey: key)
        }
        return out
    }
    static func failure(_ owner: UInt64, _ code: String) throws -> String {
        try encode(["schema": "fcb.observed-reader/1", "status": "error", "owner": String(owner),
            "exit_code": "2", "error": ["code": code]])
    }
}

final class ObservedReaderGate: @unchecked Sendable {
    private let lock = NSLock()
    private let semaphore = DispatchSemaphore(value: 0)
    private var didEnter = false
    var entered: Bool { lock.lock(); defer { lock.unlock() }; return didEnter }
    func wait() {
        lock.lock(); didEnter = true; lock.unlock()
        precondition(semaphore.wait(timeout: .now() + 5) == .success,
            "Observed reader fixture gate was never released")
    }
    func release() { semaphore.signal() }
}

final class ObservedReaderProbe: @unchecked Sendable {
    private let lock = NSLock()
    private var nextOwner: UInt64 = 40
    private var inputs: [UInt64: AtlasObservedReaderInput] = [:]
    private var ids: [UInt64: UInt64] = [:]
    private var pages: [UInt64: [UInt64: [String: Any]]] = [:]
    private var requests: [UInt64] = []
    private var active = 0, mostActive = 0, creates = 0, opens = 0, copies = 0, restores = 0
    private var gates: [UInt64: ObservedReaderGate] = [:]
    private var failureOnNextOpen = false
    private var closeAllowed = true
    private var closes: [UInt64: Int] = [:]
    private var closed: Set<UInt64> = []
    private var failures = 0
    private var released: Set<String> = []
    private var content = "old\n"

    var transport: AtlasObservedReaderTransport {
        AtlasObservedReaderTransport(create: { self.create() },
            open: { owner, input, _ in try self.open(owner, input) },
            info: { owner in try self.info(owner) },
            window: { owner, request, _ in try self.window(owner, request) },
            retained: { owner, id in try self.restore(owner, id) },
            copy: { owner, id, start, end in try self.copy(owner, id, start, end) },
            cancel: { _ in true }, close: { self.close($0) }, retirementFailed: { self.failedRetirement() })
    }
    func block(_ offset: UInt64, on gate: ObservedReaderGate) { lock.lock(); gates[offset] = gate; lock.unlock() }
    func failOpen() { lock.lock(); failureOnNextOpen = true; lock.unlock() }
    func setCloseAllowed(_ value: Bool) { lock.lock(); closeAllowed = value; lock.unlock() }
    func setContent(_ value: String) { lock.lock(); content = value; lock.unlock() }
    func released(_ label: String) { lock.lock(); released.insert(label); lock.unlock() }
    func hasReleased(_ label: String) -> Bool { lock.lock(); defer { lock.unlock() }; return released.contains(label) }
    func evict(_ owner: UInt64, _ id: UInt64) { lock.lock(); pages[owner]?.removeValue(forKey: id); lock.unlock() }
    var windowOffsets: [UInt64] { lock.lock(); defer { lock.unlock() }; return requests }
    var maximumActive: Int { lock.lock(); defer { lock.unlock() }; return mostActive }
    var createCount: Int { lock.lock(); defer { lock.unlock() }; return creates }
    var openCount: Int { lock.lock(); defer { lock.unlock() }; return opens }
    var copyCount: Int { lock.lock(); defer { lock.unlock() }; return copies }
    var restoreCount: Int { lock.lock(); defer { lock.unlock() }; return restores }
    var closedCount: Int { lock.lock(); defer { lock.unlock() }; return closed.count }
    var failedCloseOwners: Int { lock.lock(); defer { lock.unlock() }; return closes.values.filter { $0 >= 8 }.count }
    var retirementFailures: Int { lock.lock(); defer { lock.unlock() }; return failures }

    private func create() -> UInt64 {
        lock.lock(); defer { lock.unlock() }
        creates += 1; nextOwner += 1; return nextOwner
    }
    private func open(_ owner: UInt64, _ input: AtlasObservedReaderInput) throws -> String? {
        lock.lock(); defer { lock.unlock() }
        opens += 1
        if failureOnNextOpen {
            failureOnNextOpen = false
            return try ObservedReaderFixture.failure(owner, "OBSERVED_READER_INVALID_PATH")
        }
        inputs[owner] = input; ids[owner] = 0; pages[owner] = [:]
        return try ObservedReaderFixture.encode(ObservedReaderFixture.info(owner, input))
    }
    private func info(_ owner: UInt64) throws -> String? {
        lock.lock(); defer { lock.unlock() }
        guard let input = inputs[owner] else { return nil }
        return try ObservedReaderFixture.encode(ObservedReaderFixture.info(owner, input))
    }
    private func window(_ owner: UInt64, _ request: AtlasObservedReaderRequest) throws -> String? {
        lock.lock()
        guard let input = inputs[owner] else { lock.unlock(); return nil }
        requests.append(request.offset); active += 1; mostActive = max(mostActive, active)
        let id = (ids[owner] ?? 0) + 1; ids[owner] = id
        let gate = gates.removeValue(forKey: request.offset), text = content
        lock.unlock()
        gate?.wait() // Intentionally returns a valid late result even after cancellation.
        lock.lock(); defer { active -= 1; lock.unlock() }
        let page = ObservedReaderFixture.page(owner, input, id, request, text: text)
        pages[owner, default: [:]][id] = page
        if let keys = pages[owner]?.keys.sorted(), keys.count > 8, let first = keys.first { pages[owner]?.removeValue(forKey: first) }
        return try ObservedReaderFixture.encode(page)
    }
    private func restore(_ owner: UInt64, _ id: UInt64) throws -> String? {
        lock.lock(); defer { lock.unlock() }; restores += 1
        guard let page = pages[owner]?[id] else {
            return try ObservedReaderFixture.failure(owner, "OBSERVED_READER_OBSERVATION_UNAVAILABLE")
        }
        return try ObservedReaderFixture.encode(ObservedReaderFixture.retained(page))
    }
    private func copy(_ owner: UInt64, _ id: UInt64, _ start: UInt64, _ end: UInt64) throws -> String? {
        lock.lock(); defer { lock.unlock() }; copies += 1
        guard let page = pages[owner]?[id],
              let range = page["visible_range"] as? [String: String],
              range["start"] == String(start), range["end"] == String(end) else {
            return try ObservedReaderFixture.failure(owner, "OBSERVED_READER_OBSERVATION_UNAVAILABLE")
        }
        return try ObservedReaderFixture.encode(ObservedReaderFixture.copy(page))
    }
    private func close(_ owner: UInt64) -> Bool {
        lock.lock(); defer { lock.unlock() }
        closes[owner, default: 0] += 1
        guard closeAllowed else { return false }
        closed.insert(owner); inputs.removeValue(forKey: owner); pages.removeValue(forKey: owner); ids.removeValue(forKey: owner)
        return true
    }
    private func failedRetirement() { lock.lock(); failures += 1; lock.unlock() }
}

final class ObservedReaderGrant {
    let probe: ObservedReaderProbe
    let label: String
    init(_ probe: ObservedReaderProbe, _ label: String) { self.probe = probe; self.label = label }
    deinit { probe.released(label) }
}
