import Foundation
import Dispatch

/// Only the foreign transport is injected. Production saved-archive decoding,
/// model reconciliation, retained-reader scheduling and receipt admission run.
/// This does not qualify the native ABI, filesystem grants or physical Mac UI.
final class SavedRepositoryFixture: @unchecked Sendable {
    struct Member: Sendable {
        let path: [UInt8]
        let display: String
        let bytes: [UInt8]?
        let digest: String?
        let encoding: String
        init(path: [UInt8], display: String, bytes: [UInt8]?, digest: String?, encoding: String = "utf8") {
            self.path = path; self.display = display; self.bytes = bytes; self.digest = digest; self.encoding = encoding
        }
    }
    struct QueryHit: Sendable {
        let member: UInt64
        let start: UInt64
        let end: UInt64
    }
    struct Session {
        let path: String
        var generation: UInt64?
        var queryGeneration: UInt64?
        var needle: String?
        var countAllMatches = false
        var caseSensitive = true
        var expressionGeneration: UInt64?
        var expression: String?
        var countAllExpressionMatches = false
        var expressionAttempt: UInt64 = 0
        var structureGeneration: UInt64?
        var structureMode: AtlasSavedSearchMode?
        var structureNameMode: AtlasSearchNameMode?
        var structureNeedle: String?
        var structureAttempt: UInt64 = 0
        var indexGeneration: UInt64?
        var indexPin: String?
        var indexAttempt: UInt64 = 0
        var indexPageBytes: UInt64 = 0
        var queryIndexGeneration: UInt64?
        var queryIndexPin: String?
        var readBytes: UInt64 = 0
        var readCalls: UInt64 = 0
        var refused = false
    }
    let domain = UUID()
    let searchVectors: Bool
    let expansionVectors: Bool
    let headingVectors: Bool
    let longHeading: Bool
    let lineEncoding: String?
    let lineRange: (UInt64, UInt64)?
    init(searchVectors: Bool = false, headingVectors: Bool = false, longHeading: Bool = false,
        lineEncoding: String? = nil, lineRange: (UInt64, UInt64)? = nil, expansionVectors: Bool = false) {
        precondition(lineEncoding == nil || (["utf8", "utf16le", "utf16be"].contains(lineEncoding!) && !searchVectors && !headingVectors))
        precondition(!expansionVectors || (searchVectors && !headingVectors && lineEncoding == nil))
        self.searchVectors = searchVectors; self.expansionVectors = expansionVectors
        self.headingVectors = headingVectors; self.longHeading = longHeading
        self.lineEncoding = lineEncoding; self.lineRange = lineRange
    }
    let archiveDigest = String(repeating: "d", count: 64)
    var source: [UInt8] {
        if expansionVectors { return [] }
        return Array((headingVectors
            ? "\u{feff}---\ntitle: Saved\n---\n\n[guide](docs/guide.md#caf-%64etails) [again](docs/guide.md) [missing](absent.rs) [empty]()\n"
            : "\u{feff}---\ntitle: Saved\n---\n\n[code](src/lib.rs#L2) [again](src/lib.rs) [missing](absent.rs) [empty]()\n").utf8)
    }
    var sourceDigest: String { expansionVectors
        ? "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855" : headingVectors
        ? "9359955440643a4a2294506051b4cd66f7009333234a8e3f85392495ec5a9a0f"
        : "8591776e77fd8d9050b0a5796da773312c306092c3fc05f22dbe9413cfcff6d3" }
    var headingBytes: [UInt8] {
        if longHeading { return Array(("## " + String(repeating: "A", count: 3000) + "\n").utf8) }
        return Array(("\u{feff}" + String(repeating: "x", count: 200) + "\r\n\r\n## Café Details\r\n\r\nBody.\r\n").utf8)
    }
    var lineBounds: (UInt64, UInt64) { lineRange ?? (lineEncoding == "utf8" ? (7, 9) : (16, 20)) }
    private var lineMember: Member? {
        guard let lineEncoding else { return nil }
        let text = "first\r\n\r\nlet x = 7;\r\nlast\r\n"
        let bytes: [UInt8], digest: String
        switch lineEncoding {
        case "utf8":
            bytes = Array(text.utf8)
            digest = "5cca6e0d05bfa2b08813fc9f5f62c8beef040de0f7898560647e69d4f64d8b02"
        case "utf16le":
            bytes = [255, 254] + text.utf16.flatMap { [UInt8(truncatingIfNeeded: $0), UInt8(truncatingIfNeeded: $0 >> 8)] }
            digest = "b5107653ea8bc775adfd3890c97e0d9af21bd88982e247ae76de4af28529d82a"
        default:
            bytes = [254, 255] + text.utf16.flatMap { [UInt8(truncatingIfNeeded: $0 >> 8), UInt8(truncatingIfNeeded: $0)] }
            digest = "96119d4e760066f18541045ec4e737bf326656b12743530828704edc34ded703"
        }
        return Member(path: Array("src/lib.rs".utf8), display: "src/lib.rs", bytes: bytes, digest: digest, encoding: lineEncoding)
    }
    let rawPath = Array("raw-".utf8) + [UInt8(255)] + Array(".rs".utf8)
    let entered = DispatchSemaphore(value: 0)
    let resume = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var nextSaved: UInt64 = 40
    private var nextReader: UInt64 = 100
    private var sessions: [UInt64: Session] = [:]
    private var readers: [UInt64: Member] = [:]
    private var log: [String] = []
    private var fault: String?
    private var blocked = false
    private var blockedSearch = false
    private var blockedIndex = false
    private var wrongPage: Set<UInt64> = []

    private func locked<T>(_ body: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return body() }
    var calls: [String] { locked { log } }
    func corruptNext(_ kind: String) { locked { fault = kind } }
    func blockNextHandoff() { locked { blocked = true } }
    func blockNextSearch() { locked { blockedSearch = true } }
    func blockNextIndex() { locked { blockedIndex = true } }
    var searchUTF8: [UInt8] { expansionVectors ? [0xc4, 0xb0]
        : Array(("é" + String(repeating: "x", count: 200) + "banana\n").utf8) }
    var searchUTF16: [UInt8] {
        if expansionVectors { return [255, 254, 0x30, 0x01] }
        return [255, 254, 233, 0] + Array(repeating: [UInt8(120), 0], count: 200).flatMap { $0 }
            + [98, 0, 97, 0, 110, 0, 97, 0, 110, 0, 97, 0, 10, 0]
    }
    var members: [Member] {
        [
            Member(path: Array("README.md".utf8), display: "README.md", bytes: source, digest: sourceDigest),
            Member(path: rawPath, display: "raw-\\xff.rs",
                bytes: searchVectors ? searchUTF8 : Array("changed bytes\n".utf8),
                digest: expansionVectors ? "a65018cffe8e0ed3f3112a79326e4c9718dc498a1f18a3a72f2f77c792c48b70" : searchVectors ? "ec34a752af5a6ce18bfd388bda04c6c9f60b22301b44d413c085507ca994f6cc"
                    : "7c60358cb724c1b671469c5f167cea45a4f70df4e3ac87b79340692e6f17f574"),
            Member(path: Array("skipped.md".utf8), display: "skipped.md",
                bytes: searchVectors ? searchUTF16 : [255],
                digest: expansionVectors ? "138366bfcd2687fbf98b5a0f74101c54efbf1fdda422e4a8de157b57e633ac3b" : searchVectors ? "0896dfbe40292ec212fa69dac19c681c8f56ba79c407cbad6817580762413ecf"
                    : "a8100ae6aa1940d0b663bb31cd466142ebbdbd5187131b92d93818987832eb89",
                encoding: searchVectors ? "utf16le" : "utf8"),
            lineMember ?? Member(path: Array((headingVectors ? "docs/guide.md" : "src/lib.rs").utf8),
                display: headingVectors ? "docs/guide.md" : "src/lib.rs",
                bytes: headingVectors ? headingBytes : (searchVectors ? [254, 255, 0, 97, 254, 255, 0, 98] : Array("abc".utf8)),
                digest: headingVectors ? (longHeading
                    ? "62f9f79efe4621e0d2707ab9770a46ca345c885c7453d5a20827158ff76b8d33"
                    : "764c4fad3f4ed654c16f17da7c527d4213539d68ca1f13a944eab08c7dd456c9")
                    : (searchVectors ? "c71ff6c5ed08a430f34d4fcd60b2c151d57934c79a6087aa25496d9b06c65cc7"
                    : "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
                encoding: headingVectors ? "utf8" : (searchVectors ? "utf16be" : "utf8")),
            Member(path: Array("unavailable.md".utf8), display: "unavailable.md", bytes: nil, digest: nil)
        ]
    }
    func encode(_ value: [String: Any]) -> String {
        String(decoding: try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self)
    }
    func hex(_ bytes: some Sequence<UInt8>) -> String { bytes.map { String(format: "%02x", $0) }.joined() }
    func path(_ bytes: [UInt8], _ display: String) -> [String: Any] {
        ["encoding": "unix-bytes", "hex": hex(bytes), "display": display]
    }
    func header(_ owner: UInt64, _ command: String) -> [String: Any] {
        ["schema": "fcb.saved-repository/1", "status": "ok", "command": command, "owner": String(owner),
         "archive_digest": archiveDigest, "members_total": "5", "captured_members": "4",
         "discovery_complete": true, "live_root_authority": false, "native_presented": false]
    }
    func memberObject(_ id: UInt64) -> [String: Any] {
        let member = members[Int(id)]
        var out: [String: Any] = ["member": String(id), "file_id": String(id + 1),
            "path": path(member.path, member.display), "observed_bytes": String(member.bytes?.count ?? 99),
            "captured": member.bytes != nil]
        if let digest = member.digest { out["source_digest"] = digest }
        else { out["reason"] = "READ_DENIED" }
        return out
    }
    func memberPage(_ owner: UInt64, start: UInt64, limit: UInt64) -> [String: Any] {
        var out = header(owner, "members")
        let end = min(5, start + limit)
        out["members"] = (start..<end).map(memberObject)
        out["next_offset"] = end < 5 ? String(end) as Any : NSNull()
        return out
    }
    func linkObject(_ id: UInt64) -> [String: Any] {
        let target = headingVectors ? "docs/guide.md" : "src/lib.rs"
        let suffix = headingVectors ? "#caf-%64etails" : "#L2"
        let destination = [1: target + suffix, 2: target, 3: "absent.rs", 4: ""][Int(id)]!
        let captured = id <= 2
        return ["link_id": String(id), "source_member": "0", "source_file_id": "1",
            "source_revision": "1", "source_digest": sourceDigest,
            "source_path": path(Array("README.md".utf8), "README.md"),
            "evidence_range": ["start": "0", "end": String(source.count)],
            "block_index": "0", "source_link_id": String(id), "destination": destination,
            "resolution": captured ? "captured" : (id == 3 ? "missing" : "unresolved"),
            "target_member": captured ? "3" as Any : NSNull(),
            "target_path": captured ? path(Array(target.utf8), target) as Any : NSNull(),
            "target_suffix": id == 1 ? suffix : "",
            "fragment_resolution": id == 1 ? (headingVectors ? "resolved" : (lineEncoding != nil ? "resolved-lines" : "unsupported")) : "none",
            "fragment_slug": id == 1 ? (headingVectors ? "caf-details" : "L2") as Any : NSNull(),
            "target_range": id == 1 && headingVectors ? ["start": "207", "end": "223"] as Any
                : (id == 1 && lineEncoding != nil ? ["start": String(lineBounds.0), "end": String(lineBounds.1)] as Any : NSNull())]
    }
    func relationships(_ owner: UInt64, generation: UInt64, direction: AtlasSavedDirection = .all,
        member: UInt64? = nil, start: UInt64 = 0, limit: UInt64 = 64, preparing: Bool = false) -> [String: Any] {
        var out = header(owner, preparing ? "relationships-prepare" : "relationships")
        let ids: [UInt64]
        switch direction {
        case .all: ids = [1, 2, 3, 4]
        case .outgoing: ids = member == 0 ? [1, 2, 3, 4] : []
        case .incoming: ids = member == 3 ? [1, 2] : []
        }
        let end = min(UInt64(ids.count), start + limit)
        out.merge([
            "relationship_generation": String(generation), "projection": "markdown-document-links",
            "evidence_precision": "whole-captured-markdown-document", "resolver": "fmd-exact-member-v1",
            "extractor": "franken-markdown-d6fbd1d5/document-evidence-v1",
            "completeness_scope": "parsed-document-links-in-saved-manifest",
            "relationships_complete": false, "truncated": false, "markdown_members": headingVectors ? "4" : "3",
            "scanned_markdown_members": headingVectors ? "2" : "1", "unavailable_markdown_members": "1",
            "unsupported_markdown_members": "1", "source_limited_members": "0", "diagnostic_markdown_members": "0",
            "retained_links": "4", "links_seen": "4", "matching_links": String(ids.count),
            "member_bytes_read": String(source.count + members[2].bytes!.count + (headingVectors ? 2 * headingBytes.count : 0)
                + (lineEncoding != nil ? members[3].bytes!.count : 0)),
            "member_read_calls": headingVectors ? "4" : (lineEncoding != nil ? "3" : "2"),
            "retained_source_payload_bytes": "0", "direction": direction.rawValue,
            "filter_member": member.map(String.init) as Any? ?? NSNull(),
            "links": ids[Int(start)..<Int(end)].map(linkObject),
            "next_offset": end < UInt64(ids.count) ? String(end) as Any : NSNull()
        ]) { _, new in new }
        return out
    }
    func info(_ owner: UInt64, path archivePath: String = "/saved/repository.fcbs",
        generation: UInt64? = nil, query: UInt64? = nil, bytes: UInt64 = 0, calls: UInt64 = 0,
        expression: UInt64? = nil, expressionAttempt: UInt64 = 0,
        index: UInt64? = nil, pin: String? = nil, indexAttempt: UInt64 = 0, indexPageBytes: UInt64 = 0,
        structure: UInt64? = nil, structureAttempt: UInt64 = 0) -> [String: Any] {
        var out = header(owner, "info")
        out["archive_path"] = path(Array(archivePath.utf8), archivePath)
        out["accepted_relationship_generation"] = generation.map(String.init) as Any? ?? NSNull()
        out["accepted_query_generation"] = query.map(String.init) as Any? ?? NSNull()
        out["accepted_index_generation"] = index.map(String.init) as Any? ?? NSNull()
        out["last_index_attempt"] = String(indexAttempt)
        if let index, let pin {
            out.merge(indexFields(generation: index, pin: pin)) { _, new in new }
            out["index_page_bytes_read"] = String(indexPageBytes)
        }
        out["accepted_expression_generation"] = expression.map(String.init) as Any? ?? NSNull()
        out["last_expression_attempt"] = String(expressionAttempt)
        out["accepted_structure_generation"] = structure.map(String.init) as Any? ?? NSNull()
        out["last_structure_attempt"] = String(structureAttempt)
        out["initial_archive_bytes_read"] = "2048"; out["directory_reserved_bytes"] = "4096"
        out["member_bytes_read"] = String(bytes); out["member_read_calls"] = String(calls)
        return out
    }
    func indexFields(generation: UInt64, pin: String) -> [String: Any] {
        ["index_generation": String(generation), "index_pin": pin,
         "index_kind": "demand-paged-postings-v1", "index_pin_scope": "page-manifest-envelope-sha256",
         "index_body_verified_on_open": false, "index_members": "5", "index_indexed_files": "3",
         "index_uncovered_files": "1", "index_unavailable_files": "1", "index_file_bytes": "4096",
         "index_manifest_bytes_read": "512", "index_page_bytes_read": "0", "index_page_loads": "0",
         "index_cache_hits": "0", "index_cache_capacity_bytes": "65536"]
    }
    func indexAttachment(_ owner: UInt64, generation: UInt64, pin: String) -> [String: Any] {
        var out = header(owner, "index-attach")
        out.merge(indexFields(generation: generation, pin: pin)) { _, new in new }
        out["last_index_attempt"] = String(generation)
        return out
    }
    func searchHits(_ needle: String, caseSensitive: Bool = true) -> [QueryHit] {
        // Independent fixed vectors; no substitute matcher or offset inference.
        guard searchVectors else { return [] }
        if expansionVectors {
            guard (!caseSensitive && (needle.utf8.elementsEqual("i".utf8) || needle.utf8.elementsEqual("\u{307}".utf8)))
                || needle.utf8.elementsEqual("\u{130}".utf8) else { return [] }
            return [QueryHit(member: 1, start: 0, end: 2), QueryHit(member: 2, start: 2, end: 4)]
        }
        if !caseSensitive && needle.utf8.elementsEqual("ANA".utf8) { return searchHits("ana") }
        if needle.utf8.elementsEqual("\u{feff}".utf8) { return [QueryHit(member: 3, start: 4, end: 6)] }
        guard needle.utf8.elementsEqual("ana".utf8) else { return [] }
        return [QueryHit(member: 1, start: 203, end: 206), QueryHit(member: 1, start: 205, end: 208),
            QueryHit(member: 2, start: 406, end: 412), QueryHit(member: 2, start: 410, end: 416)]
    }
    func hitObject(_ id: UInt64, needle: String = "ana", caseSensitive: Bool = true) -> [String: Any] {
        let hit = searchHits(needle, caseSensitive: caseSensitive)[Int(id - 1)], member = members[Int(hit.member)]
        return ["hit_id": String(id), "member": String(hit.member), "file_id": String(hit.member + 1),
            "source_revision": String(hit.member + 1), "source_digest": member.digest!,
            "path": path(member.path, member.display),
            "original_range": ["start": String(hit.start), "end": String(hit.end)]]
    }
    func searchPage(_ owner: UInt64, generation: UInt64, needle: String = "ana",
        start: UInt64 = 0, limit: UInt64 = 64, searching: Bool = false,
        index: UInt64? = nil, pin: String? = nil, countAllMatches: Bool? = nil,
        caseSensitive: Bool = true) -> [String: Any] {
        var out = header(owner, searching ? "search" : "results")
        let hits = searchHits(needle, caseSensitive: caseSensitive), end = min(UInt64(hits.count), start + limit)
        let sourceBytes = members.compactMap(\.bytes).reduce(0) { $0 + $1.count }
        out.merge(["query_generation": String(generation), "needle": needle,
            "index_generation": index.map(String.init) as Any? ?? NSNull(), "index_pin": pin as Any? ?? NSNull(),
            "mode": caseSensitive ? "exact-decoded-literal" : "unicode-lowercase-literal",
            "case_sensitive": caseSensitive, "index_used": index != nil,
            "index_policy": index == nil ? "direct-verified-member-scan" : "exact-compatible-postings",
            "search_complete": false, "truncated": false, "retained_hits": String(hits.count),
            "matches_seen": String(hits.count), "unavailable_files": "1", "unsupported_files": "0",
            "member_bytes_read": String(sourceBytes), "member_read_calls": "4",
            "scanned_bytes": String(sourceBytes), "peak_source_bytes": String(members.compactMap(\.bytes).map(\.count).max() ?? 0),
            "index_eliminated_files": index == nil ? "0" : "1", "fallback_files": index == nil ? "0" : "2",
            "posting_entries_visited": index == nil ? "0" : "3",
            "retained_source_payload_bytes": "0",
            "hits": (start..<end).map { hitObject($0 + 1, needle: needle, caseSensitive: caseSensitive) },
            "next_offset": end < UInt64(hits.count) ? String(end) as Any : NSNull()
        ]) { _, new in new }
        if let countAllMatches {
            out["count_all_matches"] = countAllMatches; out["matches_seen_exact"] = false
            out["max_matches"] = "4096"
            out["matching_files"] = String(Set(hits.map(\.member)).count)
            out["retained_matching_files"] = out["matching_files"]
        }
        return out
    }
    func expressionPage(_ owner: UInt64, generation: UInt64, query: String = "ana +banana -unsafe",
        start: UInt64 = 0, limit: UInt64 = 64, countAllMatches: Bool? = nil) -> [String: Any] {
        // Fixed protocol vectors. The production Rust tests own grammar and
        // predicate evaluation; this fixture does not implement either.
        let filtered = query == "ana path:raw"
        let count: UInt64 = filtered ? 2 : 4
        let sourceBytes = filtered ? members[1].bytes!.count : members.compactMap(\.bytes).reduce(0) { $0 + $1.count }
        let end = min(count, start + limit)
        var out = header(owner, "expression")
        out.merge([
            "selection_namespace": "saved-expression", "expression_generation": String(generation),
            "expression": query, "primary_needle": "ana", "predicate_scope": "whole-captured-member",
            "case_sensitive": true, "index_used": false, "index_policy": "direct-verified-member-scan",
            "state": "complete", "complete": filtered, "matches_seen_exact": filtered, "truncated": false,
            "required_terms": filtered ? "0" : "1", "excluded_terms": filtered ? "0" : "1",
            "max_hits": "4096", "max_scan_bytes": "67108864", "retained_hits": String(count), "matches_seen": String(count),
            "metadata_examined": "5", "scope_files": filtered ? "1" : "5", "metadata_excluded": filtered ? "4" : "0",
            "unavailable_files": filtered ? "0" : "1", "unsupported_files": "0", "members_loaded": filtered ? "1" : "4",
            "member_bytes_read": String(sourceBytes), "member_read_calls": filtered ? "1" : "4",
            "loaded_bytes": String(sourceBytes), "peak_source_bytes": String(filtered ? sourceBytes : members.compactMap(\.bytes).map(\.count).max()!),
            "predicate_scans": filtered ? "0" : "8", "predicate_rejections": filtered ? "0" : "2",
            "primary_scans": filtered ? "1" : "2", "scanned_bytes": String(filtered ? sourceBytes : sourceBytes * 2 + searchUTF8.count + searchUTF16.count),
            "retained_source_payload_bytes": "0", "hits": (start..<end).map { hitObject($0 + 1) },
            "next_offset": end < count ? String(end) as Any : NSNull()
        ]) { _, next in next }
        if let countAllMatches {
            out["count_all_matches"] = countAllMatches
            out["matching_files"] = filtered ? "1" : "2"
            out["retained_matching_files"] = out["matching_files"]
        }
        return out
    }
    var occurrenceNeedle: String { "é" + String(repeating: "x", count: 200) + "banana" }
    func structureHits(_ mode: AtlasSavedSearchMode, needle: String) -> [QueryHit] {
        // Fixed producer vectors; this fixture does not parse declarations,
        // Markdown or identifier tokens. The shared Rust suites own extraction.
        switch mode {
        case .symbols:
            guard searchVectors, needle == "banana" || needle == "crate::banana" else { return [] }
            return [QueryHit(member: 1, start: 202, end: 208)]
        case .headings:
            guard headingVectors, needle == (longHeading ? "A" : "Café") else { return [] }
            return [QueryHit(member: 3, start: longHeading ? 0 : 207, end: longHeading ? 3003 : 223)]
        case .occurrences:
            guard searchVectors, needle.utf8.elementsEqual(occurrenceNeedle.utf8) else { return [] }
            return [QueryHit(member: 1, start: 0, end: 208), QueryHit(member: 2, start: 2, end: 416)]
        case .literal, .expression: return []
        }
    }
    func structureObject(_ mode: AtlasSavedSearchMode, hit: QueryHit) -> [String: Any] {
        let name: String, kind: String, language: String, slug: Any
        switch mode {
        case .symbols:
            name = "crate::banana"; kind = "function"; language = "rust"; slug = NSNull()
        case .headings:
            name = longHeading ? String(repeating: "A", count: 3000) : "Café Details"
            kind = "heading"; language = "markdown"
            slug = longHeading ? String(repeating: "a", count: 3000) : "caf-details"
        case .occurrences:
            name = occurrenceNeedle; kind = "identifier-occurrence"; language = "text"; slug = NSNull()
        case .literal, .expression: preconditionFailure("non-structural fixture")
        }
        return ["name": name, "kind": kind, "language": language, "candidate_id": "1",
            "heading_slug": slug, "evidence_level": mode.structure!.evidence,
            "evidence_range": ["start": String(hit.start), "end": String(mode == .symbols ? 209 : hit.end)]]
    }
    func structureHitObject(_ id: UInt64, mode: AtlasSavedSearchMode, needle: String) -> [String: Any] {
        let hit = structureHits(mode, needle: needle)[Int(id - 1)], member = members[Int(hit.member)]
        return ["hit_id": String(id), "member": String(hit.member), "file_id": String(hit.member + 1),
            "source_revision": String(hit.member + 1), "source_digest": member.digest!,
            "path": path(member.path, member.display),
            "original_range": ["start": String(hit.start), "end": String(hit.end)],
            "structure": structureObject(mode, hit: hit)]
    }
    func structurePage(_ owner: UInt64, generation: UInt64, mode: AtlasSavedSearchMode = .symbols,
        nameMode: AtlasSearchNameMode = .contains, needle: String = "banana", start: UInt64 = 0,
        limit: UInt64 = 64) -> [String: Any] {
        let hits = structureHits(mode, needle: needle), end = min(UInt64(hits.count), start + limit)
        let indices: [Int] = mode == .symbols ? [1] : (mode == .headings ? [0, 2, 3] : [0, 1, 2, 3])
        let bytes = indices.compactMap { members[$0].bytes }.reduce(0) { $0 + $1.count }
        let searched = mode == .symbols ? 1 : (mode == .headings ? 2 : 4)
        let unsupported = mode == .symbols ? 3 : (mode == .headings ? 2 : 0)
        let metadata = hits.reduce(0) { total, hit in
            let object = structureObject(mode, hit: hit)
            return total + (object["name"] as! String).utf8.count + ((object["heading_slug"] as? String)?.utf8.count ?? 0)
        }
        var out = header(owner, "structure")
        out.merge([
            "selection_namespace": "saved-structure", "structure_generation": String(generation),
            "needle": needle, "mode": mode.structure!.wireMode, "requested_mode": mode.rawValue,
            "realized_mode": mode.structure!.evidence, "evidence_level": mode.structure!.evidence,
            "semantic_complete": false, "name_case": "sensitive", "name_mode": nameMode.rawValue,
            "scope": "whole-saved-archive", "completeness_basis": "admitted-extraction-processing",
            "index_used": false, "index_policy": "direct-verified-member-extraction",
            "state": "complete", "complete": false, "matches_seen_exact": false, "truncated": false,
            "max_hits": "4096", "max_source_bytes": "67108864",
            "retained_hits": String(hits.count), "matches_seen": String(hits.count),
            "members_visited": "5", "members_loaded": String(indices.count), "files_searched": String(searched),
            "unsupported_files": String(unsupported), "unavailable_files": "1",
            "source_limited_files": "0", "extraction_limited_files": "0", "metadata_omitted": "0",
            "loaded_bytes": String(bytes), "peak_source_bytes": String(indices.compactMap { members[$0].bytes?.count }.max()!),
            "metadata_bytes": String(metadata), "member_bytes_read": String(bytes), "member_read_calls": String(indices.count),
            "retained_source_payload_bytes": "0",
            "hits": (start..<end).map { structureHitObject($0 + 1, mode: mode, needle: needle) },
            "next_offset": end < UInt64(hits.count) ? String(end) as Any : NSNull()
        ]) { _, next in next }
        if mode == .occurrences {
            out["includes_comments_and_literals"] = true; out["compiler_resolved"] = false
            out["token_policy"] = "ascii-alphanumeric-underscore-dollar-plus-nonascii-nonwhitespace-noncontrol-v1"
            out["name_syntax"] = "single-token-no-leading-ascii-digit"; out["unicode_normalization"] = "none"
        }
        return out
    }
    private func readerHeader(_ owner: UInt64, member: Member, command: String) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": command, "owner": String(owner),
         "file_id": "1", "source_revision": "1", "captured_bytes": String(member.bytes!.count), "encoding": member.encoding,
         "path": path(member.path, member.display), "capture_origin": "host-supplied", "capture_sha256": member.digest!,
         "additional_source_bytes_read": "0", "native_presented": false]
    }
    private func readerInfo(_ owner: UInt64, member: Member) -> [String: Any] {
        var out = readerHeader(owner, member: member, command: "info")
        out["initial_source_bytes_read"] = "0"; out["initial_read_calls"] = "0"
        out["accepted_query_generation"] = NSNull(); out["accepted_outline_generation"] = NSNull()
        out["accepted_document_generation"] = NSNull()
        return out
    }
    private func window(_ owner: UInt64, member: Member, offset: UInt64, limit: UInt64) -> [String: Any] {
        let bytes = member.bytes!, end = min(UInt64(bytes.count), offset + limit)
        // The real extent decoder excludes a recognized file BOM from its
        // visible range while retaining the original requested byte range.
        // These fixed vectors otherwise use scalar-aligned window boundaries.
        let header: UInt64
        if member.encoding == "utf8" && bytes.starts(with: [0xef, 0xbb, 0xbf]) { header = 3 }
        else if member.encoding == "utf16le" && bytes.starts(with: [0xff, 0xfe]) { header = 2 }
        else if member.encoding == "utf16be" && bytes.starts(with: [0xfe, 0xff]) { header = 2 }
        else { header = 0 }
        let visibleStart = max(offset, header), visibleEnd = max(end, visibleStart)
        let raw = bytes[Int(visibleStart)..<Int(visibleEnd)]
        var out = readerHeader(owner, member: member, command: "window")
        out["requested_original_range"] = ["start": String(offset), "end": String(end)]
        out["visible_range"] = ["start": String(visibleStart), "end": String(visibleEnd)]
        out["text_kind"] = "logical-captured-text-not-shaped"
        // Fixture text decoding does not derive match coordinates: every hit
        // range and witness comes from the fixed transport vectors above.
        if member.encoding == "utf16le" || member.encoding == "utf16be" {
            out["text"] = String(data: Data(raw),
                encoding: member.encoding == "utf16le" ? .utf16LittleEndian : .utf16BigEndian)!
            out["has_replacements"] = false
        } else {
            out["text"] = String(decoding: raw, as: UTF8.self)
            out["has_replacements"] = String(bytes: raw, encoding: .utf8) == nil
        }
        out["original_hex"] = hex(raw)
        out["range_limited"] = false
        out["boundaries_adjusted"] = visibleStart != offset || visibleEnd != end
        out["first_physical_line"] = NSNull(); out["selection"] = NSNull()
        out["next_offset"] = visibleEnd < UInt64(bytes.count) ? String(visibleEnd) as Any : NSNull()
        return out
    }
    private func handoff(_ saved: UInt64, reader: UInt64, ordinal: UInt64,
        generation: UInt64?, link: UInt64?, side: AtlasSavedLinkSide?,
        query: UInt64? = nil, hitID: UInt64? = nil, expression: UInt64? = nil,
        structure: UInt64? = nil) -> String? {
        let member = members[Int(ordinal)]
        guard let bytes = member.bytes else { return nil }
        readers[reader] = member
        log.append("handoff:\(reader)")
        var out = header(saved, generation == nil ? "open-reader" : "relationships-open-reader")
        var nested = readerInfo(reader, member: member)
        out["member"] = String(ordinal); out["file_id"] = String(ordinal + 1); out["source_revision"] = String(ordinal + 1)
        out["source_digest"] = member.digest!; out["source_observation"] = "verified-saved-member"
        out["live_source_reopened"] = false; out["member_bytes_read"] = String(bytes.count); out["member_read_calls"] = "1"
        out["selection"] = side == .evidence ? ["start": "0", "end": String(bytes.count)] as Any : NSNull()
        out["reader_owner"] = String(reader)
        var windowStart: UInt64 = 0, windowBytes: UInt64 = 16 * 1024
        if let query, let hitID, let session = sessions[saved], let needle = session.needle {
            let hit = searchHits(needle, caseSensitive: session.caseSensitive)[Int(hitID - 1)]
            let original = Array(bytes[Int(hit.start)..<Int(hit.end)])
            let spelling: String
            if member.encoding == "utf8" { spelling = String(decoding: original, as: UTF8.self) }
            else {
                let units = stride(from: 0, to: original.count, by: 2).map { offset in
                    member.encoding == "utf16le"
                        ? UInt16(original[offset]) | UInt16(original[offset + 1]) << 8
                        : UInt16(original[offset]) << 8 | UInt16(original[offset + 1])
                }
                spelling = String(decoding: units, as: UTF16.self)
            }
            out["selection"] = ["query_generation": String(query), "hit_id": String(hitID),
                "case_sensitive": session.caseSensitive,
                "mode": session.caseSensitive ? "exact-decoded-literal" : "unicode-lowercase-literal", "text": spelling,
                "domain": "original-source-bytes", "original_range": ["start": String(hit.start), "end": String(hit.end)],
                "original_hex": hex(original)]
            windowStart = hit.start - min(hit.start, 132)
            windowBytes = max(4, min(UInt64(bytes.count), hit.end + 132) - windowStart)
        }
        if let expression, let hitID {
            let hit = searchHits("ana")[Int(hitID - 1)]
            out["command"] = "expression-open-reader"
            out["selection_namespace"] = "saved-expression"; out["expression_generation"] = String(expression)
            out["primary_needle"] = "ana"
            out["selection"] = ["expression_generation": String(expression), "hit_id": String(hitID),
                "domain": "original-source-bytes", "original_range": ["start": String(hit.start), "end": String(hit.end)],
                "original_hex": hex(bytes[Int(hit.start)..<Int(hit.end)])]
            windowStart = hit.start - min(hit.start, 132)
            windowBytes = max(4, min(UInt64(bytes.count), hit.end + 132) - windowStart)
        }
        if let structure, let hitID, let session = sessions[saved], let mode = session.structureMode,
            let nameMode = session.structureNameMode, let needle = session.structureNeedle {
            let hit = structureHits(mode, needle: needle)[Int(hitID - 1)]
            out["command"] = "structure-open-reader"; out["selection_namespace"] = "saved-structure"
            out["structure_generation"] = String(structure); out["needle"] = needle
            out["requested_mode"] = mode.rawValue; out["name_mode"] = nameMode.rawValue
            out["structure"] = structureObject(mode, hit: hit)
            out["selection"] = ["structure_generation": String(structure), "hit_id": String(hitID),
                "domain": "original-source-bytes", "original_range": ["start": String(hit.start), "end": String(hit.end)],
                "original_hex": hit.end - hit.start <= 2048 ? hex(bytes[Int(hit.start)..<Int(hit.end)]) as Any : NSNull()]
            windowStart = hit.start - min(hit.start, 132)
            windowBytes = hit.end - hit.start <= 2048
                ? max(4, min(UInt64(bytes.count), hit.end + 132) - windowStart) : 16 * 1024
        }
        if let generation, let link, let side {
            out["relationship_generation"] = String(generation); out["link_id"] = String(link); out["side"] = side.rawValue
            out["offset"] = "0"; out["evidence_precision"] = "whole-captured-markdown-document"
            let accepted = linkObject(link)
            for field in ["fragment_resolution", "fragment_slug", "target_range"] { out[field] = accepted[field] }
            if headingVectors && link == 1 && side == .target {
                out["offset"] = "207"; out["selection"] = accepted["target_range"]
                windowStart = 79
            }
            if lineEncoding != nil && link == 1 && side == .target {
                out["offset"] = String(lineBounds.0); out["selection"] = accepted["target_range"]
                windowStart = lineBounds.0 > 128 ? lineBounds.0 - 128 : 0
            }
        }
        if let fault {
            self.fault = nil
            switch fault {
            case "lost": return nil // Destination accepted before a missing receipt.
            case "owner": out["owner"] = String(saved + 1)
            case "archive": out["archive_digest"] = String(repeating: "e", count: 64)
            case "generation": out["relationship_generation"] = String((generation ?? 0) + 1)
            case "selection": out["selection"] = ["start": "1", "end": String(bytes.count)]
            case "heading-range": out["target_range"] = ["start": "208", "end": "223"]
            case "heading-offset": out["offset"] = "0"
            case "heading-slug": out["fragment_slug"] = "caf-other"
            case "heading-resolution": out["fragment_resolution"] = "missing-heading"
            case "digest": out["source_digest"] = String(repeating: "e", count: 64)
            case "live": out["live_source_reopened"] = true
            case "nested-owner": nested["owner"] = "999"
            case "nested-path": nested["path"] = path(Array("another.rs".utf8), member.display)
            case "inherited-query": nested["accepted_query_generation"] = "1"
            case "many-read-calls": out["member_read_calls"] = "1024" // Interrupted reads can exceed source length.
            case "page-bytes": wrongPage.insert(reader)
            case "structure-namespace": out["selection_namespace"] = "saved-expression"
            case "structure-generation": out["structure_generation"] = String((structure ?? 0) + 1)
            case "structure-mode": out["requested_mode"] = "symbols"
            case "structure-name-mode": out["name_mode"] = "prefix"
            case "structure-needle": out["needle"] = "different"
            case "structure-witness":
                var witness = out["structure"] as! [String: Any]
                witness["evidence_level"] = "compiler-resolved"
                out["structure"] = witness
            case "structure-selection":
                var selected = out["selection"] as! [String: Any]
                selected["query_generation"] = selected.removeValue(forKey: "structure_generation")
                out["selection"] = selected
            case "structure-large-hex":
                var selected = out["selection"] as! [String: Any]
                selected["original_hex"] = ""
                out["selection"] = selected
            case "expression-namespace": out["selection_namespace"] = "saved-literal"
            case "expression-generation": out["expression_generation"] = String((expression ?? 0) + 1)
            case "expression-primary": out["primary_needle"] = "banana"
            case "expression-selection":
                var selected = out["selection"] as! [String: Any]
                selected["query_generation"] = selected.removeValue(forKey: "expression_generation")
                out["selection"] = selected
            case "hit-case", "hit-mode", "hit-text", "hit-text-missing", "hit-policy-missing":
                var selected = out["selection"] as! [String: Any]
                switch fault {
                case "hit-case": selected["case_sensitive"] = true
                case "hit-mode": selected["mode"] = "exact-decoded-literal"
                case "hit-text": selected["text"] = "ANA"
                case "hit-text-missing": selected.removeValue(forKey: "text")
                default:
                    for field in ["case_sensitive", "mode", "text"] { selected.removeValue(forKey: field) }
                }
                out["selection"] = selected
            case "hit-generation", "hit-id", "hit-domain", "hit-range", "hit-hex":
                var selected = out["selection"] as! [String: Any]
                switch fault {
                case "hit-generation": selected["query_generation"] = String((query ?? 0) + 1)
                case "hit-id": selected["hit_id"] = String((hitID ?? 0) + 1)
                case "hit-domain": selected["domain"] = "decoded-text"
                case "hit-range": selected["original_range"] = ["start": "0", "end": "3"]
                default: selected["original_hex"] = "000000"
                }
                out["selection"] = selected
            case "hit-context": windowStart = 0; windowBytes = 16 * 1024
            default: preconditionFailure("unexpected handoff fault")
            }
        }
        out["reader"] = nested
        out["window"] = window(reader, member: member, offset: windowStart, limit: windowBytes)
        return encode(out)
    }
    var savedTransport: AtlasSavedTransport {
        AtlasSavedTransport(create: { [self] in locked {
            let id = nextSaved; nextSaved += 1; log.append("saved-create:\(id)"); return id
        } }, run: { [self] handle, request in
            precondition(!Thread.isMainThread)
            switch request {
            case .openMember, .openRelationship, .openHit, .openExpressionHit, .openStructureHit:
                let pause = locked { let value = blocked; blocked = false; return value }
                if pause { entered.signal(); precondition(resume.wait(timeout: .now() + .seconds(5)) == .success) }
            case .search, .countedSearch, .matchingSearch, .expressionSearch, .countedExpressionSearch, .structureSearch:
                let pause = locked { let value = blockedSearch; blockedSearch = false; return value }
                if pause { entered.signal(); precondition(resume.wait(timeout: .now() + .seconds(5)) == .success) }
            case .attachIndex, .detachIndex:
                let pause = locked { let value = blockedIndex; blockedIndex = false; return value }
                if pause { entered.signal(); precondition(resume.wait(timeout: .now() + .seconds(5)) == .success) }
            default: break
            }
            return locked {
                switch request {
                case .open(let path):
                    log.append("open:\(handle)")
                    let refused = fault == "open"
                    let lostReply = fault == "open-reply"
                    if refused || lostReply { fault = nil }
                    sessions[handle] = Session(path: path, refused: refused)
                    return refused || lostReply ? nil : encode(info(handle, path: path))
                case .info:
                    log.append("info:\(handle)")
                    guard let session = sessions[handle], !session.refused else { return nil }
                    if fault == "index-info" { fault = nil; return nil }
                    return encode(info(handle, path: session.path, generation: session.generation, query: session.queryGeneration,
                        bytes: session.readBytes, calls: session.readCalls, expression: session.expressionGeneration,
                        expressionAttempt: session.expressionAttempt, index: session.indexGeneration,
                        pin: session.indexPin, indexAttempt: session.indexAttempt, indexPageBytes: session.indexPageBytes,
                        structure: session.structureGeneration, structureAttempt: session.structureAttempt))
                case .attachIndex(let generation, let path, let pin):
                    log.append("index-attach:\(handle):\(generation):\(path)")
                    guard var session = sessions[handle], generation > session.indexAttempt else { return nil }
                    session.indexAttempt = generation
                    if fault == "index-refused" {
                        fault = nil; sessions[handle] = session
                        return encode(["schema": "fcb.saved-repository/1", "status": "error", "owner": String(handle),
                            "error": ["code": "SNAPSHOT_INDEX_PIN_MISMATCH"]])
                    }
                    session.indexGeneration = generation; session.indexPin = pin; session.indexPageBytes = 0
                    sessions[handle] = session
                    if fault == "index-lost" { fault = nil; return nil }
                    if fault == "index-unknown" { fault = "index-info"; return nil }
                    return encode(indexAttachment(handle, generation: generation, pin: pin))
                case .detachIndex(let generation):
                    log.append("index-detach:\(handle):\(generation)")
                    guard var session = sessions[handle], generation > session.indexAttempt else { return nil }
                    session.indexAttempt = generation; session.indexGeneration = nil; session.indexPin = nil
                    sessions[handle] = session
                    if fault == "index-detach-lost" { fault = nil; return nil }
                    if fault == "index-unknown" { fault = "index-info"; return nil }
                    var out = header(handle, "index-detach")
                    out["accepted_index_generation"] = NSNull(); out["last_index_attempt"] = String(generation)
                    return encode(out)
                case .members(let start, let limit):
                    guard sessions[handle] != nil, start <= 5 else { return nil }
                    log.append("members:\(handle):\(start):\(limit)")
                    var page = memberPage(handle, start: start, limit: limit)
                    if limit == 1, fault == "metadata" {
                        fault = nil
                        var rows = page["members"] as! [[String: Any]]
                        rows[0]["source_digest"] = String(repeating: "e", count: 64)
                        page["members"] = rows
                    }
                    return encode(page)
                case .prepareRelationships(let generation, let links, let bytes):
                    precondition(links == 4096 && bytes == AtlasSavedWire.relationshipBytes)
                    log.append("prepare:\(handle):\(generation)")
                    guard var session = sessions[handle] else { return nil }
                    if fault == "build" { fault = nil; return nil }
                    session.generation = generation
                    session.readBytes += UInt64(source.count + members[2].bytes!.count
                        + (headingVectors ? 2 * headingBytes.count : 0) + (lineEncoding != nil ? members[3].bytes!.count : 0))
                    session.readCalls += headingVectors ? 4 : (lineEncoding != nil ? 3 : 2)
                    sessions[handle] = session
                    return encode(relationships(handle, generation: generation, preparing: true))
                case .relationships(let generation, let direction, let member, let start, let limit):
                    log.append("links:\(handle):\(generation):\(direction.rawValue):\(start)")
                    guard sessions[handle]?.generation == generation else { return nil }
                    return encode(relationships(handle, generation: generation, direction: direction,
                        member: member, start: start, limit: limit))
                case .openMember(let reader, let member):
                    guard sessions[handle] != nil, member < 5 else { return nil }
                    return handoff(handle, reader: reader, ordinal: member, generation: nil, link: nil, side: nil)
                case .openRelationship(let reader, let generation, let link, let side):
                    guard sessions[handle]?.generation == generation, (1...4).contains(link),
                          side == .evidence || link <= 2 else { return nil }
                    return handoff(handle, reader: reader, ordinal: side == .evidence ? 0 : 3,
                        generation: generation, link: link, side: side)
                case .search(let generation, let needle, let matches),
                     .countedSearch(let generation, let needle, let matches),
                     .matchingSearch(let generation, let needle, _, _, let matches):
                    precondition(matches == 4096)
                    let caseSensitive: Bool, countAllMatches: Bool
                    switch request {
                    case .matchingSearch(_, _, let matching, let counting, _):
                        caseSensitive = matching; countAllMatches = counting
                    case .countedSearch: caseSensitive = true; countAllMatches = true
                    default: caseSensitive = true; countAllMatches = false
                    }
                    log.append("search:\(handle):\(generation):\(needle)")
                    guard var session = sessions[handle] else { return nil }
                    if fault == "search" { fault = nil; return nil }
                    if fault == "index-page-failed" {
                        fault = nil; session.indexPageBytes += 16; sessions[handle] = session
                        return encode(["schema": "fcb.saved-repository/1", "status": "error", "owner": String(handle),
                            "error": ["code": "PAGED_INDEX_CHANGED_PAGE"]])
                    }
                    session.queryGeneration = generation; session.needle = needle
                    session.caseSensitive = caseSensitive; session.countAllMatches = countAllMatches
                    session.queryIndexGeneration = caseSensitive ? session.indexGeneration : nil
                    session.queryIndexPin = caseSensitive ? session.indexPin : nil
                    session.readBytes += UInt64(members.compactMap(\.bytes).reduce(0) { $0 + $1.count }); session.readCalls += 4
                    sessions[handle] = session
                    if fault == "search-lost" { fault = nil; return nil }
                    return encode(searchPage(handle, generation: generation, needle: needle, searching: true,
                        index: session.queryIndexGeneration, pin: session.queryIndexPin,
                        countAllMatches: session.countAllMatches, caseSensitive: session.caseSensitive))
                case .results(let generation, let start, let limit):
                    log.append("results:\(handle):\(generation):\(start)")
                    guard let session = sessions[handle], session.queryGeneration == generation, let needle = session.needle else { return nil }
                    if fault == "results" { fault = nil; return nil }
                    var page = searchPage(handle, generation: generation, needle: needle, start: start, limit: limit,
                        index: session.queryIndexGeneration, pin: session.queryIndexPin,
                        countAllMatches: session.countAllMatches, caseSensitive: session.caseSensitive)
                    if fault == "index-query-pin" { fault = nil; page["index_pin"] = String(repeating: "f", count: 64) }
                    if fault == "count-policy" { fault = nil; page["count_all_matches"] = !session.countAllMatches }
                    if fault == "case-policy" { fault = nil; page["case_sensitive"] = !session.caseSensitive }
                    return encode(page)
                case .clearResults(let generation):
                    log.append("clear-search:\(handle):\(generation)")
                    sessions[handle]?.queryGeneration = nil; sessions[handle]?.needle = nil
                    if fault == "clear-lost" { fault = nil; return nil }
                    var out = header(handle, "clear-results")
                    out["accepted_query_generation"] = NSNull(); out["retained_hits"] = "0"
                    return encode(out)
                case .openHit(let reader, let generation, let id):
                    guard let session = sessions[handle], session.queryGeneration == generation,
                          let needle = session.needle, id > 0,
                          id <= UInt64(searchHits(needle, caseSensitive: session.caseSensitive).count) else { return nil }
                    return handoff(handle, reader: reader,
                        ordinal: searchHits(needle, caseSensitive: session.caseSensitive)[Int(id - 1)].member,
                        generation: nil, link: nil, side: nil, query: generation, hitID: id)
                case .expressionSearch(let generation, let query, let matches, let bytes),
                     .countedExpressionSearch(let generation, let query, let matches, let bytes):
                    precondition(matches == 4096 && bytes == AtlasSavedSearchWire.maximumSourceBytes)
                    log.append("expression:\(handle):\(generation):\(query)")
                    guard var session = sessions[handle], generation > session.expressionAttempt else { return nil }
                    session.expressionAttempt = generation
                    if fault == "expression" {
                        fault = nil; sessions[handle] = session
                        return encode(["schema": "fcb.saved-repository/1", "owner": String(handle), "status": "error",
                            "error": ["code": "QUERY_SYNTAX_ERROR"]])
                    }
                    session.expressionGeneration = generation; session.expression = query
                    if case .countedExpressionSearch = request { session.countAllExpressionMatches = true }
                    else { session.countAllExpressionMatches = false }
                    session.readBytes += UInt64(members.compactMap(\.bytes).reduce(0) { $0 + $1.count }); session.readCalls += 4
                    sessions[handle] = session
                    if fault == "expression-lost" { fault = nil; return nil }
                    return encode(expressionPage(handle, generation: generation, query: query,
                        countAllMatches: session.countAllExpressionMatches))
                case .expressionResults(let generation, let start, let limit):
                    log.append("expression-results:\(handle):\(generation):\(start)")
                    guard let session = sessions[handle], session.expressionGeneration == generation,
                          let query = session.expression else { return nil }
                    if fault == "expression-results" { fault = nil; return nil }
                    var page = expressionPage(handle, generation: generation, query: query, start: start, limit: limit,
                        countAllMatches: session.countAllExpressionMatches)
                    if fault == "expression-results-primary" { fault = nil; page["primary_needle"] = "banana" }
                    if fault == "expression-count-policy" { fault = nil; page["count_all_matches"] = !session.countAllExpressionMatches }
                    return encode(page)
                case .clearExpression(let generation):
                    guard var session = sessions[handle], generation > session.expressionAttempt else { return nil }
                    session.expressionAttempt = generation; session.expressionGeneration = nil; session.expression = nil
                    sessions[handle] = session; log.append("expression-clear:\(handle):\(generation)")
                    if fault == "expression-clear-lost" { fault = nil; return nil }
                    var out = header(handle, "expression-clear")
                    out["selection_namespace"] = "saved-expression"; out["expression_generation"] = String(generation)
                    out["accepted_expression_generation"] = NSNull(); out["retained_hits"] = "0"
                    return encode(out)
                case .openExpressionHit(let reader, let generation, let id):
                    guard let session = sessions[handle], session.expressionGeneration == generation,
                          let query = session.expression, id > 0, id <= (query == "ana path:raw" ? 2 : 4) else { return nil }
                    log.append("expression-open:\(handle):\(generation):\(id)")
                    return handoff(handle, reader: reader, ordinal: searchHits("ana")[Int(id - 1)].member,
                        generation: nil, link: nil, side: nil, hitID: id, expression: generation)
                case .structureSearch(let generation, let mode, let nameMode, let needle, let matches, let bytes):
                    precondition(matches == 4096 && bytes == AtlasSavedSearchWire.maximumSourceBytes)
                    precondition(mode != .occurrences || nameMode == .exact)
                    log.append("structure:\(handle):\(generation):\(mode.rawValue):\(nameMode.rawValue):\(needle)")
                    guard var session = sessions[handle], generation > session.structureAttempt else { return nil }
                    session.structureAttempt = generation
                    if fault == "structure" {
                        fault = nil; sessions[handle] = session
                        return encode(["schema": "fcb.saved-repository/1", "owner": String(handle), "status": "error",
                            "error": ["code": "REFERENCE_INVALID_NAME"]])
                    }
                    session.structureGeneration = generation; session.structureMode = mode
                    session.structureNameMode = nameMode; session.structureNeedle = needle
                    let page = structurePage(handle, generation: generation, mode: mode, nameMode: nameMode, needle: needle)
                    session.readBytes += UInt64(page["member_bytes_read"] as! String)!
                    session.readCalls += UInt64(page["member_read_calls"] as! String)!
                    sessions[handle] = session
                    if fault == "structure-lost" { fault = nil; return nil }
                    return encode(page)
                case .structureResults(let generation, let start, let limit):
                    log.append("structure-results:\(handle):\(generation):\(start)")
                    guard let session = sessions[handle], session.structureGeneration == generation,
                          let mode = session.structureMode, let nameMode = session.structureNameMode,
                          let needle = session.structureNeedle else { return nil }
                    if fault == "structure-results" { fault = nil; return nil }
                    var page = structurePage(handle, generation: generation, mode: mode, nameMode: nameMode,
                        needle: needle, start: start, limit: limit)
                    if fault == "structure-results-mode" { fault = nil; page["requested_mode"] = "symbols" }
                    return encode(page)
                case .clearStructure(let generation):
                    guard var session = sessions[handle], generation > session.structureAttempt else { return nil }
                    session.structureAttempt = generation; session.structureGeneration = nil
                    session.structureMode = nil; session.structureNameMode = nil; session.structureNeedle = nil
                    sessions[handle] = session; log.append("structure-clear:\(handle):\(generation)")
                    if fault == "structure-clear-lost" { fault = nil; return nil }
                    var out = header(handle, "structure-clear")
                    out["selection_namespace"] = "saved-structure"; out["structure_generation"] = String(generation)
                    out["accepted_structure_generation"] = NSNull(); out["retained_hits"] = "0"
                    return encode(out)
                case .openStructureHit(let reader, let generation, let id):
                    guard let session = sessions[handle], session.structureGeneration == generation,
                          let mode = session.structureMode, let needle = session.structureNeedle, id > 0,
                          id <= UInt64(structureHits(mode, needle: needle).count) else { return nil }
                    log.append("structure-open:\(handle):\(generation):\(id)")
                    return handoff(handle, reader: reader, ordinal: structureHits(mode, needle: needle)[Int(id - 1)].member,
                        generation: nil, link: nil, side: nil, hitID: id, structure: generation)
                case .clearRelationships(let generation):
                    sessions[handle]?.generation = nil
                    var out = header(handle, "relationships-clear")
                    out["accepted_relationship_generation"] = NSNull(); out["retained_links"] = "0"
                    log.append("clear:\(generation)")
                    return encode(out)
                }
            }
        }, cancel: { [self] handle in locked { log.append("saved-cancel:\(handle)") }; return true },
        close: { [self] handle in
            precondition(!Thread.isMainThread)
            locked { sessions.removeValue(forKey: handle); log.append("saved-close:\(handle)") }; return true
        }, retirementFailed: { fatalError("saved retirement failed") }, readerDomain: domain)
    }
    func readerTransport(domain: UUID? = nil) -> AtlasReaderTransport {
        AtlasReaderTransport(create: { [self] in locked {
            let id = nextReader; nextReader += 1; log.append("reader-create:\(id)"); return id
        } }, open: { [self] _, _, _ in locked { log.append("LIVE-OPEN") }; return nil },
        window: { [self] owner, offset, limit in
            precondition(!Thread.isMainThread)
            return locked {
                guard let member = readers[owner], offset <= UInt64(member.bytes!.count) else { return nil }
                var page = window(owner, member: member, offset: offset, limit: limit)
                log.append("reader-window:\(owner):\(offset)")
                if wrongPage.remove(owner) != nil {
                    let emitted = page["original_hex"] as! String
                    page["original_hex"] = String(repeating: "00", count: emitted.utf8.count / 2)
                }
                return encode(page)
            }
        }, lines: { _, _, _, _ in nil },
        cancel: { [self] owner in locked { log.append("reader-cancel:\(owner)") }; return true },
        close: { [self] owner in
            precondition(!Thread.isMainThread)
            locked { readers.removeValue(forKey: owner); log.append("reader-close:\(owner)") }; return true
        }, retirementFailed: { fatalError("reader retirement failed") },
        fork: { [self] source, destination in locked {
            guard let member = readers[source] else { return nil }
            readers[destination] = member
            return encode(readerInfo(destination, member: member))
        } }, comparison: AtlasReaderComparisonTransport(domain: domain ?? self.domain, compare: { _, _, _, _, _ in nil }))
    }
}

#if !FCB_SAVED_SEARCH_TESTS
@main private enum AtlasSavedRepositoryTests {
    @MainActor private static func wait(_ message: String, _ ready: () -> Bool) async {
        for _ in 0..<2000 {
            if ready() { return }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        preconditionFailure(message)
    }
    @MainActor private static func opened(_ fixture: SavedRepositoryFixture) async -> AtlasSavedRepositoryModel {
        let model = AtlasSavedRepositoryModel(transport: fixture.savedTransport)
        model.open(path: "/saved/repository.fcbs", accessLease: nil)
        await wait("archive did not finish opening") { !model.busy }
        precondition(model.info?.identity.owner == 40 && model.members?.items.count == 5)
        return model
    }
    @MainActor private static func prepared(_ fixture: SavedRepositoryFixture) async -> AtlasSavedRepositoryModel {
        let model = await opened(fixture)
        model.prepareRelationships()
        await wait("relationships did not finish preparing") { !model.busy }
        precondition(model.relationshipGeneration == 1 && model.relationships?.links.count == 4)
        return model
    }
    private static func rejected(_ message: String, _ operation: () throws -> Void) {
        do { try operation(); preconditionFailure(message) } catch { }
    }
    @MainActor private static func refused(_ message: String, _ operation: @MainActor () async throws -> Void) async {
        do { try await operation(); preconditionFailure(message) } catch { }
    }
    @MainActor static func main() async throws {
        try decoderContracts()
        try await actualModelNavigationAndIndependentReaders()
        try await capturedHeadingNavigationAndFrozenReceipts()
        try await capturedSourceLineNavigationAndAlignment()
        await failedReplacementAndGenerationRecovery()
        await savedExportDigestPinsReplacement()
        await staleRowsAndForeignRegistryCannotTriggerHandoff()
        await invalidMetadataAndReceiptsRetireDestinations()
        await closedArchiveRejectsLateHandoffAndDrainsItsLease()
        print("Saved repository decoder, backlinks, raw names, retained navigation, replacement, stale identity and cancellation regressions passed.")
    }

    private static func decoderContracts() throws {
        let f = SavedRepositoryFixture()
        let info = try AtlasSavedInfo.decode(f.encode(f.info(40)), handle: 40,
            expectedPath: Array("/saved/repository.fcbs".utf8), fresh: true)
        let members = try AtlasSavedMembers.decode(f.encode(f.memberPage(40, start: 0, limit: 64)),
            identity: info.identity, start: 0, limit: 64)
        precondition(members.items[1].path.bytes == f.rawPath)
        precondition(!members.items[4].captured && members.items[4].observedBytes == 99)
        let page = try AtlasSavedRelationships.decode(f.encode(f.relationships(40, generation: 1)),
            identity: info.identity, generation: 1, direction: .all, member: nil, start: 0, limit: 64)
        precondition(!page.complete && !page.truncated && page.unsupported == 1 && page.unavailable == 1)
        precondition(page.markdown == page.scanned + page.unsupported + page.unavailable + page.sourceLimited)
        precondition(page.links[0].evidenceEnd == UInt64(f.source.count) && page.links[0].fragmentUnresolved)
        precondition(page.links[3].destination.isEmpty && !page.links[3].canOpenTarget)
        precondition(!page.links[2].canOpenTarget)
        let backlinks = try AtlasSavedRelationships.decode(f.encode(f.relationships(40, generation: 1,
            direction: .incoming, member: 3, start: 1, limit: 1)), identity: info.identity, generation: 1,
            direction: .incoming, member: 3, start: 1, limit: 1)
        precondition(backlinks.links.map(\.id) == [2] && backlinks.matching == 2 && backlinks.next == nil)
        // This occurrence can follow omitted destinations; it is not the
        // retained global-link cap and must not be narrowed to 4096.
        var occurrence = f.linkObject(1)
        occurrence["source_link_id"] = "4097"; occurrence["evidence_range"] = ["start": "0", "end": "262144"]
        _ = try JSONDecoder().decode(AtlasSavedLink.self, from: Data(f.encode(occurrence).utf8))

        var limited = f.relationships(40, generation: 1)
        for key in ["scanned_markdown_members", "unsupported_markdown_members", "retained_links", "links_seen",
                    "matching_links", "member_bytes_read", "member_read_calls"] { limited[key] = "0" }
        limited["source_limited_members"] = "2"; limited["links"] = [[String: Any]]()
        let omitted = try AtlasSavedRelationships.decode(f.encode(limited), identity: info.identity,
            generation: 1, direction: .all, member: nil, start: 0, limit: 64)
        precondition(!omitted.complete && omitted.links.isEmpty && omitted.sourceLimited == 2)
        for field in ["owner", "archive_digest", "evidence_precision", "relationships_complete",
                      "retained_source_payload_bytes", "filter_member", "next_offset"] {
            var bad = f.relationships(40, generation: 1)
            switch field {
            case "owner": bad[field] = "41"
            case "archive_digest": bad[field] = String(repeating: "e", count: 64)
            case "evidence_precision": bad[field] = "exact-link-token"
            case "relationships_complete": bad[field] = true
            case "retained_source_payload_bytes": bad[field] = "1"
            case "filter_member": bad[field] = "0"
            default: bad[field] = "1"
            }
            rejected("forged relationship page accepted: \(field)") {
                _ = try AtlasSavedRelationships.decode(f.encode(bad), identity: info.identity,
                    generation: 1, direction: .all, member: nil, start: 0, limit: 64)
            }
        }
        for field in ["range", "source_file_id", "fragment", "occurrence", "target", "number"] {
            var bad = f.linkObject(1)
            switch field {
            case "range": bad["evidence_range"] = ["start": "1", "end": String(f.source.count)]
            case "source_file_id": bad[field] = "2"
            case "fragment": bad["fragment_resolution"] = "none"
            case "occurrence": bad["source_link_id"] = "131073"
            case "target": bad["target_member"] = NSNull()
            default: bad["link_id"] = 1 // Wire integers are decimal strings.
            }
            rejected("forged link accepted: \(field)") {
                _ = try JSONDecoder().decode(AtlasSavedLink.self, from: Data(f.encode(bad).utf8))
            }
        }
        let headings = SavedRepositoryFixture(headingVectors: true)
        for field in ["missing-range", "empty-range", "oversized-range", "missing-slug", "control-slug", "unqualified-range"] {
            var bad = headings.linkObject(1)
            switch field {
            case "missing-range": bad["target_range"] = NSNull()
            case "empty-range": bad["target_range"] = ["start": "207", "end": "207"]
            case "oversized-range": bad["target_range"] = ["start": "207", "end": "262145"]
            case "missing-slug": bad["fragment_slug"] = NSNull()
            case "control-slug": bad["fragment_slug"] = "caf\0details"
            default: bad["fragment_resolution"] = "missing-heading"
            }
            rejected("invalid qualified heading metadata accepted: \(field)") {
                _ = try JSONDecoder().decode(AtlasSavedLink.self, from: Data(headings.encode(bad).utf8))
            }
        }
        for state in ["missing-heading", "ambiguous", "limited", "unsupported"] {
            var outcome = headings.linkObject(1)
            outcome["fragment_resolution"] = state; outcome["target_range"] = NSNull()
            let unresolved = try JSONDecoder().decode(AtlasSavedLink.self, from: Data(headings.encode(outcome).utf8))
            precondition(unresolved.canOpenTarget && unresolved.fragmentUnresolved && unresolved.targetRange == nil)
        }
        var line = f.linkObject(1)
        line["fragment_resolution"] = "resolved-lines"
        line["target_range"] = ["start": "270000", "end": "270006"]
        let sourceLine = try JSONDecoder().decode(AtlasSavedLink.self, from: Data(f.encode(line).utf8))
        precondition(sourceLine.targetRange?.start == 270_000 && !sourceLine.fragmentUnresolved
            && sourceLine.targetActionLabel == "Read captured source lines",
            "Source line metadata uses the archive member cap, not the Markdown document cap")
        for field in ["empty-range", "member-cap", "missing-range", "missing-slug", "invalid-slug", "markdown-target"] {
            var bad = line
            switch field {
            case "empty-range": bad["target_range"] = ["start": "270000", "end": "270000"]
            case "member-cap": bad["target_range"] = ["start": "270000", "end": "1048577"]
            case "missing-range": bad["target_range"] = NSNull()
            case "missing-slug": bad["fragment_slug"] = NSNull()
            case "invalid-slug": bad["fragment_slug"] = "L2-L1"
            default: bad["target_path"] = f.path(Array("docs/guide.MD".utf8), "docs/guide.MD")
            }
            rejected("invalid source line metadata accepted: \(field)") {
                _ = try JSONDecoder().decode(AtlasSavedLink.self, from: Data(f.encode(bad).utf8))
            }
        }
        var absent = line; absent["fragment_resolution"] = "missing-line"; absent["target_range"] = NSNull()
        let missingLine = try JSONDecoder().decode(AtlasSavedLink.self, from: Data(f.encode(absent).utf8))
        precondition(missingLine.canOpenTarget && missingLine.fragmentUnresolved
            && missingLine.targetNavigationDescription.contains("absent") && missingLine.targetRange == nil)
        var rawMismatch = f.info(40)
        rawMismatch["archive_path"] = f.path(Array("/saved/other.fcbs".utf8), "/saved/repository.fcbs")
        rejected("display path rebound exact archive bytes") {
            _ = try AtlasSavedInfo.decode(f.encode(rawMismatch), handle: 40,
                expectedPath: Array("/saved/repository.fcbs".utf8), fresh: true)
        }
    }

    @MainActor private static func actualModelNavigationAndIndependentReaders() async throws {
        let f = SavedRepositoryFixture(), model = await prepared(f)
        let info = model.info!, all = model.relationships!, source = all.links[0]
        let evidence = try await model.openLink(source, from: all, side: .evidence, readerTransport: f.readerTransport())
        precondition(evidence.reader.page?.start == 3 && evidence.reader.page?.boundariesAdjusted == true)
        precondition(evidence.reader.page?.originalHex == f.hex(f.source.dropFirst(3)))
        precondition(evidence.evidenceLabel.contains("0..<\(f.source.count)") && !evidence.reader.canRefresh)
        let code = model.members!.items[3]
        model.selectMember(code, from: info.identity)
        model.loadRelationships(direction: .incoming)
        await wait("backlinks did not load") { !model.busy }
        let incoming = model.relationships!
        precondition(incoming.links.map(\.id) == [1, 2] && incoming.member == 3 && incoming.matching == 2)
        f.corruptNext("many-read-calls")
        let target = try await model.openLink(incoming.links[0], from: incoming, side: .target, readerTransport: f.readerTransport())
        precondition(target.reader.page?.originalHex == "616263" && target.reader.page?.start == 0)
        precondition(target.evidenceLabel.contains("no supported navigation") && !target.reader.canRefresh)
        let raw = try await model.openMember(model.members!.items[1], from: info.identity, readerTransport: f.readerTransport())
        precondition(raw.reader.page?.identity.pathBytes == f.rawPath)
        precondition(raw.reader.page?.originalHex == f.hex("changed bytes\n".utf8))
        model.closeArchive()
        precondition(model.info == nil && model.relationships == nil)
        target.reader.offsetInput = "1"; target.reader.goToByte()
        await wait("independent target failed after archive close") { !target.reader.busy }
        precondition(target.reader.page?.originalHex == "6263")
        target.reader.refreshFromDisk()
        precondition(!f.calls.contains("LIVE-OPEN"))
        await wait("archive did not retire independently") { f.calls.contains("saved-close:40") }
        evidence.reader.close(); target.reader.close(); raw.reader.close(); model.close()
    }

    @MainActor private static func capturedSourceLineNavigationAndAlignment() async throws {
        for encoding in ["utf8", "utf16le", "utf16be"] {
            let f = SavedRepositoryFixture(lineEncoding: encoding), model = await prepared(f)
            let page = model.relationships!, link = page.links[0]
            let expected = f.lineBounds
            precondition(link.fragmentResolution == .resolvedLines && link.fragmentSlug == "L2"
                && link.targetRange?.start == expected.0 && link.targetRange?.end == expected.1)
            let capture = try await model.openLink(link, from: page, side: .target, readerTransport: f.readerTransport())
            let bytes = f.members[3].bytes!, bom = encoding == "utf8" ? 0 : 2
            precondition(capture.reader.page?.identity.encoding == encoding
                && capture.reader.page?.originalHex == f.hex(bytes.dropFirst(bom)))
            precondition(capture.evidenceLabel.contains("source line 2") && !capture.evidenceLabel.contains("heading")
                && capture.evidenceLabel.contains("\(expected.0)..<\(expected.1)") && !capture.reader.canRefresh)
            model.closeArchive()
            capture.reader.offsetInput = String(expected.0); capture.reader.goToByte()
            await wait("saved source-line capture did not survive archive closure") { !capture.reader.busy }
            precondition(capture.reader.page?.start == expected.0 && capture.reader.canGoBack
                && capture.reader.page?.originalHex == f.hex(bytes.dropFirst(Int(expected.0))))
            capture.reader.back()
            await wait("saved source-line context did not survive Back") { !capture.reader.busy }
            precondition(capture.reader.page?.start == UInt64(bom) && capture.reader.canPin && !f.calls.contains("LIVE-OPEN"))
            capture.reader.close(); model.close()
        }
        // The relationship page has no encoding authority. A row may only
        // acquire a source anchor after its independently opened capture agrees.
        let invalidRanges: [(UInt64, UInt64)] = [(16, 19), (15, 20), (16, 58)]
        for range in invalidRanges {
            let f = SavedRepositoryFixture(lineEncoding: "utf16le", lineRange: range), model = await prepared(f)
            let page = model.relationships!, link = page.links[0]
            await refused("unaligned or out-of-capture saved line range opened a reader") {
                _ = try await model.openLink(link, from: page, side: .target, readerTransport: f.readerTransport())
            }
            if range.1 > UInt64(f.members[3].bytes!.count) {
                precondition(!f.calls.contains(where: { $0.hasPrefix("reader-create:") || $0.hasPrefix("handoff:") }),
                    "An out-of-member line range must fail before reader admission")
            } else {
                await wait("invalid saved line candidate did not retire") { f.calls.contains("reader-close:100") }
            }
            precondition(model.relationships?.generation == page.generation
                && model.relationships?.links.first?.matches(link) == true && !f.calls.contains("LIVE-OPEN"))
            model.close()
        }
    }
    @MainActor private static func capturedHeadingNavigationAndFrozenReceipts() async throws {
        let f = SavedRepositoryFixture(headingVectors: true), model = await prepared(f)
        let page = model.relationships!, link = page.links[0]
        precondition(link.fragmentResolution == .resolved && link.fragmentSlug == "caf-details")
        precondition(link.suffix == "#caf-%64etails" && link.targetRange?.start == 207 && !link.fragmentUnresolved)
        var changed = f.linkObject(1)
        changed["target_range"] = ["start": "208", "end": "223"]
        let forged = try JSONDecoder().decode(AtlasSavedLink.self, from: Data(f.encode(changed).utf8))
        await refused("a changed heading range rebound the accepted link") {
            _ = try await model.openLink(forged, from: page, side: .target, readerTransport: f.readerTransport())
        }
        precondition(!f.calls.contains(where: { $0.hasPrefix("reader-create:") }))
        let opened = try await model.openLink(link, from: page, side: .target, readerTransport: f.readerTransport())
        precondition(opened.reader.page?.start == 79 && f.calls.contains("reader-window:100:79"))
        precondition(opened.reader.page?.originalHex == f.hex(f.headingBytes.dropFirst(79)))
        precondition(opened.evidenceLabel.contains("#caf-details") && opened.evidenceLabel.contains("207..<223"))
        model.prepareRelationships(); await wait("heading projection did not advance") { !model.busy }
        await refused("an old qualified heading survived generation replacement") {
            _ = try await model.openLink(link, from: page, side: .target, readerTransport: f.readerTransport())
        }
        model.closeArchive()
        opened.reader.offsetInput = "207"; opened.reader.goToByte()
        await wait("captured heading reader lost its independent source") { !opened.reader.busy }
        precondition(opened.reader.page?.originalHex == f.hex(f.headingBytes.dropFirst(207)))
        precondition(!f.calls.contains("LIVE-OPEN"))
        opened.reader.close(); model.close()

        for fault in ["heading-range", "heading-offset", "heading-slug", "heading-resolution", "selection"] {
            let f = SavedRepositoryFixture(headingVectors: true), model = await prepared(f), page = model.relationships!
            f.corruptNext(fault)
            await refused("a changed heading receipt was accepted: \(fault)") {
                _ = try await model.openLink(page.links[0], from: page, side: .target, readerTransport: f.readerTransport())
            }
            await wait("rejected heading reader did not retire") { f.calls.contains("reader-close:100") }
            precondition(f.calls.filter { $0 == "handoff:100" }.count == 1)
            model.close()
        }
    }

    @MainActor private static func failedReplacementAndGenerationRecovery() async {
        let f = SavedRepositoryFixture(), model = await prepared(f)
        let identity = model.info!.identity, old = model.relationships!
        f.corruptNext("build"); model.prepareRelationships()
        await wait("refused build did not reconcile") { !model.busy }
        precondition(model.relationshipGeneration == 1 && model.relationships?.links.map(\.id) == old.links.map(\.id))
        precondition(f.calls.filter { $0 == "prepare:40:2" }.count == 1 && f.calls.contains("info:40"))
        model.prepareRelationships()
        await wait("new generation did not prepare after refused attempt") { !model.busy }
        precondition(model.relationshipGeneration == 3)
        f.corruptNext("open"); model.open(path: "/saved/refused.fcbs", accessLease: nil)
        await wait("refused replacement did not drain") { !model.busy }
        precondition(model.info?.identity == identity && model.relationshipGeneration == 3)
        precondition(f.calls.filter { $0 == "open:41" }.count == 1 && f.calls.filter { $0 == "info:41" }.count == 1)
        await wait("failed replacement handle did not retire") { f.calls.contains("saved-close:41") }
        precondition(!f.calls.contains("saved-close:40"))
        model.close()
    }

    @MainActor private static func savedExportDigestPinsReplacement() async {
        let f = SavedRepositoryFixture(), model = await opened(f)
        let original = model.info!.identity
        let expected = String(repeating: "e", count: 64)
        model.open(path: "/saved/export.fcbs", accessLease: nil, expectedDigest: expected)
        await wait("mismatched exported archive did not drain") { !model.busy }
        precondition(model.info?.identity == original && !f.calls.contains("members:41:0:64"))
        await wait("mismatched candidate did not retire") { f.calls.contains("saved-close:41") }
        f.corruptNext("open-reply")
        model.open(path: "/saved/export.fcbs", accessLease: nil, expectedDigest: expected)
        await wait("reconciled mismatch did not drain") { !model.busy }
        precondition(model.info?.identity == original && f.calls.contains("info:42")
            && !f.calls.contains("members:42:0:64"))
        model.open(path: "/saved/export.fcbs", accessLease: nil, expectedDigest: f.archiveDigest)
        await wait("matching saved digest did not open") { !model.busy }
        precondition(model.info?.identity.owner == 43 && model.info?.identity.digest == f.archiveDigest)
        let before = f.calls.count
        model.open(path: "/saved/export.fcbs", accessLease: nil, expectedDigest: "invalid")
        precondition(f.calls.count == before && !model.busy)
        model.close()
    }

    @MainActor private static func staleRowsAndForeignRegistryCannotTriggerHandoff() async {
        let f = SavedRepositoryFixture(), model = await prepared(f)
        let oldPage = model.relationships!, oldIdentity = model.info!.identity, oldMember = model.members!.items[1]
        model.prepareRelationships(); await wait("replacement generation did not prepare") { !model.busy }
        await refused("old relationship occurrence rebound into new generation") {
            _ = try await model.openLink(oldPage.links[0], from: oldPage, side: .evidence, readerTransport: f.readerTransport())
        }
        let page = model.relationships!
        await refused("a foreign reader registry was admitted") {
            _ = try await model.openLink(page.links[0], from: page, side: .target,
                readerTransport: f.readerTransport(domain: UUID()))
        }
        precondition(!f.calls.contains(where: { $0.hasPrefix("reader-create:") || $0.hasPrefix("handoff:") }))
        model.open(path: "/saved/equal-bytes-other-archive.fcbs", accessLease: nil)
        await wait("second archive did not open") { !model.busy }
        precondition(model.info?.identity.owner == 41 && model.info?.identity.digest == oldIdentity.digest)
        model.selectMember(oldMember, from: oldIdentity)
        precondition(model.selectedMember?.id == 0)
        await refused("old member rebound through equal archive bytes and ordinal") {
            _ = try await model.openMember(oldMember, from: oldIdentity, readerTransport: f.readerTransport())
        }
        precondition(!f.calls.contains(where: { $0.hasPrefix("reader-create:") || $0.hasPrefix("handoff:") }))
        model.close()
    }

    @MainActor private static func invalidMetadataAndReceiptsRetireDestinations() async {
        let f = SavedRepositoryFixture(), model = await prepared(f), page = model.relationships!
        f.corruptNext("metadata")
        await refused("evidence metadata changed before reader admission") {
            _ = try await model.openLink(page.links[0], from: page, side: .evidence, readerTransport: f.readerTransport())
        }
        precondition(!f.calls.contains(where: { $0.hasPrefix("reader-create:") }))
        model.close()
        for fault in ["lost", "owner", "archive", "generation", "selection", "digest", "live",
                      "nested-owner", "nested-path", "inherited-query", "page-bytes"] {
            let f = SavedRepositoryFixture(), model = await prepared(f), page = model.relationships!
            f.corruptNext(fault)
            await refused("invalid saved handoff accepted: \(fault)") {
                _ = try await model.openLink(page.links[0], from: page, side: .evidence, readerTransport: f.readerTransport())
            }
            precondition(model.info?.identity.owner == 40 && model.relationshipGeneration == 1 && !model.busy)
            await wait("initialized but rejected reader was not retired: \(fault)") { f.calls.contains("reader-close:100") }
            precondition(f.calls.filter { $0 == "handoff:100" }.count == 1 && !f.calls.contains("LIVE-OPEN"))
            model.close()
        }
    }

    @MainActor private static func closedArchiveRejectsLateHandoffAndDrainsItsLease() async {
        let f = SavedRepositoryFixture(), model = await prepared(f), page = model.relationships!
        f.blockNextHandoff()
        let pending = Task {
            try await model.openLink(page.links[0], from: page, side: .target, readerTransport: f.readerTransport())
        }
        await wait("saved handoff did not enter worker") { f.entered.wait(timeout: .now()) == .success }
        model.close()
        precondition(!f.calls.contains("saved-close:40"), "archive retired before entered handoff drained")
        f.resume.signal()
        await refused("late transfer escaped closed archive") { _ = try await pending.value }
        await wait("late destination or archive did not retire") {
            f.calls.contains("reader-close:100") && f.calls.contains("saved-close:40")
        }
        precondition(!f.calls.contains("LIVE-OPEN") && model.closed && model.info == nil)
    }
}
#endif
