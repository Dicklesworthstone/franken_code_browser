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
        var readBytes: UInt64 = 0
        var readCalls: UInt64 = 0
        var refused = false
    }
    let domain = UUID()
    let searchVectors: Bool
    init(searchVectors: Bool = false) { self.searchVectors = searchVectors }
    let archiveDigest = String(repeating: "d", count: 64)
    let source = Array("\u{feff}---\ntitle: Saved\n---\n\n[code](src/lib.rs#L2) [again](src/lib.rs) [missing](absent.rs) [empty]()\n".utf8)
    let sourceDigest = "8591776e77fd8d9050b0a5796da773312c306092c3fc05f22dbe9413cfcff6d3"
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
    private var wrongPage: Set<UInt64> = []

    private func locked<T>(_ body: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return body() }
    var calls: [String] { locked { log } }
    func corruptNext(_ kind: String) { locked { fault = kind } }
    func blockNextHandoff() { locked { blocked = true } }
    func blockNextSearch() { locked { blockedSearch = true } }
    var searchUTF8: [UInt8] { Array(("é" + String(repeating: "x", count: 200) + "banana\n").utf8) }
    var searchUTF16: [UInt8] {
        [255, 254, 233, 0] + Array(repeating: [UInt8(120), 0], count: 200).flatMap { $0 }
            + [98, 0, 97, 0, 110, 0, 97, 0, 110, 0, 97, 0, 10, 0]
    }
    var members: [Member] {
        [
            Member(path: Array("README.md".utf8), display: "README.md", bytes: source, digest: sourceDigest),
            Member(path: rawPath, display: "raw-\\xff.rs",
                bytes: searchVectors ? searchUTF8 : Array("changed bytes\n".utf8),
                digest: searchVectors ? "ec34a752af5a6ce18bfd388bda04c6c9f60b22301b44d413c085507ca994f6cc"
                    : "7c60358cb724c1b671469c5f167cea45a4f70df4e3ac87b79340692e6f17f574"),
            Member(path: Array("skipped.md".utf8), display: "skipped.md",
                bytes: searchVectors ? searchUTF16 : [255],
                digest: searchVectors ? "0896dfbe40292ec212fa69dac19c681c8f56ba79c407cbad6817580762413ecf"
                    : "a8100ae6aa1940d0b663bb31cd466142ebbdbd5187131b92d93818987832eb89",
                encoding: searchVectors ? "utf16le" : "utf8"),
            Member(path: Array("src/lib.rs".utf8), display: "src/lib.rs",
                bytes: searchVectors ? [254, 255, 0, 97, 254, 255, 0, 98] : Array("abc".utf8),
                digest: searchVectors ? "c71ff6c5ed08a430f34d4fcd60b2c151d57934c79a6087aa25496d9b06c65cc7"
                    : "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
                encoding: searchVectors ? "utf16be" : "utf8"),
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
        let destination = [1: "src/lib.rs#L2", 2: "src/lib.rs", 3: "absent.rs", 4: ""][Int(id)]!
        let captured = id <= 2
        return ["link_id": String(id), "source_member": "0", "source_file_id": "1",
            "source_revision": "1", "source_digest": sourceDigest,
            "source_path": path(Array("README.md".utf8), "README.md"),
            "evidence_range": ["start": "0", "end": String(source.count)],
            "block_index": "0", "source_link_id": String(id), "destination": destination,
            "resolution": captured ? "captured" : (id == 3 ? "missing" : "unresolved"),
            "target_member": captured ? "3" as Any : NSNull(),
            "target_path": captured ? path(Array("src/lib.rs".utf8), "src/lib.rs") as Any : NSNull(),
            "target_suffix": id == 1 ? "#L2" : "", "fragment_resolution": id == 1 ? "unresolved" : "none"]
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
            "relationships_complete": false, "truncated": false, "markdown_members": "3",
            "scanned_markdown_members": "1", "unavailable_markdown_members": "1",
            "unsupported_markdown_members": "1", "source_limited_members": "0", "diagnostic_markdown_members": "0",
            "retained_links": "4", "links_seen": "4", "matching_links": String(ids.count),
            "member_bytes_read": String(source.count + members[2].bytes!.count), "member_read_calls": "2",
            "retained_source_payload_bytes": "0", "direction": direction.rawValue,
            "filter_member": member.map(String.init) as Any? ?? NSNull(),
            "links": ids[Int(start)..<Int(end)].map(linkObject),
            "next_offset": end < UInt64(ids.count) ? String(end) as Any : NSNull()
        ]) { _, new in new }
        return out
    }
    func info(_ owner: UInt64, path archivePath: String = "/saved/repository.fcbs",
        generation: UInt64? = nil, query: UInt64? = nil, bytes: UInt64 = 0, calls: UInt64 = 0) -> [String: Any] {
        var out = header(owner, "info")
        out["archive_path"] = path(Array(archivePath.utf8), archivePath)
        out["accepted_relationship_generation"] = generation.map(String.init) as Any? ?? NSNull()
        out["accepted_query_generation"] = query.map(String.init) as Any? ?? NSNull()
        out["accepted_index_generation"] = NSNull()
        out["initial_archive_bytes_read"] = "2048"; out["directory_reserved_bytes"] = "4096"
        out["member_bytes_read"] = String(bytes); out["member_read_calls"] = String(calls)
        return out
    }
    func searchHits(_ needle: String) -> [QueryHit] {
        // Independent fixed vectors; no substitute matcher or offset inference.
        guard searchVectors else { return [] }
        if needle.utf8.elementsEqual("\u{feff}".utf8) { return [QueryHit(member: 3, start: 4, end: 6)] }
        guard needle.utf8.elementsEqual("ana".utf8) else { return [] }
        return [QueryHit(member: 1, start: 203, end: 206), QueryHit(member: 1, start: 205, end: 208),
            QueryHit(member: 2, start: 406, end: 412), QueryHit(member: 2, start: 410, end: 416)]
    }
    func hitObject(_ id: UInt64, needle: String = "ana") -> [String: Any] {
        let hit = searchHits(needle)[Int(id - 1)], member = members[Int(hit.member)]
        return ["hit_id": String(id), "member": String(hit.member), "file_id": String(hit.member + 1),
            "source_revision": String(hit.member + 1), "source_digest": member.digest!,
            "path": path(member.path, member.display),
            "original_range": ["start": String(hit.start), "end": String(hit.end)]]
    }
    func searchPage(_ owner: UInt64, generation: UInt64, needle: String = "ana",
        start: UInt64 = 0, limit: UInt64 = 64, searching: Bool = false) -> [String: Any] {
        var out = header(owner, searching ? "search" : "results")
        let hits = searchHits(needle), end = min(UInt64(hits.count), start + limit)
        let sourceBytes = members.compactMap(\.bytes).reduce(0) { $0 + $1.count }
        out.merge(["query_generation": String(generation), "needle": needle,
            "index_generation": NSNull(), "index_pin": NSNull(), "mode": "exact-decoded-literal",
            "search_complete": false, "truncated": false, "retained_hits": String(hits.count),
            "matches_seen": String(hits.count), "unavailable_files": "1", "unsupported_files": "0",
            "member_bytes_read": String(sourceBytes), "member_read_calls": "4",
            "scanned_bytes": String(sourceBytes), "peak_source_bytes": String(members.compactMap(\.bytes).map(\.count).max() ?? 0),
            "index_eliminated_files": "0", "fallback_files": "0", "posting_entries_visited": "0",
            "retained_source_payload_bytes": "0",
            "hits": (start..<end).map { hitObject($0 + 1, needle: needle) },
            "next_offset": end < UInt64(hits.count) ? String(end) as Any : NSNull()
        ]) { _, new in new }
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
        query: UInt64? = nil, hitID: UInt64? = nil) -> String? {
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
        if let query, let hitID, let needle = sessions[saved]?.needle {
            let hit = searchHits(needle)[Int(hitID - 1)]
            out["selection"] = ["query_generation": String(query), "hit_id": String(hitID),
                "domain": "original-source-bytes", "original_range": ["start": String(hit.start), "end": String(hit.end)],
                "original_hex": hex(bytes[Int(hit.start)..<Int(hit.end)])]
            windowStart = hit.start - min(hit.start, 132)
            windowBytes = max(4, min(UInt64(bytes.count), hit.end + 132) - windowStart)
        }
        if let generation, let link, let side {
            out["relationship_generation"] = String(generation); out["link_id"] = String(link); out["side"] = side.rawValue
            out["offset"] = "0"; out["evidence_precision"] = "whole-captured-markdown-document"
            out["fragment_resolution"] = link == 1 ? "unresolved" : "none"
        }
        if let fault {
            self.fault = nil
            switch fault {
            case "lost": return nil // Destination accepted before a missing receipt.
            case "owner": out["owner"] = String(saved + 1)
            case "archive": out["archive_digest"] = String(repeating: "e", count: 64)
            case "generation": out["relationship_generation"] = String((generation ?? 0) + 1)
            case "selection": out["selection"] = ["start": "1", "end": String(bytes.count)]
            case "digest": out["source_digest"] = String(repeating: "e", count: 64)
            case "live": out["live_source_reopened"] = true
            case "nested-owner": nested["owner"] = "999"
            case "nested-path": nested["path"] = path(Array("another.rs".utf8), member.display)
            case "inherited-query": nested["accepted_query_generation"] = "1"
            case "many-read-calls": out["member_read_calls"] = "1024" // Interrupted reads can exceed source length.
            case "page-bytes": wrongPage.insert(reader)
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
            case .openMember, .openRelationship, .openHit:
                let pause = locked { let value = blocked; blocked = false; return value }
                if pause { entered.signal(); precondition(resume.wait(timeout: .now() + .seconds(5)) == .success) }
            case .search:
                let pause = locked { let value = blockedSearch; blockedSearch = false; return value }
                if pause { entered.signal(); precondition(resume.wait(timeout: .now() + .seconds(5)) == .success) }
            default: break
            }
            return locked {
                switch request {
                case .open(let path):
                    log.append("open:\(handle)")
                    let refused = fault == "open"
                    if refused { fault = nil }
                    sessions[handle] = Session(path: path, refused: refused)
                    return refused ? nil : encode(info(handle, path: path))
                case .info:
                    log.append("info:\(handle)")
                    guard let session = sessions[handle], !session.refused else { return nil }
                    return encode(info(handle, path: session.path, generation: session.generation, query: session.queryGeneration,
                        bytes: session.readBytes, calls: session.readCalls))
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
                    session.generation = generation; session.readBytes += UInt64(source.count + members[2].bytes!.count); session.readCalls += 2
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
                case .search(let generation, let needle, let matches):
                    precondition(matches == 4096)
                    log.append("search:\(handle):\(generation):\(needle)")
                    guard var session = sessions[handle] else { return nil }
                    if fault == "search" { fault = nil; return nil }
                    session.queryGeneration = generation; session.needle = needle
                    session.readBytes += UInt64(members.compactMap(\.bytes).reduce(0) { $0 + $1.count }); session.readCalls += 4
                    sessions[handle] = session
                    if fault == "search-lost" { fault = nil; return nil }
                    return encode(searchPage(handle, generation: generation, needle: needle, searching: true))
                case .results(let generation, let start, let limit):
                    log.append("results:\(handle):\(generation):\(start)")
                    guard let session = sessions[handle], session.queryGeneration == generation, let needle = session.needle else { return nil }
                    if fault == "results" { fault = nil; return nil }
                    return encode(searchPage(handle, generation: generation, needle: needle, start: start, limit: limit))
                case .clearResults(let generation):
                    log.append("clear-search:\(handle):\(generation)")
                    sessions[handle]?.queryGeneration = nil; sessions[handle]?.needle = nil
                    if fault == "clear-lost" { fault = nil; return nil }
                    var out = header(handle, "clear-results")
                    out["accepted_query_generation"] = NSNull(); out["retained_hits"] = "0"
                    return encode(out)
                case .openHit(let reader, let generation, let id):
                    guard let session = sessions[handle], session.queryGeneration == generation,
                          let needle = session.needle, id > 0, id <= UInt64(searchHits(needle).count) else { return nil }
                    return handoff(handle, reader: reader, ordinal: searchHits(needle)[Int(id - 1)].member,
                        generation: nil, link: nil, side: nil, query: generation, hitID: id)
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
        await failedReplacementAndGenerationRecovery()
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
        precondition(target.evidenceLabel.contains("unresolved") && !target.reader.canRefresh)
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
