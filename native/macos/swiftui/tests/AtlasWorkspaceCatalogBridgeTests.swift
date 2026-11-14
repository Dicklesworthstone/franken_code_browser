import Foundation

@_silgen_name("catalog_stream_fixture_reset") private func reset()
@_silgen_name("catalog_stream_fixture_receipt") private func receipt(_ kind: UInt32, _ json: UnsafePointer<CChar>?)
@_silgen_name("catalog_stream_fixture_expected") private func expected(_ query: UnsafePointer<CChar>?,
    _ mode: UInt64, _ sensitive: UInt64, _ counted: UInt64, _ start: UInt64, _ scope: UInt64)
@_silgen_name("catalog_stream_fixture_mode") private func mode(_ value: Int32)
@_silgen_name("catalog_stream_fixture_count") private func count(_ kind: UInt32) -> UInt32
@_silgen_name("catalog_stream_fixture_allocations") private func allocations() -> UInt32
@_silgen_name("catalog_stream_fixture_frees") private func frees() -> UInt32

private final class CatalogProgressCopies: @unchecked Sendable {
    private let lock = NSLock()
    private var received: [String] = []
    func append(_ value: String) { lock.lock(); received.append(value); lock.unlock() }
    var values: [String] { lock.lock(); defer { lock.unlock() }; return received }
}

@main private enum AtlasWorkspaceCatalogBridgeTests {
    static let owner: UInt64 = 0xfedcba9876543210, generation: UInt64 = 0x8000000100000007
    static let root = "/repo-é", needle = "old é😀"
    static func json(_ value: [String: Any]) throws -> String {
        String(decoding: try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self)
    }
    static func path(_ text: String) -> [String: String] {
        ["encoding": "unix-bytes", "hex": AtlasObservedReaderWire.hex(Array(text.utf8)), "display": text]
    }
    static func fixture(_ command: String, terminal: Bool) -> [String: Any] {
        let revision = terminal ? "2" : "1", bytes = terminal ? "64" : "0"
        let files = terminal ? "25001" : "0", matches = terminal ? "2" : "0"
        let rows: [[String: Any]] = terminal ? [0, 1].map { index in
            ["index": String(index), "occurrence_id": String(index + 1), "file_id": "25001",
             "source_revision": String(generation), "path": path("outside-atlas.txt"),
             "original_range": ["start": String(index * 20), "end": String(index * 20 + needle.utf8.count)],
             "encoding": "utf8", "observed_length": "64", "final_observed_length": "64",
             "consistency": "unchanged-metadata-not-atomic", "file_state": "complete-observed-input", "file_complete": true,
             "witness_sha256": String(repeating: "b", count: 64), "text": needle,
             "original_hex": AtlasObservedReaderWire.hex(Array(needle.utf8))]
        } : []
        var value: [String: Any] = ["schema": "fcb.catalog-stream/1", "status": "ok", "command": command,
            "owner": String(owner), "query_generation": String(generation), "revision": revision, "progress_sequence": revision,
            "root_path": path(root), "scope": "progressively-discovered-whole-files", "strategy": "streaming-whole-file",
            "mode": "exact-decoded-literal", "capture_kind": "literal-witnesses-only",
            "source_observation": "per-file-streams-not-atomic-workspace", "ordering": "provisional-discovery-order",
            "membership": "progressive-observation", "discovery_policy": "repository-rules-v1", "excluded_entries": "0",
            "search_scope": "all", "scope_extensions": "", "scope_filenames": "", "scope_path_kind": "workspace",
            "scope_path": "", "scope_path_hex": "", "case_sensitive": true,
            "catalogued_files": files, "eligible_files": files, "excluded_files": "0", "pending_catalogued_files": "0",
            "discovery_complete": terminal, "discovery_more": !terminal,
            "more": !terminal, "terminal": terminal, "selection_available": terminal,
            "needle": needle, "search_complete": terminal, "count_all_matches": false, "match_count_complete": terminal,
            "truncated": false, "stop_reason": NSNull(), "files_examined": files, "files_unexamined": "0",
            "incomplete_files": "0", "unavailable_files": "0", "unsupported_files": "0", "failed_files": "0",
            "matching_files": terminal ? "1" : "0", "retained_matching_files": terminal ? "1" : "0",
            "retained_hits": matches, "matches_seen": matches, "page_start_after": "0", "skipped_matches": "0",
            "page_has_more_matches": false, "payload_bytes_read": bytes, "scanned_input_bytes": bytes,
            "read_calls": terminal ? "1" : "0", "interrupted_read_calls": "0", "peak_input_buffer_bytes": bytes,
            "retained_witness_bytes": terminal ? String(needle.utf8.count) : "0",
            "limits": ["max_matches": "256", "max_scan_bytes": "4294967296", "max_read_calls": "1048576"],
            "page_offset": "0", "page_count": matches, "next_offset": NSNull(), "hits": rows]
        if command == "hit" {
            value["hit"] = rows[1]; value.removeValue(forKey: "hits")
            value["additional_source_bytes_read"] = "0"; value["additional_read_calls"] = "0"
        }
        return value
    }
    static func configure(_ request: AtlasWorkspaceStreamRequest, scope: UInt64 = 0) {
        request.needle.withCString { expected($0, request.mode == .text ? 0 : request.mode == .occurrences ? 1 : 2,
            request.caseSensitive ? 1 : 0, request.countAllMatches ? 1 : 0, request.pageStartAfter, scope) }
    }
    static func main() throws {
        reset()
        for (kind, command, terminal) in [(UInt32(0), "info", false), (1, "advance", true),
            (2, "page", true), (3, "hit", true), (4, "stop", true)] {
            try json(fixture(command, terminal: terminal)).withCString { receipt(kind, $0) }
        }
        let transport = AtlasWorkspaceStreamTransport.catalogNative, catalogSearch = transport.catalogSearch!
        let request = try AtlasWorkspaceStreamRequest(needle: needle, maximumScanBytes: 1 << 32)
        configure(request); precondition(transport.create() == owner)
        let copies = CatalogProgressCopies()
        let result = try catalogSearch.search(owner, root, generation, request,
            AtlasSearchCancellation(), AtlasWorkspaceStreamStop()) { copies.append($0) }
        let catalog = try AtlasWorkspaceStreamCatalog.ready(owner: owner, root: root)
        let progress = try AtlasWorkspaceStreamProgress.decode(copies.values.first!, catalog: catalog, request: request)
        let page = try AtlasWorkspaceStreamPage.decode(result, catalog: catalog, root: root, request: request, requireTerminal: true)
        try progress.validateTerminal(page)
        let report = try AtlasWorkspaceStreamReport(summary: page.summary, hits: page.hits)
        precondition(report.summary.cataloguedFiles == 25_001 && report.summary.complete && report.hits.count == 2)
        precondition(report.summary.witnessBytes == UInt64(needle.utf8.count) && report.hits.allSatisfy { $0.node == nil })
        let witness = try AtlasWorkspaceStreamWitness.decode(catalogSearch.hit(owner, generation, 2, 1),
            report: report, root: root, target: report.target(for: report.hits[1]))
        precondition(witness.text == needle && allocations() == frees())
        precondition(count(0) == 1 && count(1) == 1 && count(2) == 1 && count(3) == 1)

        // Opening an empty root can be terminal immediately. Do not advance
        // that cursor or invent progress to manufacture a later revision.
        var empty = fixture("info", terminal: false)
        empty["terminal"] = true; empty["selection_available"] = true; empty["more"] = false
        empty["discovery_more"] = false; empty["discovery_complete"] = true
        empty["search_complete"] = true; empty["match_count_complete"] = true
        try json(empty).withCString { receipt(0, $0) }
        empty["command"] = "page"; try json(empty).withCString { receipt(2, $0) }
        let beforeEmpty = count(1)
        let emptyResult = try catalogSearch.search(owner, root, generation, request,
            AtlasSearchCancellation(), AtlasWorkspaceStreamStop()) { _ in preconditionFailure("Empty terminal root published progress") }
        let emptyPage = try AtlasWorkspaceStreamPage.decode(emptyResult, catalog: catalog, root: root,
            request: request, requireTerminal: true)
        precondition(emptyPage.summary.complete && emptyPage.summary.cataloguedFiles == 0
            && emptyPage.hits.isEmpty && count(1) == beforeEmpty)
        try json(fixture("info", terminal: false)).withCString { receipt(0, $0) }
        try json(fixture("page", terminal: true)).withCString { receipt(2, $0) }

        // Exact ABI values preserve all query modes, scope predicates, empty
        // predicates and fresh ordinal windows, including wide counters.
        let types = try AtlasSearchFileScope(extensions: ["rs"], filenames: ["Makefile"])
        let scopes: [(UInt64, AtlasSearchFileScope, AtlasSearchPathScope)] = [
            (0, .all, .workspace), (1, try .init(extensions: []), .workspace),
            (2, types, try .init(kind: .directory, path: "src")),
            (3, types, try .init(kind: .file, path: "src/main.rs"))]
        for searchMode in AtlasWorkspaceStreamMode.allCases {
            for (scope, types, location) in scopes {
                let query = searchMode == .expression ? "old path:src" : searchMode == .occurrences ? "old_é😀" : needle
                let input = try AtlasWorkspaceStreamRequest(needle: query, maximumScanBytes: 1 << 32,
                    fileScope: types, pathScope: location, countAllMatches: true,
                    caseSensitive: searchMode == .occurrences, mode: searchMode, pageStartAfter: 257)
                configure(input, scope: scope)
                _ = try catalogSearch.search(owner, root, generation, input, AtlasSearchCancellation(), AtlasWorkspaceStreamStop()) { _ in }
            }
        }
        configure(request)
        let stop = AtlasWorkspaceStreamStop(), advances = count(1)
        _ = try catalogSearch.search(owner, root, generation, request, AtlasSearchCancellation(), stop) { _ in stop.requestStop() }
        precondition(count(1) == advances && count(4) == 1)
        let canceled = AtlasSearchCancellation(), opened = count(0)
        canceled.cancel()
        do {
            _ = try catalogSearch.search(owner, root, generation, request, canceled, AtlasWorkspaceStreamStop()) { _ in }
            preconditionFailure("Canceled query crossed the ABI")
        } catch AtlasWorkspaceStreamError.canceled { }
        precondition(count(0) == opened)
        let late = AtlasSearchCancellation(), cancels = count(5)
        do {
            _ = try catalogSearch.search(owner, root, generation, request, late, AtlasWorkspaceStreamStop()) { _ in late.cancel() }
            preconditionFailure("Canceled provisional receipt advanced")
        } catch AtlasWorkspaceStreamError.canceled { }
        precondition(count(1) == advances && count(5) == cancels + 1)
        for invalid in [Int32(1), 2, 3] {
            mode(invalid)
            do {
                _ = try catalogSearch.search(owner, root, generation, request,
                    AtlasSearchCancellation(), AtlasWorkspaceStreamStop()) { _ in }
                preconditionFailure("Malformed catalog receipt accepted")
            } catch AtlasWorkspaceStreamError.invalidResponse { }
            precondition(allocations() == frees())
        }
        mode(0)
        var stale = fixture("info", terminal: false); stale["revision"] = "0"
        try json(stale).withCString { receipt(0, $0) }
        do {
            _ = try catalogSearch.search(owner, root, generation, request,
                AtlasSearchCancellation(), AtlasWorkspaceStreamStop()) { _ in }
            preconditionFailure("Invalid initial revision accepted")
        } catch AtlasWorkspaceStreamError.invalidResponse { }
        precondition(transport.close(owner) && allocations() == frees())
        reset()
        print("Progressive catalog native ABI fixtures passed; no physical Mac qualification claimed")
    }
}
