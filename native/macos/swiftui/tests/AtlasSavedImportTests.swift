import Foundation
import Dispatch

/// The foreign replies are fixed protocol vectors, not a second extractor.
/// The shared fixture supplies independently retained original-byte readers.
private final class SavedImportFixture: @unchecked Sendable {
    let base = SavedRepositoryFixture(searchVectors: true)
    let sourceOrdinal: UInt64
    init(sourceOrdinal: UInt64 = 1) { self.sourceOrdinal = sourceOrdinal }
    var evidenceStart: UInt64 { sourceOrdinal == 1 ? 202 : (sourceOrdinal == 2 ? 404 : 2) }
    var evidenceEnd: UInt64 { sourceOrdinal == 1 ? 208 : (sourceOrdinal == 2 ? 416 : 8) }
    var source: SavedRepositoryFixture.Member { base.members[Int(sourceOrdinal)] }
    let entered = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var generation: UInt64?
    private var attempt: UInt64 = 0
    private var fault: String?
    private var block = false
    private var operations: [String] = []
    private func locked<T>(_ body: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return body() }
    var calls: [String] { locked { operations } }
    func corrupt(_ value: String) { locked { fault = value } }
    func blockNextBuild() { locked { block = true } }
    func object(_ json: String?) -> [String: Any] {
        try! JSONSerialization.jsonObject(with: Data(json!.utf8)) as! [String: Any]
    }
    var contract: [String: Any] {
        ["projection": "lexical-source-imports", "relationship_kind": "lexical-import",
         "evidence_precision": "exact-import-source-range", "resolution_domain": "declared-catalog-candidates",
         "compiler_resolved": false, "semantic_complete": false,
         "extractor": "franken-markdown-d6fbd1d5/import-statements-v3",
         "extraction_policy": "rust-use-tree-js-static-and-python-import-v3", "resolver": "fcb-closed-catalog-imports-v3"]
    }
    func link(_ id: UInt64) -> [String: Any] {
        let target = base.members[id == 1 ? 3 : 4]
        var row: [String: Any] = ["link_id": String(id), "source_member": String(sourceOrdinal), "source_file_id": String(sourceOrdinal + 1),
            "source_revision": String(sourceOrdinal + 1), "source_digest": source.digest!, "source_path": base.path(source.path, source.display),
            "source_bytes": String(source.bytes!.count), "source_link_id": String(id),
            "evidence_range": ["start": String(evidenceStart), "end": String(evidenceEnd)],
            "specifier_range": ["start": String(evidenceStart + 2), "end": String(evidenceEnd - 2)],
            "language": "rust", "lexical_import_kind": "rust-module", "destination": "saved",
            "resolution": id == 1 ? "captured" : "unavailable", "resolution_basis": "rust-module-file-candidate",
            "target_member": id == 1 ? "3" : "4", "target_file_id": id == 1 ? "4" : "5",
            "target_path": base.path(target.path, target.display), "target_revision": NSNull(),
            "target_digest": NSNull(), "target_bytes": NSNull()]
        if id == 1 {
            row["target_revision"] = "4"; row["target_digest"] = target.digest!; row["target_bytes"] = String(target.bytes!.count)
        }
        return row
    }
    func page(_ owner: UInt64, generation: UInt64, direction: AtlasSavedDirection = .all, member: UInt64? = nil,
        start: UInt64 = 0, limit: UInt64 = 64, preparing: Bool = false) -> [String: Any] {
        var out = base.header(owner, preparing ? "imports-prepare" : "imports")
        out.merge(contract) { _, next in next }
        let matches = [UInt64(1), 2].filter { direction == .all ||
            (direction == .outgoing ? member == sourceOrdinal : member == ($0 == 1 ? 3 : 4)) }
        let rows = Array(matches.dropFirst(Int(start)).prefix(Int(limit))).map(link)
        out.merge(["import_generation": String(generation), "completeness_scope": "parsed-source-imports-in-saved-manifest",
            "source_observation": "verified-saved-members", "imports_complete": true, "truncated": false,
            "source_members": "1", "scanned_source_members": "1", "unavailable_source_members": "0",
            "unsupported_source_members": "0", "source_limited_members": "0", "diagnostic_source_members": "0",
            "parser_diagnostics": "0", "retained_links": "2", "links_seen": "2",
            "member_bytes_read": String(source.bytes!.count), "member_read_calls": "1",
            "peak_source_bytes": String(source.bytes!.count), "retained_source_payload_bytes": "0",
            "max_links": "4096", "max_source_bytes": "33554432", "max_member_source_bytes": "131072",
            "direction": direction.rawValue, "filter_member": member.map { String($0) as Any } ?? NSNull(),
            "offset": String(start), "limit": String(limit), "matching_links": String(matches.count), "links": rows,
            "next_offset": start + UInt64(rows.count) < UInt64(matches.count) ? String(start + UInt64(rows.count)) as Any : NSNull()
        ]) { _, next in next }
        return out
    }
    var transport: AtlasSavedTransport {
        let saved = base.savedTransport
        return AtlasSavedTransport(create: saved.create, run: { [self] owner, request in
            if case .prepareImports = request {
                let shouldBlock = locked { let value = block; block = false; return value }
                if shouldBlock { entered.signal(); resume.wait() }
            }
            switch request {
            case .open, .info:
                guard let reply = saved.run(owner, request) else { return nil }
                return locked {
                    if case .info = request, fault == "info-lost" { fault = nil; return nil }
                    var out = object(reply)
                    out["accepted_import_generation"] = generation.map { String($0) as Any } ?? NSNull()
                    out["last_import_attempt"] = String(attempt)
                    return base.encode(out)
                }
            case .prepareImports(let next, let links, let bytes):
                return locked {
                    precondition(links == 4096 && bytes == 33_554_432)
                    operations.append("prepare:\(next)")
                    guard next > attempt else { return nil }; attempt = next
                    if fault == "refused" { fault = nil; return nil }
                    generation = next
                    if fault == "lost" { fault = nil; return nil }
                    if fault == "unknown" { fault = "info-lost"; return nil }
                    return base.encode(page(owner, generation: next, preparing: true))
                }
            case .imports(let next, let direction, let member, let start, let limit):
                return locked {
                    guard generation == next else { return nil }
                    operations.append("page:\(next):\(direction.rawValue):\(start)")
                    return base.encode(page(owner, generation: next, direction: direction, member: member, start: start, limit: limit))
                }
            case .clearImports(let next):
                return locked {
                    guard next > attempt else { return nil }; attempt = next; generation = nil
                    operations.append("clear:\(next)")
                    if fault == "clear-lost" { fault = nil; return nil }
                    var out = base.header(owner, "imports-clear")
                    out["accepted_import_generation"] = NSNull(); out["last_import_attempt"] = String(next); out["retained_links"] = "0"
                    return base.encode(out)
                }
            case .openImport(let reader, let next, let id, let side):
                guard locked({ generation == next && (1...2).contains(id) && (side == .evidence || id == 1) }) else { return nil }
                let ordinal: UInt64 = side == .evidence ? sourceOrdinal : 3
                guard let reply = saved.run(owner, .openMember(reader: reader, member: ordinal)) else { return nil }
                var out = object(reply)
                out.merge(contract) { _, next in next }
                out["command"] = "imports-open-reader"; out["import_generation"] = String(next)
                out["link_id"] = String(id); out["side"] = side.rawValue; out["relationship"] = link(id)
                let member = base.members[Int(ordinal)]
                out["path"] = base.path(member.path, member.display); out["source_bytes"] = String(member.bytes!.count)
                out["offset"] = side == .evidence ? String(evidenceStart) : "0"
                out["selection"] = side == .evidence ? ["start": String(evidenceStart), "end": String(evidenceEnd)] as Any : NSNull()
                let offset = side == .evidence ? evidenceStart - min(evidenceStart, 128) : 0
                out["window"] = object(base.readerTransport().window(reader, offset, 16 * 1024))
                return locked {
                    operations.append("open:\(next):\(id):\(side.rawValue)")
                    if let corrupt = fault {
                        fault = nil
                        switch corrupt {
                        case "reader-generation": out["import_generation"] = String(next + 1)
                        case "reader-selection": out["selection"] = ["start": String(evidenceStart + 1), "end": String(evidenceEnd)]
                        case "reader-row": var row = link(id); row["destination"] = "forged"; out["relationship"] = row
                        case "reader-owner": out["reader_owner"] = String(reader + 1)
                        case "reader-live": out["live_source_reopened"] = true
                        case "reader-many-calls": out["member_read_calls"] = "1048576"
                        case "reader-lost": return nil
                        default: preconditionFailure("unknown import fixture fault")
                        }
                    }
                    return base.encode(out)
                }
            default: return saved.run(owner, request)
            }
        }, cancel: saved.cancel, close: saved.close, retirementFailed: saved.retirementFailed, readerDomain: base.domain)
    }
}

@main private enum AtlasSavedImportTests {
    @MainActor private static func wait(_ message: String, _ ready: () -> Bool) async {
        for _ in 0..<2000 { if ready() { return }; try? await Task.sleep(nanoseconds: 1_000_000) }
        preconditionFailure(message)
    }
    private static func reject(_ work: () throws -> Void) {
        do { try work(); preconditionFailure("forged import receipt accepted") } catch {}
    }
    @MainActor private static func rustUseWire(_ fixture: SavedImportFixture, identity: AtlasSavedIdentity,
        source: AtlasSavedReaderCapture, target: AtlasSavedReaderCapture) throws {
        func decode(_ value: [String: Any]) throws -> AtlasSavedImports {
            try AtlasSavedImports.decode(fixture.base.encode(value), identity: identity, generation: 1,
                direction: .all, member: nil, start: 0, limit: 64)
        }
        let original = fixture.page(identity.owner, generation: 1)
        let targetFields = ["target_member", "target_file_id", "target_path", "target_revision", "target_digest", "target_bytes"]
        let localBases = ["rust-use-module-file-candidate", "rust-use-module-prefix-candidate"]
        var previous: AtlasSavedImport?
        for basis in localBases {
            var value = original, rows = original["links"] as! [[String: Any]]
            for i in rows.indices {
                rows[i]["lexical_import_kind"] = "rust-use"
                rows[i]["destination"] = basis == localBases[0] ? "crate::saved" : "crate::saved::Item"
                rows[i]["resolution_basis"] = basis
            }
            value["links"] = rows
            let page = try decode(value), link = page.links[0], unavailable = page.links[1]
            precondition(link.canOpenTarget && link.targetMember == 3 && link.kind == "rust-use")
            precondition(!unavailable.canOpenTarget && unavailable.targetMember == 4 && unavailable.targetDigest == nil)
            precondition(link.resolutionLabel.contains("containing-module candidate") == (basis == localBases[1]))
            precondition(unavailable.resolutionLabel.lowercased().contains("containing-module candidate") == (basis == localBases[1]))
            let targetCapture = AtlasSavedReaderCapture(reader: target.reader, owner: target.owner, member: target.member,
                origin: .importLink(generation: 1, link: link, side: .target), archiveIdentity: identity)
            precondition(targetCapture.evidenceLabel.contains("containing-module candidate") == (basis == localBases[1]))
            if basis == localBases[1] {
                precondition(targetCapture.evidenceLabel.contains("imported item location and compiler resolution are not established"))
            }
            let evidenceCapture = AtlasSavedReaderCapture(reader: source.reader, owner: source.owner, member: source.member,
                origin: .importLink(generation: 1, link: link, side: .evidence), archiveIdentity: identity)
            precondition(evidenceCapture.evidenceLabel.contains("exact statement bytes \(link.evidence.start)..<\(link.evidence.end)")
                && evidenceCapture.evidenceLabel.contains("specifier bytes \(link.specifier.start)..<\(link.specifier.end)"))
            if let previous { precondition(!link.matches(previous), "Module prefix evidence cannot replace a module file receipt") }
            previous = link
            for (language, kind) in [("rust", "rust-module"), ("python", "python-from"),
                ("javascript", "javascript-import"), ("typescript", "javascript-export"),
                ("jsx", "javascript-import"), ("tsx", "javascript-export")] {
                var bad = value, changed = rows
                changed[0]["language"] = language; changed[0]["lexical_import_kind"] = kind
                bad["links"] = changed; reject { _ = try decode(bad) }
            }
            for resolution in ["missing", "ambiguous", "external", "unresolved"] {
                var bad = value, changed = rows
                for i in changed.indices {
                    changed[i]["resolution"] = resolution
                    for field in targetFields { changed[i][field] = NSNull() }
                }
                bad["links"] = changed; reject { _ = try decode(bad) }
            }
            var old = value; old["resolver"] = "fcb-closed-catalog-imports-v2"
            reject { _ = try decode(old) }
        }
        var ambiguous = original, rows = original["links"] as! [[String: Any]]
        for i in rows.indices {
            rows[i]["lexical_import_kind"] = "rust-use"; rows[i]["destination"] = "crate::saved"
            rows[i]["resolution"] = "ambiguous"; rows[i]["resolution_basis"] = "ambiguous-rust-crate-root"
            for field in targetFields { rows[i][field] = NSNull() }
        }
        ambiguous["links"] = rows
        let refusedRoot = try decode(ambiguous).links[0]
        precondition(!refusedRoot.canOpenTarget && refusedRoot.targetMember == nil && refusedRoot.targetPath == nil
            && refusedRoot.resolutionLabel.contains("crate roots")
            && refusedRoot.resolutionLabel.contains("no target was selected"))
        for (language, kind) in [("rust", "rust-module"), ("python", "python-from"), ("javascript", "javascript-import")] {
            var bad = ambiguous, changed = rows
            changed[0]["language"] = language; changed[0]["lexical_import_kind"] = kind
            bad["links"] = changed; reject { _ = try decode(bad) }
        }
        for resolution in ["missing", "external", "unresolved"] {
            var bad = ambiguous, changed = rows
            changed[0]["resolution"] = resolution; bad["links"] = changed
            reject { _ = try decode(bad) }
        }
        var forgedTarget = original, targetRows = original["links"] as! [[String: Any]]
        targetRows[0]["lexical_import_kind"] = "rust-use"
        targetRows[0]["resolution_basis"] = "ambiguous-rust-crate-root"
        forgedTarget["links"] = targetRows; reject { _ = try decode(forgedTarget) }
    }
    @MainActor static func main() async throws {
        let f = SavedImportFixture()
        let browser = AtlasSavedRepositoryModel(transport: f.transport)
        browser.open(path: "/saved/repository.fcbs", accessLease: nil)
        await wait("archive open") { !browser.busy }
        let identity = browser.info!.identity
        let decode: ([String: Any]) throws -> AtlasSavedImports = {
            try AtlasSavedImports.decode(f.base.encode($0), identity: identity, generation: 1,
                direction: .all, member: nil, start: 0, limit: 64)
        }
        let accepted = try decode(f.page(identity.owner, generation: 1))
        precondition(accepted.complete && accepted.links[0].sourcePath.bytes == f.base.rawPath)
        precondition(accepted.links[0].canOpenTarget && !accepted.links[1].canOpenTarget)
        var interrupted = f.page(identity.owner, generation: 1); interrupted["member_read_calls"] = "1048576"
        let interruptedPage = try decode(interrupted)
        precondition(interruptedPage.readCalls > interruptedPage.sourceBytes, "interrupted snapshot attempts can exceed byte count")
        for (field, value) in [("owner", "99" as Any), ("compiler_resolved", true), ("source_members", "0"),
            ("member_bytes_read", "33554433"), ("retained_source_payload_bytes", "1"), ("next_offset", "1"),
            ("limit", "63"), ("import_generation", "2"), ("peak_source_bytes", "131073")] {
            var invalid = f.page(identity.owner, generation: 1); invalid[field] = value
            reject { _ = try decode(invalid) }
        }
        for (field, value) in [("source_revision", "3" as Any), ("source_digest", "wrong"),
            ("target_revision", NSNull()), ("language", "unknown"), ("link_id", 1), ("destination", ""),
            ("resolution_basis", "unsupported-specifier"),
            ("specifier_range", ["start": "201", "end": "207"]), ("evidence_range", ["start": "202", "end": "999"])] {
            var invalid = f.page(identity.owner, generation: 1), row = f.link(1)
            row[field] = value; invalid["links"] = [row, f.link(2)]
            reject { _ = try decode(invalid) }
        }
        var short = f.page(identity.owner, generation: 1)
        short["links"] = [f.link(1)]; short["next_offset"] = "1"
        let shortPage = try decode(short)
        precondition(shortPage.next == 1, "bounded JSON pages may contain fewer than limit rows")
        var unresolved = f.link(1)
        unresolved["language"] = "python"; unresolved["lexical_import_kind"] = "python-from"
        unresolved["resolution"] = "unresolved"; unresolved["resolution_basis"] = "unsupported-specifier"
        unresolved["destination"] = "invalid\0specifier"
        for field in ["target_member", "target_file_id", "target_revision", "target_digest", "target_path", "target_bytes"] {
            unresolved[field] = NSNull()
        }
        var lexical = f.page(identity.owner, generation: 1); lexical["links"] = [unresolved, f.link(2)]
        let lexicalPage = try decode(lexical)
        precondition(lexicalPage.links[0].destination.utf8.contains(0), "lexical spelling is not path authority")

        browser.prepareImports(); await wait("imports build") { !browser.busy }
        let first = browser.imports!, link = first.links[0]
        let source = try await browser.openImport(link, from: first, side: .evidence, readerTransport: f.base.readerTransport())
        precondition(source.member.id == 1 && source.reader.page?.start == 74 && source.reader.page?.identity.pathBytes == f.base.rawPath)
        let target = try await browser.openImport(link, from: first, side: .target, readerTransport: f.base.readerTransport())
        precondition(target.member.id == 3 && target.reader.page?.start == 0 && target.reader.page?.identity.encoding == "utf16be")
        try rustUseWire(f, identity: identity, source: source, target: target)
        f.corrupt("reader-many-calls")
        let retried = try await browser.openImport(link, from: first, side: .evidence, readerTransport: f.base.readerTransport())
        precondition(retried.reader.canPin); retried.reader.close()
        for fault in ["reader-generation", "reader-selection", "reader-row", "reader-owner", "reader-live", "reader-lost"] {
            f.corrupt(fault)
            do {
                _ = try await browser.openImport(link, from: first, side: .evidence, readerTransport: f.base.readerTransport())
                preconditionFailure("forged reader accepted: \(fault)")
            } catch {}
            precondition(source.reader.canPin && browser.importGeneration == first.generation)
        }
        browser.selectMember(browser.members!.items[3], from: identity, forImports: true)
        browser.loadImports(direction: .incoming); await wait("incoming page") { !browser.busy }
        precondition(browser.imports?.links.count == 1 && browser.imports?.links.first?.id == 1)
        let builds = f.calls.filter { $0.hasPrefix("prepare:") }.count
        browser.selectMember(browser.members!.items[4], from: identity, forImports: true)
        await wait("incoming unavailable target") { !browser.busy }
        precondition(browser.imports?.links.first?.id == 2 && browser.imports?.links.first?.canOpenTarget == false)
        precondition(f.calls.filter { $0.hasPrefix("prepare:") }.count == builds, "filtering must not rebuild")
        browser.searchInput = "ana"; browser.search(); await wait("independent text search") { !browser.busy }
        let query = browser.queryGeneration
        f.corrupt("lost"); browser.prepareImports(); await wait("lost build reconciliation") { !browser.busy }
        precondition(browser.importGeneration == 2 && browser.queryGeneration == query)
        f.corrupt("refused"); browser.prepareImports(); await wait("failed build reconciliation") { !browser.busy }
        precondition(browser.importGeneration == 2 && browser.queryGeneration == query)
        do {
            _ = try await browser.openImport(link, from: first, side: .evidence, readerTransport: f.base.readerTransport())
            preconditionFailure("old import generation navigated")
        } catch {}
        f.blockNextBuild(); browser.prepareImports()
        await wait("blocked build did not enter") { f.entered.wait(timeout: .now()) == .success }
        browser.cancel(); browser.prepareImports(); precondition(browser.busy)
        f.resume.signal(); await wait("canceled worker drain") { !browser.busy }
        precondition(browser.importGeneration == 4 && source.reader.canPin && target.reader.canPin)
        f.corrupt("unknown"); browser.prepareImports(); await wait("unknown import state") { !browser.busy }
        precondition(browser.imports == nil && browser.queryGeneration == query && source.reader.canPin)
        browser.reloadImports(); await wait("reload accepted inventory") { !browser.busy }
        precondition(browser.importGeneration == 5)
        f.corrupt("clear-lost"); browser.clearImports(); await wait("lost clear reconciliation") { !browser.busy }
        precondition(browser.importGeneration == nil && browser.imports == nil && browser.queryGeneration == query)
        browser.closeArchive()
        precondition(source.reader.canPin && target.reader.canPin && !f.base.calls.contains("LIVE-OPEN"))
        source.reader.close(); target.reader.close(); browser.close()
        let wideOrdinals: [UInt64] = [2, 3]
        for ordinal in wideOrdinals {
            let wide = SavedImportFixture(sourceOrdinal: ordinal)
            let wideBrowser = AtlasSavedRepositoryModel(transport: wide.transport)
            wideBrowser.open(path: "/saved/repository.fcbs", accessLease: nil)
            await wait("UTF16 archive") { !wideBrowser.busy }
            wideBrowser.prepareImports(); await wait("UTF16 imports") { !wideBrowser.busy }
            let page = wideBrowser.imports!
            let capture = try await wideBrowser.openImport(page.links[0], from: page, side: .evidence,
                readerTransport: wide.base.readerTransport())
            precondition(capture.member.id == ordinal && capture.reader.page?.identity.encoding == (ordinal == 2 ? "utf16le" : "utf16be"))
            precondition(capture.reader.page?.start == wide.evidenceStart - min(wide.evidenceStart, 128))
            wide.corrupt("reader-selection")
            do {
                _ = try await wideBrowser.openImport(page.links[0], from: page, side: .evidence, readerTransport: wide.base.readerTransport())
                preconditionFailure("misaligned UTF16 evidence selection accepted")
            } catch {}
            wideBrowser.closeArchive(); precondition(capture.reader.canPin)
            capture.reader.close(); wideBrowser.close()
        }
        print("AtlasSavedImportTests: strict lexical receipts, bounded pages, imported-by filters, independent generations, cancellation/recovery and saved source ownership passed")
    }
}
