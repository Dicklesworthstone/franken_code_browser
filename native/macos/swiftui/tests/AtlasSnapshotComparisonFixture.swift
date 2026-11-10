// Deterministic receipts exercise production native state and admission.
// This does not implement archive parsing, comparison or native rendering.
import Foundation
import Dispatch

final class SnapshotComparisonFixture: @unchecked Sendable {
    static let shared: UInt64 = 300
    static let total = shared + 2
    let domain = UUID()
    private struct State {
        var before = "/before.fcbs", after = "/after.fcbs"
        var generation: UInt64 = 0, examined: UInt64 = 0, steps: UInt64 = 0, last: UInt64 = 0
        var finished = false
        var beforeBytes: UInt64 = 0, beforeCalls: UInt64 = 0, afterBytes: UInt64 = 0, afterCalls: UInt64 = 0
    }
    private let lock = NSLock()
    private var states: [UInt64: State] = [:]
    private var readers: [UInt64: (UInt64, AtlasSnapshotComparisonSide)] = [:]
    private var nextOwner: UInt64 = 9_007_199_254_741_000
    private var nextReader: UInt64 = 9_007_199_254_745_000
    private var events: [String] = []
    private var paused: String?
    private var entered = false
    private var mutation: @Sendable (String, [String: Any]) -> [String: Any] = { _, value in value }
    let gate = DispatchSemaphore(value: 0)
    var log: [String] { lock.lock(); defer { lock.unlock() }; return events }
    var isBlocked: Bool { lock.lock(); defer { lock.unlock() }; return entered }
    func pause(_ phase: String) { lock.lock(); paused = phase; entered = false; lock.unlock() }
    func patch(_ body: @escaping @Sendable (String, [String: Any]) -> [String: Any]) {
        lock.lock(); mutation = body; lock.unlock()
    }
    static func json(_ value: [String: Any]) -> String {
        String(decoding: try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self)
    }
    static func hex(_ bytes: some Sequence<UInt8>) -> String { bytes.map { String(format: "%02x", $0) }.joined() }
    static func path(_ value: String) -> [String: String] {
        ["encoding": "unix-bytes", "hex": hex(value.utf8), "display": value]
    }
    static func name(_ entry: UInt64) -> String {
        entry <= shared ? String(format: "file-%04llu.rs", entry)
            : (entry == shared + 1 ? "only-after.rs" : "only-before.rs")
    }
    static func source(_ entry: UInt64, _ side: AtlasSnapshotComparisonSide) -> String {
        entry == 1 ? "same\n" : side == .before ? "old\n" : "new\n"
    }
    static func digest(_ entry: UInt64, _ side: AtlasSnapshotComparisonSide) -> String {
        String(repeating: entry == 1 ? "c" : side == .before ? "a" : "b", count: 64)
    }
    private static func archive(_ side: AtlasSnapshotComparisonSide, state: State) -> [String: Any] {
        ["digest": String(repeating: side == .before ? "d" : "e", count: 64),
         "path": path(side == .before ? state.before : state.after), "policy": "fixture-v1",
         "members_total": String(shared + 1), "captured_members": String(side == .before ? shared : shared + 1),
         "discovery_complete": true, "source_bytes": String(side == .before ? (shared - 1) * 4 + 5 : shared * 4 + 5),
         "archive_bytes": "8192", "initial_archive_bytes_read": "8192", "initial_archive_read_calls": "2",
         "member_bytes_read": String(side == .before ? state.beforeBytes : state.afterBytes),
         "member_read_calls": String(side == .before ? state.beforeCalls : state.afterCalls)]
    }
    private static func common(owner: UInt64, command: String, state: State) -> [String: Any] {
        ["schema": "fcb.snapshot-comparison/1", "status": "ok", "command": command, "owner": String(owner),
         "before_archive": archive(.before, state: state), "after_archive": archive(.after, state: state),
         "live_source_reopened": false, "live_source_bytes_read": "0", "history_inferred": false,
         "renames_inferred": false, "native_presented": false,
         "alignment": "native-path-pair-not-logical-file-continuity",
         "last_comparison_attempt": String(state.generation),
         "accepted_comparison_generation": state.finished ? String(state.generation) as Any : NSNull(),
         "pending_comparison_generation": state.generation > 0 && !state.finished ? String(state.generation) as Any : NSNull(),
         "max_members_per_archive": "65536", "max_path_bytes": "2048", "max_source_file_bytes": "1048576",
         "max_archive_bytes": "83886080", "max_retained_entries": "131072", "max_response_bytes": "4194304"]
    }
    static func info(owner: UInt64, before: String = "/before.fcbs", after: String = "/after.fcbs") -> [String: Any] {
        var state = State(); state.before = before; state.after = after
        return common(owner: owner, command: "info", state: state)
    }
    static func member(entry: UInt64, side: AtlasSnapshotComparisonSide) -> [String: Any]? {
        if entry == shared + 1 && side == .before || entry == shared + 2 && side == .after { return nil }
        let ordinal = entry <= shared ? entry - 1 : shared
        let file = ordinal + (side == .before ? 1 : 65_537)
        let captured = entry != shared || side != .before
        return ["member": String(ordinal), "file_id": String(file), "source_revision": String(file),
            "observed_bytes": captured ? String(source(entry, side).utf8.count) : String(UInt64.max),
            "captured": captured, "captured_bytes": captured ? String(source(entry, side).utf8.count) as Any : NSNull(),
            "source_digest": captured ? digest(entry, side) as Any : NSNull(),
            "unavailable_reason": captured ? NSNull() : "SOURCE_UNAVAILABLE"]
    }
    static func row(_ entry: UInt64) -> [String: Any] {
        let kind = entry == 1 ? "unchanged" : entry == shared ? "unavailable"
            : entry == shared + 1 ? "added" : entry == shared + 2 ? "removed" : "changed"
        return ["entry_id": String(entry), "path": path(name(entry)), "kind": kind,
            "before": member(entry: entry, side: .before) as Any? ?? NSNull(),
            "after": member(entry: entry, side: .after) as Any? ?? NSNull()]
    }
    private static func page(owner: UInt64, command: String, state: State,
        filter: AtlasSnapshotComparisonFilter = .differences, start: UInt64 = 0, limit: UInt64 = 64) -> [String: Any] {
        var value = common(owner: owner, command: command, state: state)
        let rows = state.examined == 0 ? [] : (1...state.examined).map(Self.row)
        var counts: [String: UInt64] = [:]
        for row in rows { counts[row["kind"] as! String, default: 0] += 1 }
        let unchanged = counts["unchanged", default: 0], changed = counts["changed", default: 0]
        let added = counts["added", default: 0], removed = counts["removed", default: 0]
        let uncertain = counts["unavailable", default: 0]
        let selected = rows.filter { filter.includes(AtlasSnapshotComparisonKind(rawValue: $0["kind"] as! String)!) }
        let total = UInt64(selected.count), end = min(start + limit, total)
        precondition(start <= total)
        value.merge([
            "comparison_generation": String(state.generation), "comparison_in_progress": !state.finished,
            "scan_complete": state.finished, "comparison_complete": state.finished && uncertain == 0,
            "policies_match": true, "membership_comparable": true, "paths_upper_bound": String((shared + 1) * 2),
            "compared_paths": String(state.examined), "unchanged": String(unchanged), "changed": String(changed),
            "added": String(added), "removed": String(removed), "only_before": "0", "only_after": "0",
            "unavailable": String(uncertain), "known_differences": String(changed + added + removed),
            "uncertain_paths": String(uncertain), "retained_entries": String(state.examined),
            "steps": String(state.steps), "last_step_entries": String(state.last), "max_step_entries": "1024",
            "filter": filter.rawValue, "offset": String(start), "limit": String(limit), "matching_entries": String(total),
            "entries": Array(selected[Int(start)..<Int(end)]),
            "next_offset": end < total ? String(end) as Any : NSNull(), "page_limited": false
        ], uniquingKeysWith: { _, new in new })
        return value
    }
    static func page(owner: UInt64, generation: UInt64 = 1, filter: AtlasSnapshotComparisonFilter = .differences,
        start: UInt64 = 0, limit: UInt64 = 64) -> [String: Any] {
        var state = State(); state.generation = generation; state.examined = total
        state.steps = 2; state.last = total - 256; state.finished = true
        return page(owner: owner, command: "page", state: state, filter: filter, start: start, limit: limit)
    }
    static func readerInfo(_ owner: UInt64, entry: UInt64, side: AtlasSnapshotComparisonSide) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": "info", "owner": String(owner),
         "file_id": "1", "source_revision": "1", "captured_bytes": String(source(entry, side).utf8.count),
         "encoding": "utf8", "path": path(name(entry)), "capture_origin": "host-supplied",
         "capture_sha256": digest(entry, side), "initial_source_bytes_read": "0", "initial_read_calls": "0",
         "additional_source_bytes_read": "0", "native_presented": false, "accepted_query_generation": NSNull(),
         "accepted_outline_generation": NSNull(), "accepted_document_generation": NSNull(), "accepted_reference_generation": NSNull()]
    }
    private func reply(_ phase: String, _ value: [String: Any]) -> String {
        lock.lock()
        events.append(phase)
        let shouldPause = paused == phase
        if shouldPause { paused = nil; entered = true }
        let mutation = mutation
        lock.unlock()
        if shouldPause { precondition(gate.wait(timeout: .now() + 10) == .success, "Fixture gate was not released") }
        return Self.json(mutation(phase, value))
    }
    var transport: AtlasSnapshotComparisonTransport {
        .init(create: {
            self.lock.lock(); defer { self.lock.unlock() }; self.nextOwner += 1
            self.states[self.nextOwner] = State(); self.events.append("create")
            return self.nextOwner
        }, open: { owner, before, after, _ in
            self.lock.lock()
            var state = self.states[owner]!; state.before = before; state.after = after; self.states[owner] = state
            self.lock.unlock()
            return self.reply("open", Self.common(owner: owner, command: "info", state: state))
        }, info: { owner in
            self.lock.lock(); let state = self.states[owner]!; self.lock.unlock()
            return self.reply("info", Self.common(owner: owner, command: "info", state: state))
        }, begin: { owner, generation, _ in
            self.lock.lock(); var state = self.states[owner]!
            state.generation = generation; state.examined = 0; state.steps = 0; state.last = 0; state.finished = false
            self.states[owner] = state; self.lock.unlock()
            return self.reply("begin", Self.page(owner: owner, command: "begin", state: state))
        }, step: { owner, generation, maximum, _ in
            self.lock.lock(); var state = self.states[owner]!
            precondition(state.generation == generation && !state.finished)
            state.last = min(maximum, Self.total - state.examined); state.examined += state.last; state.steps += 1
            state.finished = state.last < maximum
            self.states[owner] = state; self.lock.unlock()
            return self.reply("step", Self.page(owner: owner, command: "step", state: state))
        }, page: { owner, generation, filter, start, limit in
            self.lock.lock(); let state = self.states[owner]!; self.lock.unlock()
            precondition(state.finished && state.generation == generation)
            return self.reply("page", Self.page(owner: owner, command: "page", state: state, filter: filter, start: start, limit: limit))
        }, openReader: { owner, reader, generation, entry, side in
            self.lock.lock(); var state = self.states[owner]!
            precondition(state.finished && state.generation == generation)
            let member = Self.member(entry: entry, side: side)!
            precondition(member["captured"] as! Bool)
            let count = UInt64(Self.source(entry, side).utf8.count)
            if side == .before { state.beforeBytes += count; state.beforeCalls += 1 }
            else { state.afterBytes += count; state.afterCalls += 1 }
            self.states[owner] = state; self.readers[reader] = (entry, side); self.lock.unlock()
            var value = Self.common(owner: owner, command: "open-reader", state: state)
            value.merge([
                "comparison_generation": String(generation), "entry_id": String(entry), "side": side.rawValue,
                "member": member["member"]!, "file_id": member["file_id"]!, "source_revision": member["source_revision"]!,
                "path": Self.path(Self.name(entry)), "archive_digest": String(repeating: side == .before ? "d" : "e", count: 64),
                "source_digest": Self.digest(entry, side), "captured_bytes": String(count),
                "reader_owner": String(reader), "source_observation": "verified-saved-member",
                "member_bytes_read": String(count), "member_read_calls": "1",
                "reader": Self.readerInfo(reader, entry: entry, side: side)
            ], uniquingKeysWith: { _, new in new })
            return self.reply("open-reader", value)
        }, cancel: { _ in self.lock.lock(); self.events.append("cancel"); self.lock.unlock(); return true },
        close: { owner in
            self.lock.lock(); self.states.removeValue(forKey: owner); self.events.append("close"); self.lock.unlock(); return true
        }, retirementFailed: { preconditionFailure("Fixture comparison retirement failed") }, readerDomain: domain)
    }

    var readerTransport: AtlasReaderTransport {
        .init(create: {
            self.lock.lock(); defer { self.lock.unlock() }; self.nextReader += 1
            self.events.append("create-reader"); return self.nextReader
        }, open: { _, _, _ in preconditionFailure("Saved comparison readers cannot reopen live files") },
        window: { owner, offset, limit in
            self.lock.lock(); let record = self.readers[owner]!; self.lock.unlock()
            let raw = Array(Self.source(record.0, record.1).utf8), end = min(UInt64(raw.count), offset + limit)
            var value = Self.readerInfo(owner, entry: record.0, side: record.1)
            value["command"] = "window"; value["text_kind"] = "logical-captured-text-not-shaped"
            value["requested_original_range"] = ["start": String(offset), "end": String(end)]
            value["visible_range"] = ["start": String(offset), "end": String(end)]
            value["range_limited"] = false; value["boundaries_adjusted"] = false; value["has_replacements"] = false
            value["first_physical_line"] = NSNull(); value["next_offset"] = end < UInt64(raw.count) ? String(end) as Any : NSNull()
            value["text"] = String(decoding: raw[Int(offset)..<Int(end)], as: UTF8.self)
            value["original_hex"] = Self.hex(raw[Int(offset)..<Int(end)]); value["selection"] = NSNull()
            return self.reply("window", value)
        }, lines: { _, _, _, _ in nil }, cancel: { _ in true }, close: { owner in
            self.lock.lock(); self.readers.removeValue(forKey: owner); self.events.append("close-reader"); self.lock.unlock(); return true
        }, retirementFailed: { preconditionFailure("Fixture reader retirement failed") },
        comparison: .init(domain: domain, compare: { _, _, _, _, _ in nil }))
    }
}
