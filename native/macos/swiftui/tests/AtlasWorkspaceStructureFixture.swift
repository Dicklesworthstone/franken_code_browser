// Deterministic bridge receipts exercise production admission and lifecycle.
// They do not implement file discovery, source extraction, hashing or rendering.
import Foundation
import Dispatch

final class WorkspaceStructureFixture: @unchecked Sendable {
    let domain = UUID()
    let canonicalRoot: [UInt8]
    private struct State {
        let root: [UInt8]
        let generation: UInt64
        let request: AtlasWorkspaceStructureRequest
        var revision: UInt64 = 1
        var round = 0
        var stopped = false
    }
    private struct Reader {
        let request: AtlasWorkspaceStructureRequest
        let path: [UInt8]
    }
    private let lock = NSLock()
    private var states: [UInt64: State] = [:]
    private var readers: [UInt64: Reader] = [:]
    private var nextOwner: UInt64 = 9_007_199_254_741_000
    private var nextReader: UInt64 = 9_007_199_254_745_000
    private var events: [String] = []
    private var paused: String?
    private var entered = false
    private var mutation: @Sendable (String, [String: Any]) -> [String: Any] = { _, value in value }
    let gate = DispatchSemaphore(value: 0)

    init(canonicalRoot: [UInt8] = Array("/private/tmp/project".utf8)) { self.canonicalRoot = canonicalRoot }
    var log: [String] { lock.lock(); defer { lock.unlock() }; return events }
    var isBlocked: Bool { lock.lock(); defer { lock.unlock() }; return entered }
    var liveReaders: Int { lock.lock(); defer { lock.unlock() }; return readers.count }
    func pause(_ phase: String) { lock.lock(); paused = phase; entered = false; lock.unlock() }
    func patch(_ body: @escaping @Sendable (String, [String: Any]) -> [String: Any]) {
        lock.lock(); mutation = body; lock.unlock()
    }
    static func json(_ value: [String: Any]) -> String {
        String(decoding: try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self)
    }
    static func hex(_ bytes: some Sequence<UInt8>) -> String { bytes.map { String(format: "%02x", $0) }.joined() }
    static func path(_ bytes: [UInt8]) -> [String: String] {
        ["encoding": "unix-bytes", "hex": hex(bytes),
         "display": String(bytes: bytes, encoding: .utf8) ?? "Native bytes: " + hex(bytes)]
    }
    static let digest = String(repeating: "a", count: 64)
    static func relativePath(_ request: AtlasWorkspaceStructureRequest) -> [UInt8] {
        Array((request.mode == .symbols ? "src/lib.rs" : "docs/readme.md").utf8)
    }
    static func source(_ request: AtlasWorkspaceStructureRequest) -> [UInt8] {
        Array(String(repeating: line(request), count: 100).utf8)
    }
    private static func line(_ request: AtlasWorkspaceStructureRequest) -> String {
        request.mode == .symbols ? "fn " + request.needle + "() {}\n" : "# " + request.needle + "\n"
    }
    static func occurrences(_ request: AtlasWorkspaceStructureRequest, round: Int) -> [UInt64] {
        guard round > 0 else { return [] }
        let observed: [UInt64] = request.startAfter == 0
            ? (1...39).map { UInt64($0) } + [80] : (81...100).map { UInt64($0) }
        return Array(observed.filter { $0 > request.startAfter && (round > 1 || $0 <= 80) }
            .prefix(Int(request.maximumMatches)))
    }
    static func hit(_ request: AtlasWorkspaceStructureRequest, index: UInt64, occurrence: UInt64) -> [String: Any] {
        let stride = UInt64(line(request).utf8.count)
        let start = (occurrence - 1) * stride + (request.mode == .symbols ? 3 : 2)
        return ["index": String(index), "occurrence_id": String(occurrence), "candidate_id": String(occurrence),
            "file_id": "7", "source_revision": "9", "source_length": String(source(request).count),
            "path": path(relativePath(request)), "name": request.needle,
            "kind": request.mode == .symbols ? "function" : "heading",
            "language": request.mode == .symbols ? "rust" : "markdown",
            "heading_slug": request.mode == .symbols ? NSNull() : "fixture-heading" as Any,
            "original_range": ["start": String(start), "end": String(start + UInt64(request.needle.utf8.count))],
            "evidence_range": ["start": String((occurrence - 1) * stride), "end": String(occurrence * stride)],
            "capture_sha256": digest, "capture_complete": true,
            "consistency": "unchanged-metadata-not-atomic", "encoding": "utf8"]
    }
    static func page(owner: UInt64, generation: UInt64 = 1, revision: UInt64 = 3,
        root: [UInt8] = Array("/private/tmp/project".utf8), request: AtlasWorkspaceStructureRequest,
        command: String = "page", round: Int = 2, stopped: Bool = false,
        offset: UInt64 = 0, count: UInt64 = 32) -> [String: Any] {
        let all = occurrences(request, round: round), retained = UInt64(all.count)
        let seen: UInt64 = round == 0 ? 0 : round == 1 ? 80 : 100
        let catalogued: UInt64 = round == 0 ? 256 : round == 1 ? 1024 : 5001
        let examined: UInt64 = round == 0 ? 0 : catalogued
        let more = round < 2 && !stopped, complete = round == 2 && !stopped
        let end = min(offset + count, retained), sourceBytes = UInt64(source(request).count)
        precondition(offset <= retained)
        var value: [String: Any] = [
            "schema": "fcb.catalog-structure/1", "status": "ok", "command": command,
            "owner": String(owner), "query_generation": String(generation), "revision": String(revision),
            "root_path": path(root), "scope": "progressively-discovered-whole-files",
            "strategy": "bounded-complete-capture-extraction", "capture_kind": "complete-retained-source",
            "source_observation": "per-file-captures-not-atomic-workspace",
            "ordering": "provisional-discovery-order", "membership": "progressive-observation",
            "discovery_policy": request.discoveryPolicy.wireName,
            "rule_files_enabled": !request.discoveryPolicy.includesExcluded,
            "rule_policy": request.discoveryPolicy.includesExcluded ? NSNull() : [:] as Any,
            "search_scope": request.fileScope.wireKind,
            "scope_extensions": request.fileScope.extensions ?? "", "scope_filenames": request.fileScope.filenames ?? "",
            "scope_path_kind": request.pathScope.kind.rawValue, "scope_path": request.pathScope.path,
            "scope_path_hex": request.pathScope.pathHex,
            "requested_mode": request.mode.rawValue, "realized_mode": request.mode.evidence,
            "evidence_level": request.mode.evidence, "semantic_complete": false, "compiler_resolved": false,
            "name_case": request.caseSensitive ? "sensitive" : "unicode-lowercase",
            "case_sensitive": request.caseSensitive, "name_mode": request.nameMode.rawValue, "needle": request.needle,
            "completeness_basis": "admitted-extraction-processing",
            "state": more ? "discovering" : complete ? "complete" : "partial",
            "more": more, "terminal": !more, "selection_available": retained > 0,
            "discovery_more": round < 2, "discovery_complete": round == 2,
            "search_complete": complete, "count_all_matches": true, "match_count_complete": complete,
            "truncated": seen > retained, "page_has_more_matches": seen - min(request.startAfter, seen) > retained,
            "stop_reason": stopped ? "user-stopped" as Any : NSNull(),
            "catalogued_files": String(catalogued), "eligible_files": String(catalogued), "excluded_files": "0",
            "pending_catalogued_files": "0", "files_examined": String(examined),
            "files_unexamined": String(catalogued - examined), "files_scanned": String(examined),
            "incomplete_files": "0", "unsupported_files": "0", "oversized_files": "0", "unavailable_files": "0",
            "analysis_limited_files": "0", "matching_files": seen > 0 ? "1" : "0",
            "matches_seen": String(seen), "retained_hits": String(retained), "retained_matching_files": retained > 0 ? "1" : "0",
            "retained_source_bytes": retained > 0 ? String(sourceBytes) : "0",
            "structure_metadata_bytes": String(retained * 128),
            "structure_metadata_omitted": String(seen - min(request.startAfter, seen) - retained),
            "source_retention_omitted_files": "0", "page_start_after": String(request.startAfter),
            "skipped_matches": String(min(request.startAfter, seen)),
            "payload_bytes_read": examined > 0 ? String(sourceBytes) : "0", "read_calls": examined > 0 ? "1" : "0",
            "retained_catalog_pages": "1", "retained_catalog_files": "256",
            "limits": ["max_matches": String(request.maximumMatches), "max_scan_bytes": String(request.maximumScanBytes),
                "max_read_calls": String(request.maximumReadCalls), "max_file_bytes": String(request.mode.maximumFileBytes),
                "max_retained_source_bytes": "33554432", "max_steps_per_advance": "16"],
            "page_offset": String(offset), "page_count": String(end - offset),
            "hits": (offset..<end).map { hit(request, index: $0, occurrence: all[Int($0)]) },
            "next_offset": end < retained ? String(end) as Any : NSNull(),
            "diagnostics": [], "diagnostics_truncated": false
        ]
        if request.pathScope.isVisible {
            let rawPaths = request.pathScope.path.split(separator: ",")
            value["scope_path"] = ""; value["scope_path_hex"] = ""
            value["scope_region_sha256"] = request.pathScope.visibleIdentity!
            value["scope_region_files"] = String(rawPaths.count)
            value["scope_region_path_bytes"] = String(rawPaths.reduce(0) { $0 + $1.utf8.count / 2 })
            value["scope_region_catalogued_files"] = String(rawPaths.count)
            value["scope_region_missing_files"] = "0"
            value["eligible_files"] = String(rawPaths.count)
            value["excluded_files"] = String(catalogued - UInt64(rawPaths.count))
            value["files_examined"] = round > 0 ? String(rawPaths.count) : "0"
            value["files_unexamined"] = round > 0 ? "0" : String(rawPaths.count)
            value["files_scanned"] = round > 0 ? String(rawPaths.count) : "0"
        }
        return value
    }
    static func readerInfo(owner: UInt64, request: AtlasWorkspaceStructureRequest, path: [UInt8]) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": "info", "owner": String(owner),
            "file_id": "1", "source_revision": "1", "captured_bytes": String(source(request).count), "encoding": "utf8",
            "path": Self.path(path), "capture_origin": "host-supplied", "capture_sha256": digest,
            "initial_source_bytes_read": "0", "initial_read_calls": "0", "additional_source_bytes_read": "0",
            "native_presented": false, "accepted_query_generation": NSNull(), "accepted_outline_generation": NSNull(),
            "accepted_document_generation": NSNull(), "accepted_reference_generation": NSNull()]
    }
    static func window(owner: UInt64, request: AtlasWorkspaceStructureRequest, path: [UInt8],
        offset: UInt64, bytes: UInt64) -> [String: Any] {
        let raw = source(request), end = min(UInt64(raw.count), offset + bytes)
        var value = readerInfo(owner: owner, request: request, path: path)
        value["command"] = "window"; value["text_kind"] = "logical-captured-text-not-shaped"
        value["requested_original_range"] = ["start": String(offset), "end": String(end)]
        value["visible_range"] = ["start": String(offset), "end": String(end)]
        value["range_limited"] = false; value["boundaries_adjusted"] = false; value["has_replacements"] = false
        value["first_physical_line"] = NSNull(); value["next_offset"] = end < UInt64(raw.count) ? String(end) as Any : NSNull()
        value["text"] = String(decoding: raw[Int(offset)..<Int(end)], as: UTF8.self)
        value["original_hex"] = hex(raw[Int(offset)..<Int(end)]); value["selection"] = NSNull()
        return value
    }
    static func receipt(owner: UInt64, reader: UInt64, request: AtlasWorkspaceStructureRequest,
        root: [UInt8] = Array("/private/tmp/project".utf8), generation: UInt64 = 1, revision: UInt64 = 3,
        round: Int = 2, stopped: Bool = false, index: UInt64 = 0) -> [String: Any] {
        let occurrence = occurrences(request, round: round)[Int(index)]
        let selected = hit(request, index: index, occurrence: occurrence)
        let start = UInt64((selected["original_range"] as! [String: String])["start"]!)!
        var value = page(owner: owner, generation: generation, revision: revision, root: root,
            request: request, command: "open-reader", round: round, stopped: stopped)
        value["hit"] = selected; value["reader_owner"] = String(reader)
        value["activation_source"] = "retained-search-capture"
        value["additional_source_bytes_read"] = "0"; value["additional_read_calls"] = "0"
        value["reader"] = readerInfo(owner: reader, request: request, path: relativePath(request))
        value["window"] = window(owner: reader, request: request, path: relativePath(request),
            offset: start >= 256 ? start - 256 : 0, bytes: 16384)
        return value
    }
    private func reply(_ phase: String, _ value: [String: Any]) -> String {
        lock.lock()
        events.append(phase)
        let block = paused == phase, patch = mutation
        if block { paused = nil; entered = true }
        lock.unlock()
        if block { gate.wait() }
        return Self.json(patch(phase, value))
    }
    var transport: AtlasWorkspaceStructureTransport {
        .init(create: {
            self.lock.lock(); defer { self.lock.unlock() }; self.nextOwner += 1
            self.events.append("create"); return self.nextOwner
        }, open: { owner, root, generation, request in
            precondition(root == "/tmp/project")
            let state = State(root: self.canonicalRoot, generation: generation, request: request)
            self.lock.lock(); self.states[owner] = state
            self.events.append("start-after:\(request.startAfter)"); self.lock.unlock()
            return self.reply("open", Self.page(owner: owner, generation: generation, revision: 1,
                root: state.root, request: request, command: "info", round: 0))
        }, advance: { owner, generation, revision, steps in
            self.lock.lock(); var state = self.states[owner]!
            precondition(generation == state.generation && revision == state.revision && steps == 4 && !state.stopped)
            state.round += 1; state.revision += 1; self.states[owner] = state; self.lock.unlock()
            return self.reply("advance", Self.page(owner: owner, generation: generation, revision: state.revision,
                root: state.root, request: state.request, command: "advance", round: state.round))
        }, page: { owner, generation, revision, offset, count in
            self.lock.lock(); let state = self.states[owner]!; self.lock.unlock()
            precondition(generation == state.generation && revision == state.revision)
            return self.reply("page", Self.page(owner: owner, generation: generation, revision: revision,
                root: state.root, request: state.request, round: state.round, stopped: state.stopped, offset: offset, count: count))
        }, stop: { owner, generation in
            self.lock.lock(); var state = self.states[owner]!
            precondition(generation == state.generation && state.round < 2 && !state.stopped)
            state.stopped = true; state.revision += 1; self.states[owner] = state; self.lock.unlock()
            return self.reply("stop", Self.page(owner: owner, generation: generation, revision: state.revision,
                root: state.root, request: state.request, command: "stop", round: state.round, stopped: true))
        }, openReader: { owner, reader, generation, revision, index in
            self.lock.lock(); let state = self.states[owner]!
            precondition(generation == state.generation && revision == state.revision)
            self.readers[reader] = Reader(request: state.request, path: Self.relativePath(state.request)); self.lock.unlock()
            return self.reply("open-reader", Self.receipt(owner: owner, reader: reader, request: state.request,
                root: state.root, generation: generation, revision: revision, round: state.round, stopped: state.stopped, index: index))
        }, cancel: { _ in
            self.lock.lock(); self.events.append("cancel"); self.lock.unlock(); return true
        }, close: { owner in
            self.lock.lock(); self.states.removeValue(forKey: owner); self.events.append("close"); self.lock.unlock(); return true
        }, retirementFailed: { preconditionFailure("Fixture structure retirement failed") })
    }
    var readerTransport: AtlasReaderTransport {
        .init(create: {
            self.lock.lock(); defer { self.lock.unlock() }; self.nextReader += 1
            self.events.append("create-reader"); return self.nextReader
        }, open: { _, _, _ in preconditionFailure("Retained structural hits must never reopen live paths") },
        window: { owner, offset, bytes in
            self.lock.lock(); let reader = self.readers[owner]!; self.lock.unlock()
            return self.reply("reader-window", Self.window(owner: owner, request: reader.request,
                path: reader.path, offset: offset, bytes: bytes))
        }, lines: { _, _, _, _ in nil }, cancel: { _ in true }, close: { owner in
            self.lock.lock(); self.readers.removeValue(forKey: owner); self.events.append("close-reader"); self.lock.unlock(); return true
        }, retirementFailed: { preconditionFailure("Fixture retained reader retirement failed") },
        fork: { source, destination in
            self.lock.lock()
            let reader = self.readers[source]!
            self.readers[destination] = reader
            self.lock.unlock()
            return self.reply("reader-fork", Self.readerInfo(owner: destination,
                request: reader.request, path: reader.path))
        },
        comparison: .init(domain: domain, compare: { _, _, _, _, _ in nil }))
    }
}

