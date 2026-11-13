// Scripted foreign receipts for production native lifecycle/decoder tests.
// This fixture does not parse Markdown, resolve paths or capture live files.
import Foundation
import Dispatch

final class ProjectLinksFixture: @unchecked Sendable {
    let index = IndexedFixture()
    let domain = UUID()
    let includesImports: Bool
    init(includesImports: Bool = false) {
        self.includesImports = includesImports
        if includesImports {
            index.configure { $0.files = 4 }
            index.patch { _, value in
                var value = value
                if let captured = value["captured_files"] as? String, let count = Int(captured), count >= 3 {
                    let bytes = count == 3 ? "8292" : "8392"
                    value["indexed_source_bytes"] = bytes; value["initial_source_bytes_read"] = bytes
                }
                return value
            }
        }
    }
    private let lock = NSLock()
    private var events: [String] = []
    private var generations: [UInt64: UInt64] = [:]
    private var readerFiles: [UInt64: UInt64] = [:]
    private var readerProjections: [UInt64: AtlasProjectRelationshipProjection] = [:]
    private var importGenerations: [UInt64: UInt64] = [:]
    private var importSteps: [UInt64: Int] = [:]
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
    static func source(_ file: UInt64, projection: AtlasProjectRelationshipProjection = .documentation) -> String {
        let text: String
        if projection == .imports {
            text = file == 1 ? "// imports\n" + String(repeating: "mod child;\n", count: 70) : "pub struct Child;\n"
        } else {
            text = file == 1 ? String(repeating: "[guide](guide.md#part)\n", count: 70)
                : String(repeating: "x", count: 32) + "\n\n## Part\n\nCaptured guidance.\n"
        }
        return text + String(repeating: " ", count: (file == 1 ? 4096 : 100) - text.utf8.count)
    }
    static func digest(_ file: UInt64) -> String { String(repeating: file == 1 ? "a" : "b", count: 64) }
    static func name(_ file: UInt64, projection: AtlasProjectRelationshipProjection = .documentation) -> String {
        projection == .imports ? (file == 1 ? "src/lib.rs" : "src/child.rs") : (file == 1 ? "docs.md" : "guide.md")
    }
    static func identity(owner: UInt64, generation: UInt64, command: String) -> [String: Any] {
        ["schema": "fcb.atlas-links/1", "status": "ok", "command": command, "owner": String(owner),
         "source_manifest": "1", "layout_revision": "3", "index_generation": "1", "capture_manifest": "2",
         "relationship_generation": String(generation)]
    }
    static func row(_ id: UInt64) -> [String: Any] {
        ["link_id": String(id), "source_file_id": "1", "source_revision": "1", "source_path": path("docs.md"),
         "source_sha256": digest(1), "source_bytes": "4096", "source_link_id": String(id), "block_index": "0",
         "evidence_range": ["start": "0", "end": "4096"], "destination": "guide.md#part", "resolution": "captured",
         "target_file_id": "2", "target_revision": "1", "target_path": path("guide.md"),
         "target_sha256": digest(2), "target_bytes": "100", "destination_suffix": "#part", "fragment_resolution": "unqualified"]
    }
    static func page(owner: UInt64, generation: UInt64, command: String, running: Bool = false,
        direction: AtlasProjectLinkDirection = .all, path: String? = nil, start: UInt64 = 0, limit: UInt64 = 64) -> [String: Any] {
        var result = identity(owner: owner, generation: generation, command: command)
        let retained: UInt64 = running ? 0 : 70
        let selected: [UInt64] = running ? [] : ((path == nil
            || (direction == .all && ["docs.md", "guide.md"].contains(path!))
            || (direction == .outgoing && path == "docs.md") || (direction == .incoming && path == "guide.md")) ? Array(1...70) : [])
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
    static func readerInfo(_ owner: UInt64, file: UInt64, projection: AtlasProjectRelationshipProjection = .documentation) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": "info", "owner": String(owner),
         "file_id": "1", "source_revision": "1", "captured_bytes": String(source(file, projection: projection).utf8.count), "encoding": "utf8",
         "path": path(name(file, projection: projection)), "capture_origin": "host-supplied", "capture_sha256": digest(file),
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
            "fragment_resolution": "unqualified", "fragment_suffix": side == .target ? "#part" : "",
            "fragment_status": side == .target ? "resolved" : "none", "fragment_slug": side == .target ? "part" : NSNull(),
            "target_range": side == .target ? ["start": "34", "end": "42"] : NSNull(),
            "initial_original_offset": side == .target ? "34" : "0"], uniquingKeysWith: { _, value in value })
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
            let names = self.includesImports ? ["docs.md", "guide.md", "src/lib.rs", "src/child.rs"] : ["docs.md", "guide.md"]
            let selected = names.first { Self.hex($0.utf8) == hex }
            if hex != nil && selected == nil {
                return self.call("page", ["schema": "fcb.atlas-links/1", "status": "error", "error": ["code": "ATLAS_LINKS_PATH_NOT_FOUND"]])
            }
            var page = Self.page(owner: owner, generation: generation, command: "links-page",
                direction: direction, path: selected, start: start, limit: limit)
            if let selected, let at = names.firstIndex(of: selected) { page["filter_file_id"] = String(at + 1) }
            return self.call("page", page)
        }, openReader: { owner, reader, generation, link, side in
            self.lock.lock(); self.readerFiles[reader] = side == .evidence ? 1 : 2; self.lock.unlock()
            return self.call("reader", Self.receipt(owner: owner, reader: reader, generation: generation, link: link, side: side))
        }, imports: includesImports ? importOperations : nil)
    }
    var readerTransport: AtlasReaderTransport {
        .init(create: {
            self.lock.lock(); defer { self.lock.unlock() }; self.readerNext += 1
            self.events.append("create-reader"); return self.readerNext
        }, open: { _, _, _ in preconditionFailure("Retained documentation readers must never reopen live files") },
        window: { owner, offset, count in
            self.lock.lock(); let file = self.readerFiles[owner]
            let projection = self.readerProjections[owner] ?? .documentation; self.lock.unlock()
            guard let file else { return nil }
            let bytes = Array(Self.source(file, projection: projection).utf8), end = min(UInt64(bytes.count), offset + count)
            precondition(offset <= end)
            let raw = bytes[Int(offset)..<Int(end)]
            var value = Self.readerInfo(owner, file: file, projection: projection)
            value["command"] = "window"; value["text_kind"] = "logical-captured-text-not-shaped"
            value["requested_original_range"] = ["start": String(offset), "end": String(end)]
            value["visible_range"] = ["start": String(offset), "end": String(end)]
            value["range_limited"] = false; value["boundaries_adjusted"] = false; value["has_replacements"] = false
            value["first_physical_line"] = NSNull(); value["next_offset"] = end < UInt64(bytes.count) ? String(end) as Any : NSNull()
            value["text"] = String(decoding: raw, as: UTF8.self); value["original_hex"] = Self.hex(raw); value["selection"] = NSNull()
            return self.call("window", value)
        }, lines: { _, _, _, _ in nil }, cancel: { _ in true }, close: { owner in
            self.lock.lock(); defer { self.lock.unlock() }
            self.readerFiles.removeValue(forKey: owner); self.readerProjections.removeValue(forKey: owner)
            self.events.append("close-reader"); return true
        }, retirementFailed: { preconditionFailure("Unexpected fixture retirement failure") },
        comparison: .init(domain: domain, compare: { _, _, _, _, _ in nil }))
    }
}


// Scripted lexical evidence exercises the shared production native lifecycle.
// The Rust extractor and catalog resolver have their own real-source tests.
extension ProjectLinksFixture {
    static var importProof: [String: Any] {
        ["projection": "lexical-source-imports", "relationship_kind": "lexical-import",
         "evidence_precision": "exact-import-source-range", "extractor": "franken-markdown-d6fbd1d5/import-statements-v3",
         "resolver": "fcb-closed-catalog-imports-v3", "extraction_policy": "rust-use-tree-js-static-and-python-import-v3",
         "compiler_resolved": false, "semantic_complete": false]
    }
    static func importRow(_ id: UInt64) -> [String: Any] {
        var value = row(id)
        value["source_file_id"] = "3"; value["target_file_id"] = "4"
        value["source_path"] = path(name(1, projection: .imports))
        value["target_path"] = path(name(2, projection: .imports))
        value["destination"] = "child"
        value["evidence_range"] = ["start": String(11 + (id - 1) * 11), "end": String(21 + (id - 1) * 11)]
        value["language"] = "rust"; value["lexical_import_kind"] = "rust-module"
        value["resolution_basis"] = "rust-module-file-candidate"
        for key in ["block_index", "destination_suffix", "fragment_resolution"] { value.removeValue(forKey: key) }
        return value
    }
    static func importPage(owner: UInt64, generation: UInt64, command: String, examined: Int = 2,
        direction: AtlasProjectLinkDirection = .all, path: String? = nil, start: UInt64 = 0, limit: UInt64 = 64) -> [String: Any] {
        var value = identity(owner: owner, generation: generation, command: command)
        value["schema"] = "fcb.atlas-imports/1"
        value.merge(importProof, uniquingKeysWith: { _, value in value })
        let retained: UInt64 = examined == 0 ? 0 : 70
        let selected: [UInt64] = retained == 0 ? [] : ((path == nil
            || (direction == .all && ["src/lib.rs", "src/child.rs"].contains(path!))
            || (direction == .outgoing && path == "src/lib.rs") || (direction == .incoming && path == "src/child.rs"))
            ? Array(1...70) : [])
        let total = UInt64(selected.count), end = min(total, start + limit)
        value.merge([
            "source_observation": "retained-index-captures", "max_links": "4096", "max_parse_bytes": "33554432",
            "max_source_bytes": "131072", "max_metadata_bytes": "8388608",
            "metadata_bytes": examined == 0 ? "0" : "35000", "source_bytes_read": "0", "read_calls": "0",
            "parsed_source_bytes": examined == 0 ? "0" : (examined == 1 ? "4096" : "4196"),
            "hashed_source_bytes": examined == 0 ? "0" : "4196", "capture_region_complete": true,
            "source_files": "2", "captured_source_files": "2", "capture_unavailable_source_files": "0",
            "outside_capture_scope_source_files": "0", "uncaptured_source_files": "0",
            "examined_source_files": String(examined), "pending_source_files": String(2 - examined),
            "scanned_source_files": String(examined), "unsupported_source_files": "0", "source_limited_source_files": "0",
            "parser_diagnostics": "0", "links_seen": String(retained), "retained_links": String(retained),
            "steps": String(examined), "last_step_files": examined == 0 ? "0" : "1",
            "relationships_in_progress": examined < 2, "relationships_complete": examined == 2, "discovery_complete": true,
            "truncated": false, "stop_reason": examined < 2 ? NSNull() : "all-source-files-examined",
            "direction": direction.rawValue, "filter_path": path == nil ? NSNull() : Self.path(path!),
            "filter_file_id": path == nil ? NSNull() : (path == "src/lib.rs" ? "3" : "4"),
            "offset": String(start), "limit": String(limit), "matching_links": String(total),
            "links": start < end ? selected[Int(start)..<Int(end)].map(Self.importRow) : [],
            "next_offset": end < total ? String(end) as Any : NSNull()
        ], uniquingKeysWith: { _, value in value })
        return value
    }
    static func importReceipt(owner: UInt64, reader: UInt64, generation: UInt64, link: UInt64,
        side: AtlasProjectLinkSide) -> [String: Any] {
        let file: UInt64 = side == .evidence ? 1 : 2
        let row = importRow(link)
        var value = identity(owner: owner, generation: generation, command: "imports-open-reader")
        value["schema"] = "fcb.atlas-imports/1"
        value.merge(importProof, uniquingKeysWith: { _, value in value })
        value.merge([
            "link_id": String(link), "side": side.rawValue, "file_id": String(file + 2), "source_revision": "1",
            "path": path(name(file, projection: .imports)), "source_sha256": digest(file),
            "source_bytes": String(source(file, projection: .imports).utf8.count),
            "source_observation": "retained-index-capture", "source_reopened": false, "additional_source_bytes_read": "0",
            "reader_owner": String(reader), "reader": readerInfo(reader, file: file, projection: .imports),
            "relationship": row, "evidence_range": row["evidence_range"]!,
            "initial_original_offset": side == .evidence ? String(11 + (link - 1) * 11) : "0"
        ], uniquingKeysWith: { _, value in value })
        return value
    }
    var importOperations: AtlasProjectLinkOperations {
        .init(begin: { owner, generation, index, _ in
            precondition(index == 1)
            self.lock.lock()
            let previous = self.importGenerations[owner] ?? 0
            precondition(generation > previous)
            self.importGenerations[owner] = generation; self.importSteps[owner] = 0
            self.lock.unlock()
            return self.call("imports-begin", Self.importPage(owner: owner, generation: generation,
                command: "imports-begin", examined: 0))
        }, step: { owner, generation, _ in
            self.lock.lock()
            precondition(self.importGenerations[owner] == generation)
            let next = (self.importSteps[owner] ?? 0) + 1
            self.importSteps[owner] = next
            self.lock.unlock()
            return self.call("imports-step", Self.importPage(owner: owner, generation: generation,
                command: "imports-step", examined: next))
        }, page: { owner, generation, direction, hex, start, limit in
            let names = ["docs.md", "guide.md", "src/lib.rs", "src/child.rs"]
            let selected = names.first { Self.hex($0.utf8) == hex }
            if hex != nil && selected == nil {
                return self.call("imports-page", ["schema": "fcb.atlas-imports/1", "status": "error",
                    "error": ["code": "ATLAS_IMPORTS_PATH_NOT_FOUND"]])
            }
            var page = Self.importPage(owner: owner, generation: generation, command: "imports-page",
                direction: direction, path: selected, start: start, limit: limit)
            if let selected, let at = names.firstIndex(of: selected) { page["filter_file_id"] = String(at + 1) }
            return self.call("imports-page", page)
        }, openReader: { owner, reader, generation, link, side in
            self.lock.lock()
            self.readerFiles[reader] = side == .evidence ? 1 : 2
            self.readerProjections[reader] = .imports
            self.lock.unlock()
            return self.call("imports-reader", Self.importReceipt(owner: owner, reader: reader,
                generation: generation, link: link, side: side))
        })
    }
}
