import Foundation
import Dispatch

/// The foreign replies are fixed protocol vectors, not a second extractor.
/// The shared fixture supplies independently retained original-byte readers.
private final class SavedImportFixture: @unchecked Sendable {
    let base: SavedRepositoryFixture
    let sourceOrdinal: UInt64
    let largeEncoding: String?
    init(sourceOrdinal: UInt64 = 1, largeEncoding: String? = nil) {
        precondition(largeEncoding == nil || sourceOrdinal == 1)
        self.sourceOrdinal = sourceOrdinal; self.largeEncoding = largeEncoding
        let replacement = largeEncoding.map { encoding in
            SavedRepositoryFixture.Member(path: Array("src/large.rs".utf8), display: "src/large.rs",
                bytes: Self.largeSource(encoding), digest: String(repeating: "c", count: 64), encoding: encoding)
        }
        base = SavedRepositoryFixture(searchVectors: true, sourceMemberOverride: replacement)
    }
    /// Exact original source vectors; the native fixture does not extract or
    /// hash declarations. Real extraction is covered by host and C regressions.
    private static func largeSource(_ encoding: String) -> [UInt8] {
        precondition(["utf8", "utf16le", "utf16be"].contains(encoding))
        let unit = encoding == "utf8" ? 1 : 2
        func bytes(_ text: String) -> [UInt8] {
            if unit == 1 { return Array(text.utf8) }
            return text.utf16.flatMap { value in
                encoding == "utf16le" ? [UInt8(value & 255), UInt8(value >> 8)]
                    : [UInt8(value >> 8), UInt8(value & 255)]
            }
        }
        let bom: [UInt8] = encoding == "utf8" ? [239, 187, 191]
            : encoding == "utf16le" ? [255, 254] : [254, 255]
        let suffix = bytes("// 😀\r\n" + String(repeating: "mod saved;\n", count: 70))
        let padding = (1_048_576 - bom.count - suffix.count) / unit
        let raw = bom + bytes(String(repeating: " ", count: padding)) + suffix
        precondition(raw.count == 1_048_576)
        return raw
    }
    var evidenceStart: UInt64 {
        if let largeEncoding { return 1_048_576 - 70 * 11 * (largeEncoding == "utf8" ? 1 : 2) }
        return sourceOrdinal == 1 ? 202 : (sourceOrdinal == 2 ? 404 : 2)
    }
    var evidenceEnd: UInt64 {
        if let largeEncoding { return evidenceStart + 10 * (largeEncoding == "utf8" ? 1 : 2) }
        return sourceOrdinal == 1 ? 208 : (sourceOrdinal == 2 ? 416 : 8)
    }
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
         "extractor": "franken-markdown-d6fbd1d5/import-statements-v10",
         "extraction_policy": "rust-inline-modules-file-includes-use-tree-js-literal-loads-python-cpp-go-java-v10", "resolver": "fcb-closed-catalog-imports-v13"]
    }
    func link(_ id: UInt64) -> [String: Any] {
        let target = base.members[id == 1 ? 3 : 4]
        let unit: UInt64 = largeEncoding == nil || largeEncoding == "utf8" ? 1 : 2
        let specifierStart = largeEncoding == nil ? evidenceStart + 2 : evidenceStart + 4 * unit
        let specifierEnd = largeEncoding == nil ? evidenceEnd - 2 : evidenceEnd - unit
        var row: [String: Any] = ["link_id": String(id), "source_member": String(sourceOrdinal), "source_file_id": String(sourceOrdinal + 1),
            "source_revision": String(sourceOrdinal + 1), "source_digest": source.digest!, "source_path": base.path(source.path, source.display),
            "source_bytes": String(source.bytes!.count), "source_link_id": String(id),
            "evidence_range": ["start": String(evidenceStart), "end": String(evidenceEnd)],
            "specifier_range": ["start": String(specifierStart), "end": String(specifierEnd)],
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
            "max_links": "4096", "max_source_bytes": "33554432", "max_member_source_bytes": String(AtlasSavedImportWire.maximumMemberBytes),
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
        var inline = original, inlineRow = fixture.link(1)
        inlineRow["lexical_import_kind"] = "rust-use"; inlineRow["destination"] = "self::*"
        inlineRow["resolution_basis"] = "rust-use-inline-module-candidate"
        for (target, source) in [("target_member", "source_member"), ("target_file_id", "source_file_id"),
            ("target_path", "source_path"), ("target_revision", "source_revision"),
            ("target_digest", "source_digest"), ("target_bytes", "source_bytes")] {
            inlineRow[target] = inlineRow[source]
        }
        inline["links"] = [inlineRow, fixture.link(2)]
        let inlineLink = try decode(inline).links[0]
        precondition(inlineLink.canOpenTarget && inlineLink.targetMember == inlineLink.sourceMember
            && inlineLink.resolutionLabel.contains("inline module"))
        let inlineCapture = AtlasSavedReaderCapture(reader: source.reader, owner: source.owner, member: source.member,
            origin: .importLink(generation: 1, link: inlineLink, side: .target), archiveIdentity: identity)
        precondition(inlineCapture.evidenceLabel.contains("inline module")
            && inlineCapture.evidenceLabel.contains("module position and compiler resolution are not established"))
        for (field, changed) in [("target_member", "3" as Any), ("target_file_id", "4"),
            ("target_path", fixture.base.path(Array("src/other.rs".utf8), "src/other.rs")),
            ("target_revision", "4"), ("target_digest", String(repeating: "f", count: 64)),
            ("target_bytes", "1"), ("lexical_import_kind", "rust-module")] {
            var bad = inline, changedRow = inlineRow
            changedRow[field] = changed; bad["links"] = [changedRow, fixture.link(2)]
            reject { _ = try decode(bad) }
        }
        var unavailableInline = inline, unavailableRow = inlineRow
        unavailableRow["resolution"] = "unavailable"
        for field in ["target_revision", "target_digest", "target_bytes"] { unavailableRow[field] = NSNull() }
        unavailableInline["links"] = [unavailableRow, fixture.link(2)]
        reject { _ = try decode(unavailableInline) }
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
    private static func pythonAbsoluteWire(_ f: SavedImportFixture, identity: AtlasSavedIdentity) throws {
        let decode: ([String: Any]) throws -> AtlasSavedImports = {
            try AtlasSavedImports.decode(f.base.encode($0), identity: identity, generation: 1,
                direction: .all, member: nil, start: 0, limit: 64)
        }
        let original = f.page(identity.owner, generation: 1)
        for kind in ["python-import", "python-from"] {
            var value = original, rows = original["links"] as! [[String: Any]]
            for i in rows.indices {
                rows[i]["language"] = "python"; rows[i]["lexical_import_kind"] = kind
                rows[i]["destination"] = "pkg.worker"; rows[i]["resolution_basis"] = "python-absolute-module-candidate"
                rows[i]["source_path"] = f.base.path(Array("src/pkg/main.py".utf8), "src/pkg/main.py")
                rows[i]["target_path"] = f.base.path(Array("src/pkg/worker.py".utf8), "src/pkg/worker.py")
            }
            value["links"] = rows
            let page = try decode(value)
            precondition(page.links[0].canOpenTarget && !page.links[1].canOpenTarget)
            precondition(page.links[0].resolutionLabel.contains("Captured Python source-file candidate")
                && page.links[0].resolutionLabel.contains("runtime import binding is not resolved"))
            precondition(page.links[1].resolutionLabel.contains("recorded without source bytes"))
            for (key, wrong) in [("language", "rust"), ("lexical_import_kind", "rust-use"),
                ("destination", ".worker"), ("resolution_basis", "external-specifier")] {
                var bad = value, changed = rows
                changed[0][key] = wrong; bad["links"] = changed; reject { _ = try decode(bad) }
            }
            var forged = value, foreign = rows
            foreign[0]["language"] = "rust"; foreign[0]["lexical_import_kind"] = "rust-use"
            forged["links"] = foreign; reject { _ = try decode(forged) }
            for resolution in ["missing", "ambiguous", "external", "unresolved"] {
                var bad = value, changed = rows
                changed[0]["resolution"] = resolution
                for key in ["target_member", "target_file_id", "target_path", "target_revision", "target_digest", "target_bytes"] {
                    changed[0][key] = NSNull()
                }
                bad["links"] = changed; reject { _ = try decode(bad) }
            }
            var old = value; old["resolver"] = "fcb-closed-catalog-imports-v7"
            reject { _ = try decode(old) }
        }
    }
    private static func javaWire(_ f: SavedImportFixture, identity: AtlasSavedIdentity) throws {
        let decode: ([String: Any]) throws -> AtlasSavedImports = {
            try AtlasSavedImports.decode(f.base.encode($0), identity: identity, generation: 1,
                direction: .all, member: nil, start: 0, limit: 64)
        }
        let original = f.page(identity.owner, generation: 1)
        for (kind, destination, basis) in [
            ("java-import", "example.Tool", "java-source-file-candidate"),
            ("java-import", "example.Tool.Nested", "java-enclosing-type-candidate"),
            ("java-static-import", "example.Tool.VALUE", "java-source-file-candidate"),
            ("java-static-import", "example.Tool.*", "java-source-file-candidate")
        ] {
            var value = original, rows = original["links"] as! [[String: Any]]
            for i in rows.indices {
                rows[i]["language"] = "java"; rows[i]["lexical_import_kind"] = kind
                rows[i]["destination"] = destination; rows[i]["resolution_basis"] = basis
                rows[i]["source_path"] = f.base.path(Array("src/main/java/app/Main.java".utf8), "src/main/java/app/Main.java")
                rows[i]["target_path"] = f.base.path(Array("src/main/java/example/Tool.java".utf8), "src/main/java/example/Tool.java")
            }
            value["links"] = rows
            let page = try decode(value)
            precondition(page.links[0].canOpenTarget && !page.links[1].canOpenTarget)
            precondition(page.links[0].label == (kind == "java-import" ? "Java type import" : "Java static member import"))
            precondition(page.links[0].resolutionLabel.contains("classpath are not resolved"))
            precondition(page.links[1].resolutionLabel.contains("recorded without source bytes"))
            for (key, wrong) in [("language", "csharp"), ("lexical_import_kind", "go-import"),
                ("resolution_basis", "go-module-package-file-candidate"), ("resolution_basis", "external-specifier"),
                ("resolution_basis", "unresolved-java-wildcard"), ("resolution_basis", "ambiguous-catalog-target")] {
                var bad = value, changed = rows
                changed[0][key] = wrong; bad["links"] = changed; reject { _ = try decode(bad) }
            }
            var wrongLanguage = value, changed = rows
            changed[0]["language"] = "rust"; changed[0]["lexical_import_kind"] = "rust-use"
            wrongLanguage["links"] = changed; reject { _ = try decode(wrongLanguage) }
            for (key, old) in [("extractor", "franken-markdown-d6fbd1d5/import-statements-v6"),
                ("resolver", "fcb-closed-catalog-imports-v6"),
                ("extraction_policy", "rust-use-tree-js-literal-loads-python-cpp-go-v6")] {
                var bad = value; bad[key] = old; reject { _ = try decode(bad) }
            }
        }
        for (resolution, basis) in [("unresolved", "unresolved-java-wildcard"),
            ("missing", "no-catalog-target"), ("ambiguous", "ambiguous-catalog-target")] {
            var value = original, rows = original["links"] as! [[String: Any]]
            for i in rows.indices {
                rows[i]["language"] = "java"; rows[i]["lexical_import_kind"] = "java-import"
                rows[i]["destination"] = resolution == "unresolved" ? "example.*" : "example.Tool"
                rows[i]["resolution"] = resolution; rows[i]["resolution_basis"] = basis
                for key in ["target_member", "target_file_id", "target_revision", "target_digest", "target_path", "target_bytes"] { rows[i][key] = NSNull() }
            }
            value["links"] = rows
            let edge = try decode(value).links[0]
            precondition(!edge.canOpenTarget && edge.targetMember == nil)
            if resolution == "unresolved" {
                precondition(edge.resolutionLabel.contains("on-demand import"))
                var staticWildcard = value, changed = rows
                changed[0]["lexical_import_kind"] = "java-static-import"
                staticWildcard["links"] = changed; reject { _ = try decode(staticWildcard) }
            }
            rows[0]["target_member"] = "3"; rows[0]["target_file_id"] = "4"
            rows[0]["target_path"] = f.base.path(Array("src/main/java/example/Tool.java".utf8), "src/main/java/example/Tool.java")
            value["links"] = rows; reject { _ = try decode(value) }
        }
    }

    private static func cargoDependencyWire(_ f: SavedImportFixture, identity: AtlasSavedIdentity) throws {
        let decode: ([String: Any]) throws -> AtlasSavedImports = {
            try AtlasSavedImports.decode(f.base.encode($0), identity: identity, generation: 1,
                direction: .all, member: nil, start: 0, limit: 64)
        }
        for basis in ["rust-path-dependency-file-candidate", "rust-path-dependency-prefix-candidate"] {
            var value = f.page(identity.owner, generation: 1)
            // The verified source, importer and per-use target manifests can
            // require more reads than the former source + go.mod ceiling.
            value["member_read_calls"] = String(3 * AtlasSavedImportWire.maximumMemberReadCalls)
            var rows = value["links"] as! [[String: Any]]
            for i in rows.indices {
                rows[i]["language"] = "rust"; rows[i]["lexical_import_kind"] = "rust-use"
                rows[i]["destination"] = "dep_alias::model::Thing"; rows[i]["resolution_basis"] = basis
                rows[i]["source_path"] = f.base.path(Array("app/src/main.rs".utf8), "app/src/main.rs")
                rows[i]["target_path"] = f.base.path(Array("dep/src/model.rs".utf8), "dep/src/model.rs")
            }
            value["links"] = rows
            let page = try decode(value)
            precondition(page.links[0].canOpenTarget && !page.links[1].canOpenTarget
                && page.links[0].resolutionLabel.contains("Cargo path dependency")
                && page.links[0].resolutionLabel.contains("compiler resolution is not evaluated"))
            for (key, wrong) in [("language", "go"), ("lexical_import_kind", "rust-module"),
                ("resolution", "external"), ("resolution_basis", "rust-dependency-context-unavailable")] {
                var bad = value, changed = rows
                changed[0][key] = wrong; bad["links"] = changed; reject { _ = try decode(bad) }
            }
            var old = value; old["resolver"] = "fcb-closed-catalog-imports-v12"; reject { _ = try decode(old) }
            var excess = value
            excess["member_read_calls"] = String(4 * AtlasSavedImportWire.maximumMemberReadCalls + 1)
            reject { _ = try decode(excess) }
            var unresolved = value, changed = rows
            for i in changed.indices {
                changed[i]["resolution"] = "unresolved"; changed[i]["resolution_basis"] = "rust-dependency-context-unavailable"
                for key in ["target_member", "target_file_id", "target_revision", "target_digest", "target_path", "target_bytes"] {
                    changed[i][key] = NSNull()
                }
            }
            unresolved["links"] = changed
            let edge = try decode(unresolved).links[0]
            precondition(!edge.canOpenTarget && edge.resolutionLabel.contains("Cargo path dependency context"))
            changed[0]["target_member"] = "3"; changed[0]["target_file_id"] = "4"
            changed[0]["target_path"] = f.base.path(Array("dep/src/model.rs".utf8), "dep/src/model.rs")
            unresolved["links"] = changed; reject { _ = try decode(unresolved) }
        }
    }

    private static func goWire(_ f: SavedImportFixture, identity: AtlasSavedIdentity) throws {
        let decode: ([String: Any]) throws -> AtlasSavedImports = {
            try AtlasSavedImports.decode(f.base.encode($0), identity: identity, generation: 1,
                direction: .all, member: nil, start: 0, limit: 64)
        }
        var value = f.page(identity.owner, generation: 1)
        var rows = value["links"] as! [[String: Any]]
        for i in rows.indices {
            rows[i]["language"] = "go"; rows[i]["lexical_import_kind"] = "go-import"
            rows[i]["destination"] = "example.local/project/pkg"
            rows[i]["resolution_basis"] = "go-module-package-file-candidate"
            rows[i]["source_path"] = f.base.path(Array("main.go".utf8), "main.go")
            rows[i]["target_path"] = f.base.path(Array("pkg/a.go".utf8), "pkg/a.go")
        }
        value["links"] = rows
        let page = try decode(value)
        precondition(page.links[0].canOpenTarget && !page.links[1].canOpenTarget
            && page.links[0].label == "Go package import"
            && page.links[0].resolutionLabel.contains("representative Go package file")
            && page.links[1].resolutionLabel.contains("has no captured bytes"))
        for (key, wrong) in [("language", "python"), ("lexical_import_kind", "python-from"),
            ("resolution_basis", "python-relative-module-candidate"), ("resolution_basis", "ambiguous-catalog-target")] {
            var bad = value, changed = rows
            changed[0][key] = wrong; bad["links"] = changed; reject { _ = try decode(bad) }
        }
        for (basis, label) in [("go-module-context-unavailable", "go.mod context"),
            ("go-nested-module-boundary", "nested Go module")] {
            var unresolved = value, changed = rows
            for i in changed.indices {
                changed[i]["resolution"] = "unresolved"; changed[i]["resolution_basis"] = basis
                for key in ["target_member", "target_file_id", "target_revision", "target_digest", "target_path", "target_bytes"] {
                    changed[i][key] = NSNull()
                }
            }
            unresolved["links"] = changed
            let edge = try decode(unresolved).links[0]
            precondition(!edge.canOpenTarget && edge.resolutionLabel.contains(label))
            changed[0]["target_member"] = "3"; changed[0]["target_file_id"] = "4"
            changed[0]["target_path"] = f.base.path(Array("pkg/a.go".utf8), "pkg/a.go")
            unresolved["links"] = changed; reject { _ = try decode(unresolved) }
        }
    }

    private static func cppWire(_ f: SavedImportFixture, identity: AtlasSavedIdentity) throws {
        let decode: ([String: Any]) throws -> AtlasSavedImports = {
            try AtlasSavedImports.decode(f.base.encode($0), identity: identity, generation: 1,
                direction: .all, member: nil, start: 0, limit: 64)
        }
        var value = f.page(identity.owner, generation: 1)
        var rows = value["links"] as! [[String: Any]]
        for i in rows.indices {
            rows[i]["language"] = "cpp"; rows[i]["lexical_import_kind"] = "cpp-quoted-include"
            rows[i]["destination"] = "detail/value.h"
            rows[i]["resolution_basis"] = "cpp-source-relative-include-candidate"
            rows[i]["source_path"] = f.base.path(Array("src/main.cpp".utf8), "src/main.cpp")
            rows[i]["target_path"] = f.base.path(Array("src/detail/value.h".utf8), "src/detail/value.h")
        }
        value["links"] = rows
        let accepted = try decode(value)
        precondition(accepted.links[0].canOpenTarget && !accepted.links[1].canOpenTarget
            && accepted.links[0].language == "cpp" && accepted.links[0].label == "Quoted C/C++ include")
        for (key, wrong) in [("language", "python"), ("lexical_import_kind", "cpp-angle-include"),
            ("lexical_import_kind", "cpp-computed-include"), ("resolution_basis", "unique-relative-module-candidate"),
            ("resolution_basis", "python-relative-module-candidate"), ("resolution_basis", "ambiguous-catalog-target")] {
            var bad = value, changed = rows
            changed[0][key] = wrong; bad["links"] = changed; reject { _ = try decode(bad) }
        }
        var compiler = value; compiler["compiler_resolved"] = true; reject { _ = try decode(compiler) }
        var oldPolicy = value; oldPolicy["resolver"] = "fcb-closed-catalog-imports-v3"
        reject { _ = try decode(oldPolicy) }
        for (kind, destination, resolution, basis, label) in [
            ("cpp-angle-include", "vector", "external", "external-specifier", "External C/C++ include"),
            ("cpp-computed-include", "HEADER_NAME", "unresolved", "unresolved-cpp-include", "Unresolved computed C/C++ include"),
            ("cpp-quoted-include", "missing.h", "missing", "no-catalog-target", "Quoted C/C++ include"),
            ("cpp-angle-include", "broken\\header", "unresolved", "unsupported-specifier", "External C/C++ include")
        ] {
            var other = value, changed = rows
            for i in changed.indices {
                changed[i]["lexical_import_kind"] = kind; changed[i]["destination"] = destination
                changed[i]["resolution"] = resolution; changed[i]["resolution_basis"] = basis
                for key in ["target_member", "target_file_id", "target_revision", "target_digest", "target_path", "target_bytes"] {
                    changed[i][key] = NSNull()
                }
            }
            other["links"] = changed
            let edge = try decode(other).links[0]
            precondition(!edge.canOpenTarget && edge.targetMember == nil && edge.label == label)
            changed[0]["target_member"] = "3"; changed[0]["target_file_id"] = "4"
            changed[0]["target_path"] = f.base.path(Array("src/detail/value.h".utf8), "src/detail/value.h")
            other["links"] = changed; reject { _ = try decode(other) }
        }
        var partial = value
        partial["source_members"] = "2"; partial["unsupported_source_members"] = "1"
        partial["imports_complete"] = false
        let page = try decode(partial)
        precondition(!page.complete && page.unsupported == 1 && page.links[0].canOpenTarget)
        partial["imports_complete"] = true; reject { _ = try decode(partial) }
    }


    private static func javascriptLoadWire(_ f: SavedImportFixture, identity: AtlasSavedIdentity) throws {
        let decode: ([String: Any]) throws -> AtlasSavedImports = {
            try AtlasSavedImports.decode(f.base.encode($0), identity: identity, generation: 1,
                direction: .all, member: nil, start: 0, limit: 64)
        }
        let original = f.page(identity.owner, generation: 1)
        for (kind, label) in [
            ("javascript-require", "Bare require() candidate (binding not resolved)"),
            ("javascript-dynamic-import", "Literal import() expression")
        ] {
            var value = original, rows = original["links"] as! [[String: Any]]
            for i in rows.indices {
                rows[i]["language"] = "typescript"; rows[i]["lexical_import_kind"] = kind
                rows[i]["destination"] = "./child"; rows[i]["resolution_basis"] = "unique-relative-module-candidate"
                rows[i]["source_path"] = f.base.path(Array("src/main.cts".utf8), "src/main.cts")
                rows[i]["target_path"] = f.base.path(Array("src/child.cjs".utf8), "src/child.cjs")
            }
            value["links"] = rows
            let page = try decode(value)
            precondition(page.links[0].canOpenTarget && !page.links[1].canOpenTarget)
            precondition(page.links[0].kind == kind && page.links[0].label == label)
            for (key, wrong) in [("language", "rust"), ("language", "python"),
                ("resolution_basis", "rust-module-file-candidate"),
                ("resolution_basis", "python-relative-module-candidate"),
                ("resolution_basis", "cpp-source-relative-include-candidate")] {
                var bad = value, changed = rows
                changed[0][key] = wrong; bad["links"] = changed; reject { _ = try decode(bad) }
            }
            for (key, old) in [("extractor", "franken-markdown-d6fbd1d5/import-statements-v4"),
                ("resolver", "fcb-closed-catalog-imports-v4"),
                ("extraction_policy", "rust-use-tree-js-python-and-cpp-literal-include-v4")] {
                var bad = value; bad[key] = old; reject { _ = try decode(bad) }
            }
            for i in rows.indices {
                rows[i]["resolution"] = "external"; rows[i]["resolution_basis"] = "external-specifier"
                rows[i]["destination"] = "package"
                for key in ["target_member", "target_file_id", "target_revision", "target_digest", "target_path", "target_bytes"] {
                    rows[i][key] = NSNull()
                }
            }
            value["links"] = rows
            let external = try decode(value).links[0]
            precondition(!external.canOpenTarget && external.kind == kind)
        }
    }

    private static func typescriptSourceWire(_ f: SavedImportFixture, identity: AtlasSavedIdentity) throws {
        let decode: ([String: Any]) throws -> AtlasSavedImports = {
            try AtlasSavedImports.decode(f.base.encode($0), identity: identity, generation: 1,
                direction: .all, member: nil, start: 0, limit: 64)
        }
        for language in ["typescript", "tsx"] {
            var value = f.page(identity.owner, generation: 1)
            var rows = value["links"] as! [[String: Any]]
            for i in rows.indices {
                rows[i]["language"] = language; rows[i]["lexical_import_kind"] = "javascript-import"
                rows[i]["destination"] = "./child.js"
                rows[i]["resolution_basis"] = "typescript-source-extension-candidate"
                rows[i]["source_path"] = f.base.path(Array("src/main.ts".utf8), "src/main.ts")
                rows[i]["target_path"] = f.base.path(Array("src/child.ts".utf8), "src/child.ts")
            }
            value["links"] = rows
            let page = try decode(value)
            precondition(page.links[0].canOpenTarget && !page.links[1].canOpenTarget)
            precondition(page.links[0].resolutionBasis == "typescript-source-extension-candidate")
            for (key, wrong) in [("language", "javascript"), ("language", "jsx"), ("language", "rust"),
                ("lexical_import_kind", "rust-module"), ("resolution", "missing"), ("resolution", "ambiguous")] {
                var bad = value, changed = rows
                changed[0][key] = wrong; bad["links"] = changed; reject { _ = try decode(bad) }
            }
            var old = value; old["resolver"] = "fcb-closed-catalog-imports-v8"
            reject { _ = try decode(old) }
        }
    }

    private static func rustPathModuleWire(_ f: SavedImportFixture, identity: AtlasSavedIdentity) throws {
        let decode: ([String: Any]) throws -> AtlasSavedImports = {
            try AtlasSavedImports.decode(f.base.encode($0), identity: identity, generation: 1,
                direction: .all, member: nil, start: 0, limit: 64)
        }
        var value = f.page(identity.owner, generation: 1)
        let destination = "platform/custom.rs", prefix = "#[path = \""
        let declaration = prefix + destination + "\"]\npub mod child;"
        var rows = value["links"] as! [[String: Any]]
        for i in rows.indices {
            rows[i]["language"] = "rust"; rows[i]["lexical_import_kind"] = "rust-path-module"
            rows[i]["destination"] = destination; rows[i]["resolution_basis"] = "rust-path-attribute-candidate"
            rows[i]["source_path"] = f.base.path(Array("src/lib.rs".utf8), "src/lib.rs")
            rows[i]["target_path"] = f.base.path(Array("src/platform/custom.rs".utf8), "src/platform/custom.rs")
            rows[i]["evidence_range"] = ["start": "0", "end": String(declaration.utf8.count)]
            rows[i]["specifier_range"] = ["start": String(prefix.utf8.count),
                "end": String(prefix.utf8.count + destination.utf8.count)]
        }
        value["links"] = rows
        let page = try decode(value), link = page.links[0], unavailable = page.links[1]
        precondition(link.canOpenTarget && !unavailable.canOpenTarget
            && link.kind == "rust-path-module" && link.language == "rust"
            && link.label == "Rust path module declaration"
            && link.resolutionBasis == "rust-path-attribute-candidate"
            && link.destination == destination && link.targetPath?.bytes == Array("src/platform/custom.rs".utf8)
            && link.evidence.start == 0 && link.evidence.end == UInt64(declaration.utf8.count)
            && link.specifier.start == UInt64(prefix.utf8.count)
            && link.specifier.end == UInt64(prefix.utf8.count + destination.utf8.count)
            && unavailable.targetMember == 4 && unavailable.targetDigest == nil,
            "Saved path modules retain exact attribute/declaration ranges and unavailable target identity")

        for (kind, label) in [("rust-include", "Rust include! file candidate"),
            ("rust-include-str", "Rust include_str! text candidate"), ("rust-include-bytes", "Rust include_bytes! data candidate")] {
            var includes = value, includedRows = rows
            for i in includedRows.indices {
                includedRows[i]["lexical_import_kind"] = kind
                includedRows[i]["resolution_basis"] = "rust-include-file-candidate"
            }
            includes["links"] = includedRows
            let included = try decode(includes).links[0]
            precondition(included.canOpenTarget && included.label == label)
            includedRows[0]["resolution_basis"] = "rust-path-attribute-candidate"
            includes["links"] = includedRows
            reject { _ = try decode(includes) }
            includedRows[0]["lexical_import_kind"] = "rust-module"
            includedRows[0]["resolution_basis"] = "rust-include-file-candidate"
            includes["links"] = includedRows
            reject { _ = try decode(includes) }
        }
        for (language, kind) in [("rust", "rust-module"), ("rust", "rust-use"),
            ("python", "python-from"), ("javascript", "javascript-import"), ("typescript", "rust-path-module")] {
            var bad = value, changed = rows
            changed[0]["language"] = language; changed[0]["lexical_import_kind"] = kind
            bad["links"] = changed; reject { _ = try decode(bad) }
        }
        for basis in ["rust-module-file-candidate", "rust-use-module-file-candidate",
            "exact-relative-path", "unique-relative-module-candidate"] {
            var bad = value, changed = rows
            changed[0]["resolution_basis"] = basis; bad["links"] = changed
            reject { _ = try decode(bad) }
        }
        for resolution in ["missing", "ambiguous", "external", "unresolved"] {
            var bad = value, changed = rows
            changed[0]["resolution"] = resolution
            for key in ["target_member", "target_file_id", "target_path", "target_revision", "target_digest", "target_bytes"] {
                changed[0][key] = NSNull()
            }
            bad["links"] = changed; reject { _ = try decode(bad) }
        }
        for (key, old) in [("extractor", "franken-markdown-d6fbd1d5/import-statements-v7"),
            ("extraction_policy", "rust-use-tree-js-literal-loads-python-cpp-go-java-v7"),
            ("resolver", "fcb-closed-catalog-imports-v9")] {
            var bad = value; bad[key] = old; reject { _ = try decode(bad) }
        }
    }

    @MainActor private static func largeSourceReaders() async throws {
        precondition(AtlasSavedImportWire.maximumMemberBytes == 1_048_576)
        for encoding in ["utf8", "utf16le", "utf16be"] {
            let fixture = SavedImportFixture(largeEncoding: encoding)
            let browser = AtlasSavedRepositoryModel(transport: fixture.transport)
            browser.open(path: "/saved/repository.fcbs", accessLease: nil)
            await wait("large import archive") { !browser.busy }
            let identity = browser.info!.identity
            func decode(_ value: [String: Any]) throws -> AtlasSavedImports {
                try AtlasSavedImports.decode(fixture.base.encode(value), identity: identity, generation: 1,
                    direction: .all, member: nil, start: 0, limit: 64)
            }
            let wire = fixture.page(identity.owner, generation: 1)
            let admitted = try decode(wire)
            precondition(admitted.links[0].sourceBytes == 1_048_576
                && admitted.links[0].evidence.end == fixture.evidenceEnd)
            var oldLimit = wire; oldLimit["max_member_source_bytes"] = "131072"
            reject { _ = try decode(oldLimit) }
            // The same ceiling bounds the simultaneously held source and Go
            // module payload, even when the aggregate read budget has room.
            var excessPeak = wire
            excessPeak["member_bytes_read"] = "1114112"; excessPeak["peak_source_bytes"] = "1048577"
            reject { _ = try decode(excessPeak) }
            for (field, wrong) in [
                ("source_bytes", "1048577" as Any),
                ("evidence_range", ["start": String(fixture.evidenceStart), "end": "1048577"])
            ] {
                var invalid = wire, rows = wire["links"] as! [[String: Any]]
                rows[0][field] = wrong; invalid["links"] = rows
                reject { _ = try decode(invalid) }
            }
            browser.prepareImports(); await wait("large import build") { !browser.busy }
            let page = browser.imports!, link = page.links[0]
            let source = try await browser.openImport(link, from: page, side: .evidence,
                readerTransport: fixture.base.readerTransport())
            let target = try await browser.openImport(link, from: page, side: .target,
                readerTransport: fixture.base.readerTransport())
            let shown = source.reader.page!, bytes = fixture.source.bytes!
            let unit: UInt64 = encoding == "utf8" ? 1 : 2
            precondition(source.member.observedBytes == 1_048_576
                && shown.identity.encoding == encoding && shown.start == fixture.evidenceStart - 128
                && shown.text.contains("// 😀\r\nmod saved;\n") && !shown.hasReplacements
                && !source.reader.canRefresh)
            let selectedHex = String(shown.originalHex.dropFirst(Int((fixture.evidenceStart - shown.start) * 2))
                .prefix(Int((fixture.evidenceEnd - fixture.evidenceStart) * 2)))
            precondition(selectedHex == fixture.base.hex(bytes[Int(fixture.evidenceStart)..<Int(fixture.evidenceEnd)]),
                "Late import statement copy bytes must come from the retained original encoding")
            precondition(target.member.id == 3 && target.reader.page?.identity.encoding == "utf16be")
            browser.closeArchive()
            precondition(source.reader.canPin && target.reader.canPin)
            let last = fixture.evidenceStart + 69 * 11 * unit
            source.reader.offsetInput = String(last); source.reader.goToByte()
            await wait("large retained import navigation") { !source.reader.busy }
            precondition(source.reader.page?.start == last && source.reader.page?.text == "mod saved;\n"
                && source.reader.page?.originalHex == fixture.base.hex(bytes[Int(last)...])
                && !fixture.base.calls.contains("LIVE-OPEN")
                && fixture.calls.filter { $0.hasPrefix("prepare:") }.count == 1,
                "Archive closure must not recapture or discard the independently retained large source")
            source.reader.close(); target.reader.close(); browser.close()
        }
    }

    @MainActor static func main() async throws {
        try await largeSourceReaders()
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
        try cppWire(f, identity: identity)
        try typescriptSourceWire(f, identity: identity)
        try rustPathModuleWire(f, identity: identity)
        try goWire(f, identity: identity)
        try cargoDependencyWire(f, identity: identity)
        try javaWire(f, identity: identity)
        try pythonAbsoluteWire(f, identity: identity)
        try javascriptLoadWire(f, identity: identity)
        precondition(accepted.complete && accepted.links[0].sourcePath.bytes == f.base.rawPath)
        precondition(accepted.links[0].canOpenTarget && !accepted.links[1].canOpenTarget)
        // Scripted host evidence for conventional inline modules keeps source
        // provenance; the Rust host fixtures perform the actual extraction.
        for (destination, kind, basis) in [
            ("child", "rust-module", "rust-module-file-candidate"),
            ("self::child::Thing", "rust-use", "rust-use-module-prefix-candidate"),
            ("super::child::Thing", "rust-use", "rust-use-module-prefix-candidate")
        ] {
            var nested = f.page(identity.owner, generation: 1), row = f.link(1)
            row["destination"] = destination; row["lexical_import_kind"] = kind; row["resolution_basis"] = basis
            row["target_path"] = f.base.path(Array("src/outer/child.rs".utf8), "src/outer/child.rs")
            nested["links"] = [row, f.link(2)]
            let candidate = try decode(nested).links[0], source = accepted.links[0]
            precondition(candidate.canOpenTarget && candidate.targetPath?.bytes == Array("src/outer/child.rs".utf8))
            precondition(candidate.sourceMember == source.sourceMember && candidate.sourceRevision == source.sourceRevision
                && candidate.sourceDigest == source.sourceDigest && candidate.sourcePath.bytes == source.sourcePath.bytes
                && candidate.evidence == source.evidence && candidate.specifier == source.specifier)
        }
        var interrupted = f.page(identity.owner, generation: 1); interrupted["member_read_calls"] = "1048576"
        let interruptedPage = try decode(interrupted)
        precondition(interruptedPage.readCalls > interruptedPage.sourceBytes, "interrupted snapshot attempts can exceed byte count")
        for (field, value) in [("owner", "99" as Any), ("compiler_resolved", true), ("source_members", "0"),
            ("member_bytes_read", "33554433"), ("retained_source_payload_bytes", "1"), ("next_offset", "1"),
            ("limit", "63"), ("import_generation", "2"), ("peak_source_bytes", "1048577"),
            ("max_member_source_bytes", "131072"),
            ("extractor", "franken-markdown-d6fbd1d5/import-statements-v9"),
            ("extraction_policy", "rust-file-includes-use-tree-js-literal-loads-python-cpp-go-java-v9"),
            ("resolver", "fcb-closed-catalog-imports-v11")] {
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
