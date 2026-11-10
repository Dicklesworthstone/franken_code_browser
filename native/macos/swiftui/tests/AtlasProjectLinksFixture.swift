// Scripted foreign receipts for production native lifecycle/decoder tests.
// This fixture does not parse Markdown, resolve paths or capture live files.
import Foundation
import Dispatch

final class ProjectLinksFixture: @unchecked Sendable {
    let index = IndexedFixture()
    let domain = UUID()
    private let lock = NSLock()
    private var events: [String] = []
    private var generations: [UInt64: UInt64] = [:]
    private var readerFiles: [UInt64: UInt64] = [:]
    private var readerNext: UInt64 = 100
    private var paused: String?
    private var entered = false
    private var mutation: @Sendable (String, [String: Any]) -> [String: Any] = { _, value in value }
    let gate = DispatchSemaphore(value: 0)
    var log: [String] { lock.lock(); defer { lock.unlock() }; return events }
    var isBlocked: Bool { lock.lock(); defer { lock.unlock() }; return entered }
    func pause(_ phase: String) { lock.lock(); paused = phase; entered = false; lock.unlock() }
    func patch(_ mutate: @escaping @Sendable (String, [String: Any]) -> [String: Any]) {
        lock.lock(); mutation = mutate; lock.unlock()
    }
    static func json(_ value: [String: Any]) -> String {
        String(decoding: try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self)
    }
    static func hex(_ bytes: some Sequence<UInt8>) -> String { bytes.map { String(format: "%02x", $0) }.joined() }
    static func path(_ path: String) -> [String: String] {
        ["encoding": "unix-bytes", "hex": hex(path.utf8), "display": path]
    }
    static func source(_ file: UInt64) -> String {
        let text = file == 1 ? String(repeating: "[code](src.rs#part)\n", count: 70) : "fn captured() {}\n"
        return text + String(repeating: " ", count: (file == 1 ? 4096 : 100) - text.utf8.count)
    }
    static func digest(_ file: UInt64) -> String { String(repeating: file == 1 ? "a" : "b", count: 64) }
    static func name(_ file: UInt64) -> String { file == 1 ? "docs.md" : "src.rs" }
    static func identity(owner: UInt64, generation: UInt64, command: String) -> [String: Any] {
        ["schema": "fcb.atlas-links/1", "status": "ok", "command": command, "owner": String(owner),
         "source_manifest": "1", "layout_revision": "3", "index_generation": "1", "capture_manifest": "2",
         "relationship_generation": String(generation)]
    }
    static func row(_ id: UInt64) -> [String: Any] {
        ["link_id": String(id), "source_file_id": "1", "source_revision": "1", "source_path": path("docs.md"),
         "source_sha256": digest(1), "source_bytes": "4096", "source_link_id": String(id), "block_index": "0",
         "evidence_range": ["start": "0", "end": "4096"], "destination": "src.rs#part", "resolution": "captured",
         "target_file_id": "2", "target_revision": "1", "target_path": path("src.rs"),
         "target_sha256": digest(2), "target_bytes": "100", "destination_suffix": "#part", "fragment_resolution": "unqualified"]
    }
    static func page(owner: UInt64, generation: UInt64, command: String, running: Bool = false,
        direction: AtlasProjectLinkDirection = .all, path: String? = nil, start: UInt64 = 0, limit: UInt64 = 64) -> [String: Any] {
        var result = identity(owner: owner, generation: generation, command: command)
        let retained: UInt64 = running ? 0 : 70
        let selected: [UInt64] = running ? [] : ((path == nil || direction == .all
            || (direction == .outgoing && path == "docs.md") || (direction == .incoming && path == "src.rs")) ? Array(1...70) : [])
        let total = UInt64(selected.count), end = min(total, start + limit)
        result.merge([
            "source_observation": "retained-index-captures", "evidence_precision": "whole-captured-markdown-document",
            "relationship_kind": "parsed-markdown-link", "resolution_domain": "exact-catalog-relative-path",
            "max_links": "4096", "max_parse_bytes": "33554432", "max_markdown_source_bytes": "262144",
            "max_metadata_bytes": "8388608", "metadata_bytes": running ? "0" : "35000",
            "source_bytes_read": "0", "read_calls": "0", "parsed_source_bytes": running ? "0" : "4096",
            "hashed_source_bytes": running ? "0" : "4196", "capture_region_complete": true,
            "markdown_files": "1", "captured_markdown_files": "1", "capture_unavailable_markdown_files": "0",
            "outside_capture_scope_markdown_files": "0", "uncaptured_markdown_files": "0",
            "examined_markdown_files": running ? "0" : "1", "pending_markdown_files": running ? "1" : "0",
            "scanned_markdown_files": running ? "0" : "1", "unsupported_markdown_files": "0", "source_limited_markdown_files": "0",
            "parser_diagnostics": "0", "links_seen": String(retained), "retained_links": String(retained),
            "steps": running ? "0" : "1", "last_step_files": running ? "0" : "1",
            "relationships_in_progress": running, "relationships_complete": !running, "discovery_complete": true,
            "truncated": false, "stop_reason": running ? NSNull() : "all-markdown-examined",
            "direction": direction.rawValue,
            "filter_path": path == nil ? NSNull() : Self.path(path!),
            "filter_file_id": path == nil ? NSNull() : (path == "docs.md" ? "1" : "2"),
            "offset": String(start), "limit": String(limit), "matching_links": String(total),
            "links": start < end ? selected[Int(start)..<Int(end)].map(Self.row) : [],
            "next_offset": end < total ? String(end) as Any : NSNull()
        ], uniquingKeysWith: { _, value in value })
        return result
    }
    static func readerInfo(_ owner: UInt64, file: UInt64) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": "info", "owner": String(owner),
         "file_id": "1", "source_revision": "1", "captured_bytes": String(source(file).utf8.count), "encoding": "utf8",
         "path": path(name(file)), "capture_origin": "host-supplied", "capture_sha256": digest(file),
         "initial_source_bytes_read": "0", "initial_read_calls": "0", "additional_source_bytes_read": "0",
         "native_presented": false, "accepted_query_generation": NSNull(), "accepted_outline_generation": NSNull(),
         "accepted_document_generation": NSNull(), "accepted_reference_generation": NSNull()]
    }
    static func receipt(owner: UInt64, reader: UInt64, generation: UInt64, link: UInt64, side: AtlasProjectLinkSide) -> [String: Any] {
        let file: UInt64 = side == .evidence ? 1 : 2
        var value = identity(owner: owner, generation: generation, command: "links-open-reader")
        value.merge(["link_id": String(link), "side": side.rawValue, "file_id": String(file), "source_revision": "1",
            "path": path(name(file)), "source_sha256": digest(file), "source_bytes": String(source(file).utf8.count),
            "source_observation": "retained-index-capture", "source_reopened": false, "additional_source_bytes_read": "0",
            "reader_owner": String(reader), "reader": readerInfo(reader, file: file),
            "evidence_precision": "whole-captured-markdown-document", "destination_suffix": "#part",
            "fragment_resolution": "unqualified"], uniquingKeysWith: { _, value in value })
        return value
    }
    private func call(_ phase: String, _ value: [String: Any]) -> String {
        precondition(!Thread.isMainThread)
        lock.lock(); events.append(phase); let mutate = mutation
        let wait = paused == phase
        if wait { paused = nil; entered = true }
        lock.unlock()
        if wait { precondition(gate.wait(timeout: .now() + 10) == .success) }
        return Self.json(mutate(phase, value))
    }
    var transport: AtlasProjectLinksTransport {
        .init(index: index.transport, readerDomain: domain, begin: { owner, generation, index, _ in
            precondition(index == 1)
            self.lock.lock(); let previous = self.generations[owner] ?? 0
            precondition(generation > previous); self.generations[owner] = generation; self.lock.unlock()
            return self.call("begin", Self.page(owner: owner, generation: generation, command: "links-begin", running: true))
        }, step: { owner, generation, _ in
            self.call("step", Self.page(owner: owner, generation: generation, command: "links-step"))
        }, page: { owner, generation, direction, hex, start, limit in
            let selected = ["docs.md", "src.rs"].first { Self.hex($0.utf8) == hex }
            if hex != nil && selected == nil {
                return self.call("page", ["schema": "fcb.atlas-links/1", "status": "error", "error": ["code": "ATLAS_LINKS_PATH_NOT_FOUND"]])
            }
            return self.call("page", Self.page(owner: owner, generation: generation, command: "links-page",
                direction: direction, path: selected, start: start, limit: limit))
        }, openReader: { owner, reader, generation, link, side in
            self.lock.lock(); self.readerFiles[reader] = side == .evidence ? 1 : 2; self.lock.unlock()
            return self.call("reader", Self.receipt(owner: owner, reader: reader, generation: generation, link: link, side: side))
        })
    }
    var readerTransport: AtlasReaderTransport {
        .init(create: {
            self.lock.lock(); defer { self.lock.unlock() }; self.readerNext += 1
            self.events.append("create-reader"); return self.readerNext
        }, open: { _, _, _ in preconditionFailure("Retained documentation readers must never reopen live files") },
        window: { owner, offset, count in
            self.lock.lock(); let file = self.readerFiles[owner]; self.lock.unlock()
            guard let file else { return nil }
            let bytes = Array(Self.source(file).utf8), end = min(UInt64(bytes.count), offset + count)
            precondition(offset <= end)
            let raw = bytes[Int(offset)..<Int(end)]
            var value = Self.readerInfo(owner, file: file)
            value["command"] = "window"; value["text_kind"] = "logical-captured-text-not-shaped"
            value["requested_original_range"] = ["start": String(offset), "end": String(end)]
            value["visible_range"] = ["start": String(offset), "end": String(end)]
            value["range_limited"] = false; value["boundaries_adjusted"] = false; value["has_replacements"] = false
            value["first_physical_line"] = NSNull(); value["next_offset"] = end < UInt64(bytes.count) ? String(end) as Any : NSNull()
            value["text"] = String(decoding: raw, as: UTF8.self); value["original_hex"] = Self.hex(raw); value["selection"] = NSNull()
            return self.call("window", value)
        }, lines: { _, _, _, _ in nil }, cancel: { _ in true }, close: { owner in
            self.lock.lock(); defer { self.lock.unlock() }
            self.readerFiles.removeValue(forKey: owner); self.events.append("close-reader"); return true
        }, retirementFailed: { preconditionFailure("Unexpected fixture retirement failure") },
        comparison: .init(domain: domain, compare: { _, _, _, _, _ in nil }))
    }
}
