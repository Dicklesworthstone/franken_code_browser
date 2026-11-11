import Foundation
import Dispatch

/// Only the foreign boundary is injected. Production page admission, current
/// row authority, recovery and retained-reader scheduling remain in use.
private final class ProjectBrowserFixture: @unchecked Sendable {
    struct Session { var latest: UInt64 = 1; var paused = false }
    let domain = UUID(), entered = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
    let source = Array("captured\n".utf8)
    let digest = "af9f5d825b4a9e493a23ea438db355287c4320107eb98c3542879fef26b82628"
    let headingSource = Array("preface\n\n## Details\n\nTarget.\n".utf8)
    let headingDigest = "12e28cc36046d8f48dd90d5364790e2d5d591c7c9b1658dcd636125a631bd59d"
    let emptyFirst: Bool
    let completeFirst: Bool
    let fragmentStatus: String
    let repositoryRules: Bool
    let ruleFailure: String?
    private let lock = NSLock()
    private var sessions: [UInt64: Session] = [:], readers: [UInt64: [UInt8]] = [:]
    private var receipts: [UInt64: (page: UInt64, file: UInt64)] = [:]
    private var documents: [UInt64: (generation: UInt64, width: UInt64)] = [:]
    private var nextOwner: UInt64 = 40, nextReader: UInt64 = 100
    private var nextDirectFile: UInt64 = 1 << 63
    private var fault: String?, blocked = false
    private var log: [String] = []
    init(emptyFirst: Bool = false, completeFirst: Bool = false, fragmentStatus: String = "unsupported",
        repositoryRules: Bool = false, ruleFailure: String? = nil) {
        precondition(["none", "document-start", "resolved", "missing-heading", "ambiguous",
            "limited", "unsupported", "invalid"].contains(fragmentStatus))
        precondition(ruleFailure == nil || repositoryRules)
        self.emptyFirst = emptyFirst; self.completeFirst = completeFirst; self.fragmentStatus = fragmentStatus
        self.repositoryRules = repositoryRules; self.ruleFailure = ruleFailure
    }
    private func locked<T>(_ work: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return work() }
    var calls: [String] { locked { log } }
    func corrupt(_ value: String) { locked { fault = value } }
    func blockNext() { locked { blocked = true } }
    func json(_ value: [String: Any]) -> String {
        String(decoding: try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self)
    }
    func hex(_ bytes: some Sequence<UInt8>) -> String { bytes.map { String(format: "%02x", $0) }.joined() }
    func path(_ bytes: [UInt8]) -> [String: Any] {
        ["encoding": "unix-bytes", "hex": hex(bytes), "display": String(bytes: bytes, encoding: .utf8) ?? "\\xff.rs"]
    }
    func raw(_ page: UInt64) -> [UInt8] {
        if page == 2 { return [255, 46, 114, 115] }
        if page == 3 && fragmentStatus != "unsupported" { return Array("src/guide.md".utf8) }
        return Array("src/p\(page).rs".utf8)
    }
    private func sourceBytes(for raw: [UInt8]) -> [UInt8] {
        fragmentStatus == "resolved" && raw == self.raw(3) ? headingSource : source
    }
    private func captureDigest(for raw: [UInt8]) -> String {
        fragmentStatus == "resolved" && raw == self.raw(3) ? headingDigest : digest
    }
    private var fragmentSuffix: String {
        switch fragmentStatus {
        case "none": return ""
        case "document-start": return "#"
        case "missing-heading": return "#absent"
        case "ambiguous": return "#repeat-2"
        case "invalid": return "#%ff"
        default: return "#details"
        }
    }
    private var fragmentSlug: String? {
        switch fragmentStatus {
        case "none", "document-start", "invalid": return nil
        case "missing-heading": return "absent"
        case "ambiguous": return "repeat-2"
        default: return "details"
        }
    }
    func row(_ page: UInt64) -> [String: Any] {
        ["file_id": String(page - (emptyFirst ? 1 : 0)), "path": path(raw(page)), "observed_bytes": "4096"]
    }
    private func ruleEvidence(scopeComplete: Bool) -> [String: Any] {
        let diagnostics: [[String: Any]] = ruleFailure.map {
            [["path": path(Array("nested/.gitignore".utf8)), "code": $0]]
        } ?? []
        return ["observed_rules_complete": ruleFailure == nil, "scope_complete": scopeComplete,
            "configuration_bytes_read": "24", "configuration_read_calls": "2", "configuration_checks": "6",
            "files_read": "2", "files_loaded": ruleFailure == nil ? "2" : "1", "rules_loaded": "2",
            "compiled_admission_bytes": "256", "match_steps": "12", "failed_files": ruleFailure == nil ? "0" : "1",
            "unresolved_paths": "0", "diagnostics_omitted": "0", "diagnostics": diagnostics,
            "limits": ["max_file_bytes": "16384", "max_total_bytes": "65536", "max_files": "256",
                "max_checks": "32768", "max_read_calls": "65536", "max_rules": "1024", "max_pattern_bytes": "512",
                "max_compiled_bytes": "16777216", "max_match_steps": "1000000", "max_total_match_steps": "64000000"]]
    }
    func header(_ owner: UInt64 = 40, command: String = "info", latest: UInt64 = 1, paused: Bool = false) -> [String: Any] {
        let first = latest > 4 ? latest - 3 : 1
        let pages = Array(first...latest), files = pages.filter { !(emptyFirst && $0 == 1) }
        let count = latest - (emptyFirst ? 1 : 0)
        let partialRules = ruleFailure != nil
        let complete = completeFirst && latest == 1 && !paused && !partialRules
        var out: [String: Any] = ["schema": "fcb.project-catalog-session/1", "status": "ok", "command": command, "owner": String(owner),
            "identity_scope": "retained-session", "ordering": "provisional-discovery-order",
            "policy": "product-defaults/no-rule-files-v1", "native_presented": false,
            "discovery_source_bytes_read": "0", "discovery_source_read_calls": "0",
            "state": partialRules ? "partial" : (paused ? "paused" : (complete ? "complete" : "discovering")),
            "more": !complete && !partialRules, "paused": paused && !partialRules, "discovery_complete": complete,
            "latest_page": String(latest), "first_retained_page": String(first), "retained_pages": String(pages.count),
            "retained_files": String(files.count), "retained_path_bytes": String(files.reduce(0) { $0 + raw($1).count }),
            "catalogued_files": String(count),
            "limits": ["page_entries": "256", "page_path_bytes": "67584", "max_path_bytes": "2048",
                "retained_pages": "4", "retained_files": "1024", "retained_path_bytes": "270336"],
            "discovery": ["files": String(count), "directories": String(latest), "excluded_entries": "0",
                "symlinks_not_followed": "0", "special_objects": "0", "unavailable": "0", "cycles": "0",
                "path_limited": "0", "depth_limited": "0", "queue_refused": "0"]]
        if repositoryRules {
            out["policy"] = "repository-rules-v1"
            out["rule_files_enabled"] = true
            out["rule_policy"] = ruleEvidence(scopeComplete: complete)
        }
        return out
    }
    func page(_ id: UInt64 = 1, owner: UInt64 = 40, latest: UInt64 = 1, paused: Bool = false) -> [String: Any] {
        var out = header(owner, command: "page", latest: latest, paused: paused)
        out["page_id"] = String(id); out["after_page"] = String(id - 1)
        out["files"] = emptyFirst && id == 1 ? [] : [row(id)]
        return out
    }
    private func readerHeader(_ owner: UInt64, raw: [UInt8], command: String) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": command, "owner": String(owner),
            "file_id": "1", "source_revision": "1", "captured_bytes": String(sourceBytes(for: raw).count), "encoding": "utf8",
            "path": path(raw), "capture_origin": "host-supplied", "capture_sha256": captureDigest(for: raw),
            "additional_source_bytes_read": "0", "native_presented": false]
    }
    private func readerInfo(_ owner: UInt64, raw: [UInt8]) -> [String: Any] {
        var out = readerHeader(owner, raw: raw, command: "info")
        out["initial_source_bytes_read"] = "0"; out["initial_read_calls"] = "0"
        out["accepted_query_generation"] = NSNull(); out["accepted_outline_generation"] = NSNull()
        out["accepted_document_generation"] = NSNull()
        return out
    }
    private func window(_ owner: UInt64, raw: [UInt8], start: UInt64, limit: UInt64) -> [String: Any] {
        let source = sourceBytes(for: raw)
        let end = min(UInt64(source.count), start + limit), bytes = source[Int(start)..<Int(end)]
        var out = readerHeader(owner, raw: raw, command: "window")
        out["requested_original_range"] = ["start": String(start), "end": String(end)]
        out["visible_range"] = ["start": String(start), "end": String(end)]
        out["text_kind"] = "logical-captured-text-not-shaped"; out["text"] = String(decoding: bytes, as: UTF8.self)
        out["original_hex"] = hex(bytes); out["range_limited"] = false; out["boundaries_adjusted"] = false
        out["has_replacements"] = false; out["first_physical_line"] = NSNull(); out["selection"] = NSNull()
        out["next_offset"] = end < UInt64(source.count) ? String(end) as Any : NSNull()
        return out
    }
    private func documentDestination(_ raw: [UInt8]) -> String {
        raw == self.raw(3) ? "p1.rs" : (fragmentStatus == "unsupported" ? "p3.rs" : "guide.md") + fragmentSuffix
    }
    /// Bounded logical output at the injected boundary. The sample capture is
    /// unchanged; neither the fixture nor native model implements a Markdown
    /// parser. Rust's real parser/receipt integration has its separate tests.
    private func document(_ owner: UInt64, command: String, generation: UInt64,
        width newWidth: UInt64? = nil, first: UInt64 = 0) -> String? {
        precondition(!Thread.isMainThread)
        return locked {
            guard let raw = readers[owner] else { return nil }
            if let newWidth { documents[owner] = (generation, newWidth) }
            guard let accepted = documents[owner], accepted.generation == generation else { return nil }
            log.append("\(command):\(owner):\(generation)")
            var out = readerHeader(owner, raw: raw, command: command)
            out["document_generation"] = String(generation); out["document_ready"] = true
            out["layout_complete"] = true; out["native_shaped"] = false
            out["rendering"] = "logical-frankenmarkdown-flow"
            out["source_mapping"] = "enclosing-regions-not-glyph-exact"
            out["width_columns"] = String(accepted.width); out["parser_source_base"] = "0"
            out["total_flow_lines"] = "1"; out["total_headings"] = "0"; out["rendered_utf8_bytes"] = "8"
            if command == "document-prepare" || command == "document-window" {
                precondition(first == 0)
                out["first_flow_line"] = "0"; out["next_flow_line"] = NSNull(); out["whole_document_visible"] = true
                out["flow_lines"] = [["flow_line": "0", "text": "captured",
                    "rendered_utf8_range": ["start": "0", "end": "8"],
                    "enclosing_original_range": ["start": "0", "end": String(sourceBytes(for: raw).count)]] as [String: Any]]
                out["headings"] = [] as [String]; out["next_heading"] = NSNull()
            } else {
                let destination = documentDestination(raw)
                let link: [String: Any] = ["link_id": "1", "block_index": "0",
                    "evidence_range": ["start": "0", "end": String(sourceBytes(for: raw).count)], "destination": destination,
                    "destination_display": destination, "kind": "local-reference", "fragment_slug": NSNull()]
                out["links_ready"] = true; out["links_complete"] = true; out["links_truncated"] = false
                out["links_seen"] = "1"; out["retained_links"] = "1"; out["parser_diagnostics"] = "0"
                out["evidence_precision"] = "whole-captured-markdown-document"
                if command == "document-link" {
                    out["link"] = link; out["activated"] = false
                } else {
                    precondition(first == 0)
                    out["first_link"] = "0"; out["links"] = [link]; out["next_link"] = NSNull()
                }
            }
            return json(out)
        }
    }
    var documentTransport: AtlasReaderDocumentTransport {
        .init(prepare: { [self] owner, generation, width in
            document(owner, command: "document-prepare", generation: generation, width: width)
        }, window: { [self] owner, generation, first, count in
            precondition(count == 64)
            return document(owner, command: "document-window", generation: generation, first: first)
        }, headings: { _, _, _, _ in nil }, heading: { _, _, _, _ in nil },
        fromSource: { _, _, _, _ in nil }, source: { _, _, _, _, _ in nil }, copy: { _, _, _, _, _ in nil },
        prepareLinks: { [self] owner, generation, limit in
            precondition(limit == 4096)
            return document(owner, command: "document-links-prepare", generation: generation)
        }, links: { [self] owner, generation, first, count in
            precondition(count == 64)
            return document(owner, command: "document-links", generation: generation, first: first)
        }, link: { [self] owner, generation, id, count in
            precondition(id == 1 && count == 64)
            return document(owner, command: "document-link", generation: generation)
        })
    }
    var transport: AtlasProjectBrowserTransport {
        AtlasProjectBrowserTransport(create: { [self] in locked { let id = nextOwner; nextOwner += 1; return id } },
            run: { [self] owner, request in
                precondition(!Thread.isMainThread)
                let delay: Bool
                switch request {
                case .open, .advance, .openReader, .openDocumentLink:
                    delay = locked { let value = blocked; blocked = false; return value }
                default: delay = false
                }
                let response: String? = locked {
                    switch request {
                    case .open:
                        sessions[owner] = Session(); log.append("open:\(owner)")
                        if fault == "open-lost" { fault = nil; return nil }
                        if fault == "open-recovery-lost" { fault = "info-lost"; return nil }
                        return json(page(owner: owner))
                    case .info:
                        guard let session = sessions[owner] else { return nil }
                        log.append("info:\(owner)")
                        if fault == "info-lost" { fault = nil; return nil }
                        return json(header(owner, latest: session.latest, paused: session.paused))
                    case .page(let id):
                        guard let session = sessions[owner], id > 0, id <= session.latest, session.latest - id < 4 else { return nil }
                        log.append("page:\(owner):\(id)")
                        return json(page(id, owner: owner, latest: session.latest, paused: session.paused))
                    case .advance(let after):
                        guard var session = sessions[owner], after == session.latest else { return nil }
                        session.latest += 1; session.paused = false; sessions[owner] = session
                        log.append("advance:\(owner):\(after)")
                        if fault == "advance-lost" { fault = nil; return nil }
                        return json(page(session.latest, owner: owner, latest: session.latest))
                    case .stop:
                        guard var session = sessions[owner] else { return nil }
                        session.paused = true; sessions[owner] = session; log.append("stop:\(owner)")
                        return json(header(owner, command: "stop", latest: session.latest, paused: true))
                    case .openReader(let reader, let pageID, let file, let bytes):
                        guard let session = sessions[owner], pageID <= session.latest, session.latest - pageID < 4,
                              file == pageID - (emptyFirst ? 1 : 0), bytes == AtlasReaderLimits.captureBytes else { return nil }
                        log.append("capture:\(owner):\(pageID):\(file)")
                        let raw = raw(pageID); readers[reader] = raw
                        receipts[reader] = (pageID, file)
                        var out = header(owner, command: "open-reader", latest: session.latest, paused: session.paused)
                        out["page_id"] = String(pageID); out["file_id"] = String(file); out["path"] = path(raw)
                        out["reader_owner"] = String(reader); out["source_observation"] = "new-capture-after-metadata-selection"
                        out["source_bytes_read"] = String(sourceBytes(for: raw).count); out["source_read_calls"] = "1"
                        var nested = readerInfo(reader, raw: raw)
                        if let fault {
                            self.fault = nil
                            switch fault {
                            case "lost": return nil
                            case "page": out["page_id"] = String(pageID + 1)
                            case "file": out["file_id"] = String(file + 1)
                            case "path": out["path"] = path(Array("other.rs".utf8))
                            case "observation": out["source_observation"] = "metadata-snapshot"
                            case "reader-path": nested["path"] = path(Array("other.rs".utf8))
                            case "reader-generation": nested["accepted_query_generation"] = "1"
                            default: preconditionFailure("unknown capture fault")
                            }
                        }
                        out["reader"] = nested; out["window"] = window(reader, raw: raw, start: 0, limit: 16_384)
                        return json(out)
                    case .openDocumentLink(let origin, let destination, let pageID, let file, let generation, let link, let bytes):
                        guard let session = sessions[owner], let raw = readers[origin],
                              let document = documents[origin], document.generation == generation,
                              let receipt = receipts[origin], receipt.page == pageID, receipt.file == file,
                              link == 1, bytes == AtlasReaderLimits.captureBytes, readers[destination] == nil else { return nil }
                        log.append("link:\(owner):\(origin):\(destination):\(pageID):\(file):\(generation):\(link)")
                        let targetPage: UInt64 = raw == self.raw(3) ? 1 : 3
                        let retained = targetPage <= session.latest && session.latest - targetPage < 4
                        let acceptedPage = retained ? targetPage : 0
                        let acceptedFile = retained ? targetPage - (emptyFirst ? 1 : 0) : nextDirectFile
                        if !retained { nextDirectFile += 1 }
                        let targetRaw = self.raw(targetPage), destinationText = documentDestination(raw)
                        let suffix = targetPage == 3 ? fragmentSuffix : ""
                        let status = targetPage == 3 ? fragmentStatus : "none"
                        let slug = targetPage == 3 ? fragmentSlug : nil
                        let offset: UInt64 = status == "resolved" ? 9 : 0
                        let targetBytes = sourceBytes(for: targetRaw)
                        readers[destination] = targetRaw
                        receipts[destination] = (acceptedPage, acceptedFile)
                        var targetFile = row(targetPage); targetFile["file_id"] = String(acceptedFile)
                        if !retained { targetFile["observed_bytes"] = String(targetBytes.count) }
                        var out = header(owner, command: "open-document-link", latest: session.latest, paused: session.paused)
                        out["source_page_id"] = String(pageID); out["source_file_id"] = String(file)
                        out["source_reader_owner"] = String(origin); out["source_capture_sha256"] = captureDigest(for: raw)
                        out["document_generation"] = String(generation); out["link_id"] = String(link)
                        out["target_page_id"] = String(acceptedPage); out["target_file"] = targetFile
                        out["source_authority"] = "retained-catalog-capture"
                        out["target_resolution"] = retained ? "retained-catalog-entry" : "root-grant-path"
                        out["reader_owner"] = String(destination); out["source_observation"] = "new-capture-after-document-link"
                        out["source_bytes_read"] = String(targetBytes.count); out["source_read_calls"] = "1"
                        out["destination"] = destinationText; out["destination_suffix"] = suffix
                        out["fragment_status"] = status
                        out["fragment_slug"] = slug.map { $0 as Any } ?? NSNull()
                        out["target_range"] = status == "resolved" ? ["start": "9", "end": "20"] as Any : NSNull()
                        out["fragment_suffix"] = suffix; out["initial_original_offset"] = String(offset)
                        var nested = readerInfo(destination, raw: targetRaw)
                        var first = window(destination, raw: targetRaw, start: offset, limit: 16_384)
                        if let fault {
                            self.fault = nil
                            switch fault {
                            case "link-lost": return nil
                            case "link-catalog": out["owner"] = String(owner + 1)
                            case "link-source-sha": out["source_capture_sha256"] = String(repeating: "0", count: 64)
                            case "link-source-owner": out["source_reader_owner"] = String(origin + 99)
                            case "link-source-page": out["source_page_id"] = String(pageID + 1)
                            case "link-source-file": out["source_file_id"] = String(file + 1)
                            case "link-source-authority": out["source_authority"] = "path-label"
                            case "link-authority-fields":
                                out.removeValue(forKey: "source_authority"); out.removeValue(forKey: "target_resolution")
                            case "link-generation": out["document_generation"] = String(generation + 1)
                            case "link-id": out["link_id"] = String(link + 1)
                            case "link-destination": out["destination"] = "other.rs"
                            case "link-target-page": out["target_page_id"] = String(session.latest + 1)
                            case "link-target-resolution": out["target_resolution"] = "invented"
                            case "link-target-resolution-mismatch":
                                out["target_resolution"] = retained ? "root-grant-path" : "retained-catalog-entry"
                            case "link-target-file":
                                var target = targetFile
                                target["file_id"] = retained ? String(session.latest + 1) : "1"
                                out["target_file"] = target
                            case "link-target-zero-file":
                                var target = targetFile; target["file_id"] = "0"; out["target_file"] = target
                            case "link-target-observed-bytes":
                                var target = targetFile; target["observed_bytes"] = String(targetBytes.count + 1); out["target_file"] = target
                            case "link-target-path":
                                var target = targetFile; target["path"] = path(Array("other.rs".utf8)); out["target_file"] = target
                            case "link-reader-owner": out["reader_owner"] = String(destination + 99)
                            case "link-reader-path": nested["path"] = path(Array("other.rs".utf8))
                            case "link-reader-generation": nested["accepted_document_generation"] = "1"
                            case "link-source-bytes": out["source_bytes_read"] = String(targetBytes.count + 1)
                            case "link-observation": out["source_observation"] = "metadata-snapshot"
                            case "link-fragment": out["fragment_suffix"] = "#invented"
                            case "link-fragment-resolved": out["fragment_status"] = "resolved"
                            case "link-fragment-offset":
                                out["initial_original_offset"] = "1"
                                first = window(destination, raw: targetRaw, start: 1, limit: 16_384)
                            case "link-fragment-slug": out["fragment_slug"] = NSNull()
                            case "link-fragment-fields":
                                out.removeValue(forKey: "fragment_slug"); out.removeValue(forKey: "target_range")
                            case "link-fragment-empty-range": out["target_range"] = ["start": "9", "end": "9"]
                            case "link-fragment-outside": out["target_range"] = ["start": "9", "end": String(targetBytes.count + 1)]
                            case "link-fragment-fallback-range": out["fragment_status"] = "unsupported"
                            case "link-fragment-unknown": out["fragment_status"] = "invented"
                            case "link-window-zero": first = window(destination, raw: targetRaw, start: 0, limit: 16_384)
                            case "link-window":
                                // Valid reader/window metadata with different
                                // original bytes must fail the second-window
                                // round trip after destination initialization.
                                let tampered = Array(repeating: UInt8(120), count: targetBytes.count - Int(offset))
                                first["text"] = String(decoding: tampered, as: UTF8.self); first["original_hex"] = hex(tampered)
                            default: preconditionFailure("unknown link receipt fault")
                            }
                        }
                        out["reader"] = nested; out["window"] = first
                        return json(out)
                    }
                }
                // Hold an already accepted foreign result, not the model's
                // validation. This exercises late delivery after cancellation
                // or reflow while the candidate reader really exists.
                if delay {
                    entered.signal(); precondition(resume.wait(timeout: .now() + .seconds(5)) == .success)
                }
                return response
            }, cancel: { [self] owner in locked { log.append("cancel:\(owner)") }; return true },
            close: { [self] owner in locked { sessions.removeValue(forKey: owner); log.append("close:\(owner)") }; return true },
            retirementFailed: { fatalError("catalog retirement failed") }, readerDomain: domain,
            verifiedDocumentLink: { [self] owner, origin, destination, generation, link, bytes in
                verifiedLink(owner: owner, origin: origin, destination: destination,
                    generation: generation, link: link, bytes: bytes)
            })
    }
    /// Inject the distinct service verification receipt. Source-byte comparison
    /// and root confinement belong to the real Rust service tests; native tests
    /// ensure only its qualified result can publish a new target reader.
    private func verifiedLink(owner: UInt64, origin: UInt64, destination: UInt64,
        generation: UInt64, link: UInt64, bytes: UInt64) -> String? {
        precondition(!Thread.isMainThread)
        let verification: (raw: [UInt8], file: UInt64, fault: String?)? = locked {
            guard sessions[owner] != nil, let raw = readers[origin], readers[destination] == nil,
                  documents[origin]?.generation == generation, link == 1,
                  bytes == AtlasReaderLimits.captureBytes else { return nil }
            log.append("verify:\(owner):\(origin):\(destination):\(generation):\(link)")
            let verificationFault = fault?.hasPrefix("verify-") == true ? fault : nil
            if verificationFault != nil { fault = nil }
            if verificationFault == "verify-origin-changed" { return (raw, 0, verificationFault) }
            let file = nextDirectFile; nextDirectFile += 1
            receipts[origin] = (0, file)
            return (raw, file, verificationFault)
        }
        guard let verification else { return nil }
        if verification.fault == "verify-origin-changed" {
            return json(["schema": "fcb.project-catalog-session/1", "status": "error",
                "owner": String(owner), "exit_code": "2",
                "error": ["code": "CATALOG_SESSION_ORIGIN_CAPTURE_CHANGED"]])
        }
        guard let response = transport.run(owner, .openDocumentLink(origin: origin, destination: destination,
            page: 0, file: verification.file, generation: generation, link: link, bytes: bytes)),
              var out = try? JSONSerialization.jsonObject(with: Data(response.utf8)) as? [String: Any] else { return nil }
        guard out["status"] as? String == "ok" else { return response }
        let count = sourceBytes(for: verification.raw).count
        out["source_authority"] = "verified-current-source"
        out["source_verification_bytes_read"] = String(count)
        out["source_verification_read_calls"] = count == 0 ? "0" : "1"
        switch verification.fault {
        case "verify-command": out["command"] = "open-verified-document-link"
        case "verify-authority": out["source_authority"] = "retained-catalog-capture"
        case "verify-page": out["source_page_id"] = "1"
        case "verify-file": out["source_file_id"] = "1"
        case "verify-digest": out["source_capture_sha256"] = String(repeating: "0", count: 64)
        case "verify-bytes": out["source_verification_bytes_read"] = String(count + 1)
        case "verify-calls": out["source_verification_read_calls"] = "0"
        case "verify-call-limit": out["source_verification_read_calls"] = "1048577"
        case "verify-fields":
            out.removeValue(forKey: "source_verification_bytes_read"); out.removeValue(forKey: "source_verification_read_calls")
        case nil: break
        default: preconditionFailure("unknown verification receipt fault")
        }
        return json(out)
    }
    var readerTransport: AtlasReaderTransport {
        AtlasReaderTransport(create: { [self] in locked { let id = nextReader; nextReader += 1; log.append("reader-create:\(id)"); return id } },
            open: { [self] _, _, _ in locked { log.append("LABEL-OPEN") }; return nil },
            window: { [self] owner, start, limit in locked {
                guard let raw = readers[owner] else { return nil }
                log.append("reader-window:\(owner)")
                log.append("reader-window-offset:\(owner):\(start)")
                return json(window(owner, raw: raw, start: start, limit: limit))
            } }, lines: { _, _, _, _ in nil },
            cancel: { [self] owner in locked { log.append("reader-cancel:\(owner)") }; return true },
            close: { [self] owner in locked {
                readers.removeValue(forKey: owner); documents.removeValue(forKey: owner)
                receipts.removeValue(forKey: owner); log.append("reader-close:\(owner)")
            }; return true },
            retirementFailed: { fatalError("reader retirement failed") },
            fork: { [self] origin, destination in locked {
                guard let raw = readers[origin], readers[destination] == nil else { return nil }
                readers[destination] = raw; log.append("reader-fork:\(origin):\(destination)")
                return json(readerInfo(destination, raw: raw))
            } }, comparison: .init(domain: domain, compare: { _, _, _, _, _ in nil }))
    }
}

@main private enum AtlasProjectBrowserTests {
    @MainActor private static func wait(_ message: String, _ ready: () -> Bool) async {
        for _ in 0..<2000 { if ready() { return }; try? await Task.sleep(nanoseconds: 1_000_000) }
        preconditionFailure(message)
    }
    private static func reject(_ message: String, _ operation: () throws -> Void) {
        do { try operation(); preconditionFailure(message) } catch { }
    }
    @MainActor private static func refuse(_ message: String, _ operation: @MainActor () async throws -> Void) async {
        do { try await operation(); preconditionFailure(message) } catch { }
    }
    @MainActor static func main() async throws {
        try boundedWireAndCoverage()
        try repositoryRuleWireAndCoverage()
        try await repositoryRuleFailuresKeepRowsAndDirtyHints()
        try await emptyPagesLostRepliesPauseAndEviction()
        try await exactRawFileCaptureAndForgedReceipts()
        try await closeDrainsGrantAndLateCapture()
        try await refreshKeepsAcceptedProjectUsableUntilPublication()
        try await failedAndRejectedRefreshKeepTheAcceptedProject()
        try await canceledRefreshDrainsBeforeRetryAndRetainsItsGrant()
        try await refreshPreservesPartialAndEmptyCoverage()
        await refreshRejectsInvalidRootsAndCancelBeforeEntry()
        try await capturedDocumentLinkOpensIndependentChain()
        try await forgedDocumentLinkReceiptsAreRefused()
        try await unobservedLinkTargetOpensWithoutDiscovery()
        try await evictedOriginAndTargetKeepCapturedLinkAuthority()
        try await forgedDirectLinkReceiptsAreRefused()
        try await verifiedPinnedSourceOpensAndChainsWithoutDiscovery()
        try await unqualifiedVerifiedLinksDoNotCreateCandidates()
        try await changedVerifiedOriginKeepsRetainedReader()
        try await forgedVerifiedLinkReceiptsAreRefused()
        try await interruptedVerifiedLinksRetireOnlyCandidates()
        try await staleAndInterruptedDocumentLinksRetireOnlyCandidates()
        try await qualifiedHeadingOffsetStartsReaderAndHistoryAtCapturedAnchor()
        try await typedFragmentFallbacksKeepReadableTargets()
        try await malformedQualifiedHeadingReceiptsNeverPublishTargets()
        print("Progressive project pages, repository-rule coverage, staged refresh, recovery, raw-file capture, page-independent document-link proofs, independent targets and grant-lifetime regressions passed.")
    }
    private static func boundedWireAndCoverage() throws {
        let f = ProjectBrowserFixture()
        let first = try AtlasProjectBrowserPage.decode(f.json(f.page()), owner: 40, page: 1)
        precondition(first.files.count == 1 && first.info.more && !first.info.complete)
        var large = f.page(20_001, latest: 20_001)
        let admitted = try AtlasProjectBrowserPage.decode(f.json(large), owner: 40, page: 20_001)
        precondition(admitted.info.discovered == 20_001 && admitted.files[0].id == 20_001 && admitted.info.retainedPages == 4)
        large["state"] = "complete"; large["more"] = false; large["discovery_complete"] = true
        let finished = try AtlasProjectBrowserPage.decode(f.json(large), owner: 40)
        precondition(finished.info.complete && !finished.info.more)
        for field in ["owner", "identity_scope", "retained_pages", "first_retained_page", "more", "after_page",
                      "discovery_source_bytes_read", "catalogued_files", "files"] {
            var bad = f.page()
            switch field {
            case "owner": bad[field] = "41"
            case "identity_scope": bad[field] = "response-local"
            case "retained_pages": bad[field] = "5"
            case "first_retained_page": bad[field] = "0"
            case "more": bad[field] = false
            case "after_page": bad[field] = "1"
            case "discovery_source_bytes_read": bad[field] = "1"
            case "catalogued_files": bad[field] = "2"
            default: bad[field] = Array(repeating: f.row(1), count: 257)
            }
            reject("forged catalog page accepted: \(field)") { _ = try AtlasProjectBrowserPage.decode(f.json(bad), owner: 40, page: 1) }
        }
        var omitted = large, discovery = omitted["discovery"] as! [String: String]
        discovery["unavailable"] = "1"; omitted["discovery"] = discovery
        reject("complete discovery hid an unavailable entry") { _ = try AtlasProjectBrowserPage.decode(f.json(omitted), owner: 40) }
        omitted["state"] = "partial"; omitted["discovery_complete"] = false
        let partial = try AtlasProjectBrowserPage.decode(f.json(omitted), owner: 40)
        precondition(!partial.info.more && !partial.info.complete && partial.info.unavailable == 1)
        var unknownFile = f.page(), row = f.row(1)
        row["file_id"] = "2"; unknownFile["files"] = [row]
        reject("row ID exceeded the discovered identity domain") { _ = try AtlasProjectBrowserPage.decode(f.json(unknownFile), owner: 40) }
        var collision = f.page(), counts = collision["discovery"] as! [String: String]
        let directFile = String(UInt64(1) << 63)
        collision["catalogued_files"] = directFile; counts["files"] = directFile; collision["discovery"] = counts
        reject("metadata claimed the direct-capture identity range") { _ = try AtlasProjectBrowserPage.decode(f.json(collision), owner: 40) }
        var unordered = f.page(2, latest: 2)
        unordered["files"] = [f.row(2), f.row(1)]
        reject("non-increasing file IDs were accepted") { _ = try AtlasProjectBrowserPage.decode(f.json(unordered), owner: 40) }
    }
    private static func repositoryRuleWireAndCoverage() throws {
        // The production page/info decoders accept old service receipts, but
        // new policy claims require evidence even when the page has no rows.
        let legacy = ProjectBrowserFixture()
        let old = try AtlasProjectBrowserPage.decode(legacy.json(legacy.page()), owner: 40)
        precondition(old.info.policy.name == "product-defaults/no-rule-files-v1" && old.info.policy.evidence == nil)
        var explicitLegacy = legacy.header()
        explicitLegacy["rule_files_enabled"] = false; explicitLegacy["rule_policy"] = NSNull()
        let legacyInfo = try AtlasProjectBrowserInfo.decode(legacy.json(explicitLegacy), owner: 40)
        precondition(legacyInfo.policy == old.info.policy)

        let progressive = ProjectBrowserFixture(repositoryRules: true)
        let first = try AtlasProjectBrowserPage.decode(progressive.json(progressive.page()), owner: 40, page: 1)
        let evidence = first.info.policy.evidence!
        precondition(first.info.policy.name == "repository-rules-v1" && first.info.more && !first.info.complete)
        precondition(evidence.observedRulesComplete && !evidence.scopeComplete
            && evidence.configurationBytesRead == 24 && evidence.configurationReadCalls == 2
            && evidence.configurationChecks == 6 && evidence.filesRead == 2 && evidence.filesLoaded == 2
            && evidence.rulesLoaded == 2 && evidence.compiledAdmissionBytes == 256 && evidence.matchSteps == 12)
        precondition(first.files[0].rawPath == progressive.raw(1) && evidence.diagnostics.isEmpty)
        let paused = try AtlasProjectBrowserInfo.decode(progressive.json(progressive.header(command: "stop", paused: true)),
            owner: 40, command: "stop")
        precondition(paused.paused && paused.more && !paused.complete && paused.policy == first.info.policy)

        let complete = ProjectBrowserFixture(completeFirst: true, repositoryRules: true)
        let finished = try AtlasProjectBrowserPage.decode(complete.json(complete.page()), owner: 40)
        let info = try AtlasProjectBrowserInfo.decode(complete.json(complete.header()), owner: 40)
        precondition(finished.info.complete && !finished.info.more && info.policy == finished.info.policy
            && info.policy.evidence?.scopeComplete == true && info.omissionCount == 0)
        let empty = ProjectBrowserFixture(emptyFirst: true, completeFirst: true, repositoryRules: true)
        let emptyPage = try AtlasProjectBrowserPage.decode(empty.json(empty.page()), owner: 40)
        precondition(emptyPage.files.isEmpty && emptyPage.info.complete && emptyPage.info.policy.evidence?.scopeComplete == true)

        for fault in ["unknown-policy", "legacy-with-rules", "missing-enabled", "disabled", "missing-evidence",
                      "scope-progressive", "scope-incomplete", "failed-complete", "configuration-bytes",
                      "configuration-type", "rule-limits", "source-bytes", "source-calls"] {
            var bad = complete.page(), proof = bad["rule_policy"] as! [String: Any]
            switch fault {
            case "unknown-policy": bad["policy"] = "repository-rules-invented"
            case "legacy-with-rules": bad["policy"] = "product-defaults/no-rule-files-v1"
            case "missing-enabled": bad.removeValue(forKey: "rule_files_enabled")
            case "disabled": bad["rule_files_enabled"] = false
            case "missing-evidence": bad["rule_policy"] = NSNull()
            case "scope-progressive":
                bad["state"] = "discovering"; bad["more"] = true; bad["discovery_complete"] = false
            case "scope-incomplete": proof["scope_complete"] = false; bad["rule_policy"] = proof
            case "failed-complete":
                proof["failed_files"] = "1"; proof["observed_rules_complete"] = false
                proof["scope_complete"] = false; bad["rule_policy"] = proof
            case "configuration-bytes": proof["configuration_bytes_read"] = "65537"; bad["rule_policy"] = proof
            case "configuration-type": proof["configuration_read_calls"] = 2; bad["rule_policy"] = proof
            case "rule-limits":
                var limits = proof["limits"] as! [String: String]
                limits["max_total_bytes"] = "131072"; proof["limits"] = limits; bad["rule_policy"] = proof
            case "source-bytes": bad["discovery_source_bytes_read"] = "1"
            default: bad["discovery_source_read_calls"] = "1"
            }
            reject("browser page trusted invalid rule coverage: \(fault)") {
                _ = try AtlasProjectBrowserPage.decode(complete.json(bad), owner: 40)
            }
            bad["command"] = "info"
            reject("browser recovery info trusted invalid rule coverage: \(fault)") {
                _ = try AtlasProjectBrowserInfo.decode(complete.json(bad), owner: 40)
            }
        }
    }
    @MainActor private static func repositoryRuleFailuresKeepRowsAndDirtyHints() async throws {
        for failure in ["RULE_UNSUPPORTED_PATTERN", "RULE_READ_FAILED"] {
            let f = ProjectBrowserFixture(completeFirst: true, repositoryRules: true, ruleFailure: failure)
            let model = AtlasProjectBrowserModel(transport: f.transport)
            try await model.open(root: "/repo", accessLease: nil)
            let page = model.page!, policy = page.info.policy, evidence = policy.evidence!
            precondition(model.currentPage && model.canOperate && !model.canContinue && page.info.state == "partial")
            precondition(!page.info.complete && !page.info.more && page.info.omissionCount == 0
                && !evidence.observedRulesComplete && !evidence.scopeComplete && evidence.failedFiles == 1)
            precondition(evidence.diagnostics.count == 1 && evidence.diagnostics[0].code == failure
                && evidence.diagnostics[0].path.hex == f.hex(Array("nested/.gitignore".utf8)))
            precondition(policy.summary.contains("affected paths were omitted")
                && policy.summary.contains(failure == "RULE_UNSUPPORTED_PATTERN"
                    ? "unsupported ignore pattern" : "ignore file could not be read"))
            // General directory omissions can be zero while rule scope is
            // unresolved. That exhausted page must not acknowledge dirty hints.
            var changes = AtlasProjectChangeState()
            let watcher = UUID(), project = UUID()
            changes.bind(watcher)
            changes.receive(AtlasProjectChangeDelivery(generation: watcher, monitoring: true, hints: .contentsChanged))
            changes.beginDiscovery(project: project)
            changes.reportDiscovery(project: project, complete: page.info.complete, more: page.info.more,
                omissions: page.info.omissionCount, pendingChanges: false)
            precondition(changes.hints.contains(.contentsChanged))
            // Rule failures do not revoke qualified rows elsewhere in the
            // project. Reader receipts and replayed info carry the same policy.
            let capture = try await model.openFile(page.files[0], from: page, readerTransport: f.readerTransport)
            precondition(capture.reader.page?.text == "captured\n" && model.info?.policy == policy)
            model.reload(); await wait("partial rule coverage did not survive page recovery") { !model.busy }
            precondition(model.currentPage && model.info?.policy == policy && model.info?.complete == false)
            model.close()
            precondition(capture.reader.canPin && !f.calls.contains("LABEL-OPEN"))
            capture.reader.close()
        }
    }
    @MainActor private static func emptyPagesLostRepliesPauseAndEviction() async throws {
        let failed = ProjectBrowserFixture(), recovered = AtlasProjectBrowserModel(transport: failed.transport)
        failed.corrupt("open-recovery-lost")
        await refuse("an unverifiable initial page was accepted") { try await recovered.open(root: "/repo", accessLease: nil) }
        precondition(recovered.info == nil && recovered.page == nil)
        recovered.reload(); await wait("manual initial-page recovery did not finish") { !recovered.busy }
        precondition(recovered.currentPage && recovered.canContinue && failed.calls.filter { $0 == "open:40" }.count == 1)
        recovered.close()
        let f = ProjectBrowserFixture(emptyFirst: true), model = AtlasProjectBrowserModel(transport: f.transport)
        f.corrupt("open-lost"); try await model.open(root: "/repo", accessLease: nil)
        precondition(model.page?.files.isEmpty == true && model.canContinue)
        precondition(f.calls.filter { $0 == "open:40" }.count == 1 && f.calls.contains("page:40:1"))
        f.corrupt("advance-lost"); model.advance()
        await wait("lost consumed page did not recover") { !model.busy }
        precondition(model.page?.id == 2 && model.info?.discovered == 1)
        precondition(f.calls.filter { $0 == "advance:40:1" }.count == 1 && f.calls.contains("page:40:2"))
        let old = model.page!
        f.blockNext(); model.advance()
        await wait("advance did not enter") { f.entered.wait(timeout: .now()) == .success }
        model.stop(); precondition(model.busy)
        f.resume.signal(); await wait("paused advance did not drain/reconcile") { !model.busy }
        precondition(model.info?.paused == true && model.page?.id == 3 && model.canContinue)
        for _ in 0..<3 { model.advance(); await wait("discovery did not continue") { !model.busy } }
        precondition(model.page?.id == 6 && model.info?.firstRetained == 3 && model.page?.files.count == 1)
        await refuse("an evicted row authorized a capture") { _ = try await model.openFile(old.files[0], from: old, readerTransport: f.readerTransport) }
        precondition(!f.calls.contains(where: { $0.hasPrefix("reader-create:") }))
        model.load(page: 3); await wait("retained page replay did not finish") { !model.busy }
        precondition(model.page?.id == 3 && model.info?.latest == 6)
        model.close()
    }
    @MainActor private static func exactRawFileCaptureAndForgedReceipts() async throws {
        let f = ProjectBrowserFixture(), model = AtlasProjectBrowserModel(transport: f.transport)
        try await model.open(root: "/repo", accessLease: nil); model.advance()
        await wait("raw filename page did not arrive") { !model.busy }
        let page = model.page!, file = page.files[0]
        precondition(String(bytes: file.rawPath, encoding: .utf8) == nil)
        let capture = try await model.openFile(file, from: page, readerTransport: f.readerTransport)
        precondition(capture.reader.page?.identity.pathBytes == file.rawPath && capture.reader.page?.text == "captured\n")
        precondition(capture.reader.page?.identity.capturedBytes == 9 && file.observedBytes == 4096)
        model.close(); precondition(capture.reader.canPin && !capture.reader.canRefresh)
        capture.reader.offsetInput = "4"; capture.reader.goToByte()
        await wait("captured reader stopped working after project close") { !capture.reader.busy }
        precondition(capture.reader.page?.text == "ured\n" && !f.calls.contains("LABEL-OPEN"))
        capture.reader.close()
        for fault in ["page", "file", "path", "observation", "reader-path", "reader-generation", "lost"] {
            let f = ProjectBrowserFixture(), model = AtlasProjectBrowserModel(transport: f.transport)
            try await model.open(root: "/repo", accessLease: nil)
            let page = model.page!; f.corrupt(fault)
            await refuse("corrupt file capture was published: \(fault)") {
                _ = try await model.openFile(page.files[0], from: page, readerTransport: f.readerTransport)
            }
            await wait("corrupt reader did not retire") { f.calls.contains("reader-close:100") }
            precondition(f.calls.filter { $0 == "capture:40:1:1" }.count == 1 && !f.calls.contains("LABEL-OPEN"))
            model.close()
        }
    }
    @MainActor private static func closeDrainsGrantAndLateCapture() async throws {
        final class Grant { }
        let f = ProjectBrowserFixture(), model = AtlasProjectBrowserModel(transport: f.transport)
        var grant: Grant? = Grant(); weak var observed = grant
        try await model.open(root: "/repo", accessLease: grant); grant = nil
        let page = model.page!; f.blockNext()
        let pending = Task { try await model.openFile(page.files[0], from: page, readerTransport: f.readerTransport) }
        await wait("capture did not enter") { f.entered.wait(timeout: .now()) == .success }
        model.close(); precondition(observed != nil && !f.calls.contains("close:40"))
        f.resume.signal()
        await refuse("closed project published a late reader") { _ = try await pending.value }
        await wait("closed project/capture failed to retire") { f.calls.contains("close:40") && f.calls.contains("reader-close:100") && observed == nil }
        precondition(model.info == nil && model.page == nil && !f.calls.contains("LABEL-OPEN"))
    }
    @MainActor private static func refreshKeepsAcceptedProjectUsableUntilPublication() async throws {
        let f = ProjectBrowserFixture(), old = AtlasProjectBrowserModel(transport: f.transport)
        try await old.open(root: "/repo", accessLease: nil)
        let page = old.page!, previousProject = UUID()
        let capture = try await old.openFile(page.files[0], from: page, readerTransport: f.readerTransport)
        let original = capture.reader.page!
        let refresh = AtlasProjectRefresh(transport: f.transport)
        var installed: AtlasProjectBrowserModel?, publishedProject: UUID?
        f.blockNext()
        let candidateProject = refresh.stage(root: "/repo", project: previousProject, accessLease: nil,
            accepted: { candidate in
                precondition(!old.closed && old.currentPage && candidate.previousProject == previousProject)
                precondition(candidate.project == candidate.opening.projectID && candidate.opening.canBrowse
                    && candidate.opening.canPrepare && !candidate.opening.hasAtlas)
                precondition(candidate.root.utf8.elementsEqual(old.root.utf8) && candidate.accessIdentity == nil)
                installed = candidate.browser; publishedProject = candidate.project
                old.close()
                return true
            }, failed: { _ in preconditionFailure("successful refresh reported failure") })!
        await wait("refresh did not enter its private discovery") { f.entered.wait(timeout: .now()) == .success }
        precondition(refresh.isRefreshing && installed == nil && old.canOperate && old.currentPage)
        precondition(!f.calls.contains("close:40") && !f.calls.contains("reader-close:100"))
        precondition(refresh.stage(root: "/repo", project: previousProject, accessLease: nil,
            accepted: { _ in preconditionFailure("a second candidate was admitted") }, failed: { _ in }) == nil)
        // The accepted browser has its own queue: user browsing remains live
        // even while the replacement's foreign call is blocked.
        old.advance(); await wait("accepted project stopped browsing during refresh") { !old.busy }
        precondition(old.page?.id == 2 && old.currentPage && installed == nil)
        f.resume.signal()
        await wait("refresh did not publish its qualified replacement") { !refresh.isRefreshing }
        let replacement = installed!
        precondition(old.closed && replacement.canContinue && publishedProject == candidateProject
            && candidateProject != previousProject && replacement.info?.owner == 41)
        precondition(capture.reader.page?.identity.matches(original.identity) == true
            && capture.reader.page?.originalHex == original.originalHex && capture.reader.canPin)
        capture.reader.offsetInput = "4"; capture.reader.goToByte()
        await wait("retained source stopped reading after refresh") { !capture.reader.busy }
        precondition(capture.reader.page?.text == "ured\n" && !f.calls.contains("LABEL-OPEN"))
        refresh.close()
        precondition(replacement.canOperate && !replacement.closed)
        capture.reader.close(); replacement.close()
    }
    @MainActor private static func failedAndRejectedRefreshKeepTheAcceptedProject() async throws {
        for mode in ["failed", "rejected"] {
            let f = ProjectBrowserFixture(), old = AtlasProjectBrowserModel(transport: f.transport)
            try await old.open(root: "/repo", accessLease: nil)
            let page = old.page!, project = UUID(), refresh = AtlasProjectRefresh(transport: f.transport)
            var deliveries = 0, failures: [UUID] = []
            if mode == "failed" { f.corrupt("open-recovery-lost") }
            let candidateProject = refresh.stage(root: "/repo", project: project, accessLease: nil,
                accepted: { candidate in
                    deliveries += 1
                    precondition(candidate.browser.currentPage && old.canOperate && !old.closed)
                    return false // The host revoked the original project/root/grant.
                }, failed: { failures.append($0) })!
            await wait("failed/rejected candidate did not drain") { !refresh.isRefreshing }
            await wait("failed/rejected candidate was not retired") { f.calls.contains("close:41") }
            precondition(failures == [candidateProject] && deliveries == (mode == "failed" ? 0 : 1))
            precondition(old.currentPage && old.canOperate && old.page?.id == page.id
                && old.info?.owner == 40 && !f.calls.contains("close:40"))
            precondition(f.calls.filter { $0 == "open:41" }.count == 1
                && f.calls.filter { $0 == "close:41" }.count == 1)
            let capture = try await old.openFile(page.files[0], from: page, readerTransport: f.readerTransport)
            precondition(capture.reader.page?.text == "captured\n")
            refresh.close(); precondition(failures.count == 1)
            capture.reader.close(); old.close()
        }
    }
    @MainActor private static func canceledRefreshDrainsBeforeRetryAndRetainsItsGrant() async throws {
        final class Grant { }
        let f = ProjectBrowserFixture(), old = AtlasProjectBrowserModel(transport: f.transport)
        try await old.open(root: "/repo", accessLease: nil)
        let refresh = AtlasProjectRefresh(transport: f.transport), project = UUID()
        var grant: Grant? = Grant(); weak var observed = grant
        var failures: [UUID] = [], publications = 0
        f.blockNext()
        let candidateProject = refresh.stage(root: "/repo", project: project, accessLease: grant,
            accepted: { _ in publications += 1; return true }, failed: { failures.append($0) })!
        grant = nil
        await wait("cancel fixture did not enter discovery") { f.entered.wait(timeout: .now()) == .success }
        refresh.cancel(); refresh.cancel()
        precondition(failures == [candidateProject] && publications == 0 && old.canOperate)
        precondition(refresh.isRefreshing && observed != nil && !f.calls.contains("close:41"))
        precondition(refresh.stage(root: "/repo", project: project, accessLease: nil,
            accepted: { _ in preconditionFailure("retry accumulated behind canceled work") }, failed: { _ in }) == nil)
        f.resume.signal()
        await wait("canceled discovery/grant did not retire") {
            !refresh.isRefreshing && f.calls.contains("close:41") && observed == nil
        }
        precondition(publications == 0 && failures == [candidateProject] && old.currentPage)
        var installed: AtlasProjectBrowserModel?
        precondition(refresh.stage(root: "/repo", project: project, accessLease: nil,
            accepted: { installed = $0.browser; return true },
            failed: { _ in preconditionFailure("retry failed") }) != nil)
        await wait("refresh slot did not reopen after cancellation drained") { !refresh.isRefreshing }
        precondition(installed?.info?.owner == 42 && installed?.canOperate == true && old.canOperate)
        precondition(f.calls.filter { $0 == "close:41" }.count == 1)
        installed?.close(); old.close()
    }
    @MainActor private static func refreshPreservesPartialAndEmptyCoverage() async throws {
        for complete in [false, true] {
            let f = ProjectBrowserFixture(emptyFirst: true, completeFirst: complete)
            let refresh = AtlasProjectRefresh(transport: f.transport), generation = UUID()
            var changes = AtlasProjectChangeState(), installed: AtlasProjectBrowserModel?
            changes.bind(generation)
            changes.receive(AtlasProjectChangeDelivery(generation: generation, monitoring: true, hints: .contentsChanged))
            let candidateProject = refresh.stage(root: "/repo", project: UUID(), accessLease: nil,
                accepted: { candidate in
                    let info = candidate.browser.info!
                    precondition(candidate.browser.page?.files.isEmpty == true && candidate.opening.canBrowse)
                    precondition(info.complete == complete && info.more == !complete)
                    changes.reportDiscovery(project: candidate.project, complete: info.complete,
                        more: info.more, omissions: info.omissionCount, pendingChanges: false)
                    installed = candidate.browser
                    return true
                }, failed: { changes.cancelDiscovery(project: $0) })!
            changes.beginDiscovery(project: candidateProject)
            await wait("empty first page failed refresh admission") { !refresh.isRefreshing }
            precondition(installed?.currentPage == true && changes.hints.isEmpty == complete)
            precondition(installed?.canContinue == !complete)
            installed?.close()
        }
    }
    @MainActor private static func refreshRejectsInvalidRootsAndCancelBeforeEntry() async {
        let f = ProjectBrowserFixture(), refresh = AtlasProjectRefresh(transport: f.transport)
        for root in ["relative", "/bad\0root", "/" + String(repeating: "a", count: 16_384)] {
            precondition(refresh.stage(root: root, project: UUID(), accessLease: nil,
                accepted: { _ in preconditionFailure("invalid root was opened") },
                failed: { _ in preconditionFailure("invalid root created an attempt") }) == nil)
        }
        var failures: [UUID] = []
        let project = refresh.stage(root: "/repo", project: UUID(), accessLease: nil,
            accepted: { _ in preconditionFailure("canceled pre-entry refresh was published") },
            failed: { failures.append($0) })!
        refresh.close()
        precondition(refresh.isRefreshing && failures == [project])
        await wait("pre-entry cancel did not release its slot") { !refresh.isRefreshing }
        precondition(f.calls.isEmpty && failures == [project])
    }
    @MainActor private static func prepareLink(_ reader: AtlasPagedReaderModel) async -> AtlasReaderDocumentLinkTarget {
        let preview = reader.documentPreview
        preview.prepare()
        await wait("logical document preparation did not finish") { !preview.busy }
        precondition(preview.canNavigate && preview.page?.lines.count == 1)
        preview.prepareLinks()
        await wait("captured link preparation did not finish") { !preview.busy }
        let target = preview.links!.target(id: 1)!
        precondition(preview.allows(target) && target.link.kind == .local)
        return target
    }
    @MainActor private static func linkEnvironment(_ f: ProjectBrowserFixture, retainTarget: Bool = true)
        async throws -> (AtlasProjectBrowserModel, AtlasProjectBrowserCapture, AtlasReaderDocumentLinkTarget) {
        let model = AtlasProjectBrowserModel(transport: f.transport)
        try await model.open(root: "/repo", accessLease: nil)
        let page = model.page!
        let origin = try await model.openFile(page.files[0], from: page,
            readerTransport: f.readerTransport, document: f.documentTransport)
        if retainTarget {
            for _ in 0..<2 { model.advance(); await wait("target metadata was not observed") { !model.busy } }
            precondition(model.info?.latest == 3 && model.info?.retains(origin.pageID) == true)
        }
        return (model, origin, await prepareLink(origin.reader))
    }
    @MainActor private static func capturedDocumentLinkOpensIndependentChain() async throws {
        let f = ProjectBrowserFixture()
        let (model, origin, local) = try await linkEnvironment(f)
        let original = origin.reader.page!, previewID = origin.reader.documentPreview.page!.id
        let sourceReads = f.calls.filter { $0 == "reader-window:100" }.count
        var forwarded: AtlasReaderDocumentLinkTarget?
        origin.reader.documentPreview.openLink(local, openLocal: { target in
            precondition(!origin.reader.documentPreview.busy)
            // Dismissing the source preview after activation drains must leave
            // the accepted captured link available to the project callback.
            origin.reader.documentPreview.cancel(silent: true)
            precondition(origin.reader.documentPreview.allows(target))
            forwarded = target
        })
        await wait("local activation did not reach its explicit project callback") { forwarded != nil }
        precondition(forwarded == local && f.calls.contains("document-link:100:1"))
        let target = try await model.openDocumentLink(forwarded!, from: origin,
            readerTransport: f.readerTransport, document: f.documentTransport)
        precondition(target.catalogOwner == origin.catalogOwner && target.pageID == 3 && target.file.id == 3)
        precondition(target.file.rawPath == f.raw(3) && target.file.observedBytes == 4096)
        precondition(target.reader.page?.identity.capturedBytes == 9 && target.captureSHA256 == f.digest)
        precondition(target.unappliedFragment == "#details" && target.reader.page?.start == 0)
        precondition(target.fragment.status == .unsupported && target.fragment.slug == "details"
            && target.fragment.targetRange == nil && target.fragment.initialOriginalOffset == 0)
        precondition(target.reader.page?.identity.owner == 101 && target.reader.page?.text == "captured\n")
        precondition(origin.reader.page?.identity.matches(original.identity) == true
            && origin.reader.page?.originalHex == original.originalHex && origin.reader.page?.start == original.start)
        precondition(origin.reader.documentPreview.page?.id == previewID && origin.reader.documentPreview.allows(local))
        precondition(f.calls.filter { $0 == "reader-window:100" }.count == sourceReads)
        precondition(!f.calls.contains("reader-close:100") && !f.calls.contains("LABEL-OPEN"))

        let back = await prepareLink(target.reader)
        let second = try await model.openDocumentLink(back, from: target,
            readerTransport: f.readerTransport, document: f.documentTransport)
        precondition(second.catalogOwner == origin.catalogOwner && second.pageID == 1 && second.file.id == origin.file.id)
        precondition(second.reader.page?.identity.pathBytes == origin.file.rawPath
            && second.reader.page?.identity.owner == 102 && second.unappliedFragment.isEmpty)
        precondition(f.calls.contains("link:40:101:102:3:3:1:1"))
        // Closing independent targets is the return path; no original filename
        // is reopened and the origin's capture/preview position remains exact.
        second.reader.close(); target.reader.close()
        await wait("independent targets did not retire") {
            f.calls.contains("reader-close:101") && f.calls.contains("reader-close:102")
        }
        precondition(origin.reader.canPin && origin.reader.page?.originalHex == original.originalHex)
        precondition(origin.reader.documentPreview.page?.id == previewID && !f.calls.contains("reader-close:100"))
        origin.reader.close(); model.close()
    }
    @MainActor private static func forgedDocumentLinkReceiptsAreRefused() async throws {
        for fault in ["link-catalog", "link-source-sha", "link-source-owner", "link-source-page", "link-source-file",
                      "link-source-authority", "link-authority-fields", "link-generation", "link-id", "link-destination", "link-target-page", "link-target-file",
                      "link-target-resolution", "link-target-resolution-mismatch", "link-target-zero-file",
                      "link-target-path", "link-reader-owner", "link-reader-path", "link-reader-generation",
                      "link-source-bytes", "link-observation", "link-fragment", "link-fragment-resolved",
                      "link-fragment-offset", "link-window", "link-lost"] {
            let f = ProjectBrowserFixture()
            let (model, origin, local) = try await linkEnvironment(f)
            let original = origin.reader.page!
            f.corrupt(fault)
            await refuse("forged local-link capture was published: \(fault)") {
                _ = try await model.openDocumentLink(local, from: origin,
                    readerTransport: f.readerTransport, document: f.documentTransport)
            }
            await wait("rejected local-link candidate failed to retire: \(fault)") { f.calls.contains("reader-close:101") }
            precondition(f.calls.filter { $0 == "link:40:100:101:1:1:1:1" }.count == 1)
            precondition(origin.reader.page?.identity.matches(original.identity) == true
                && origin.reader.page?.originalHex == original.originalHex && origin.reader.documentPreview.allows(local))
            precondition(!model.busy && !model.closed && !f.calls.contains("reader-close:100") && !f.calls.contains("LABEL-OPEN"))
            origin.reader.close(); model.close()
        }
    }
    @MainActor private static func unobservedLinkTargetOpensWithoutDiscovery() async throws {
        let f = ProjectBrowserFixture()
        let (model, origin, local) = try await linkEnvironment(f, retainTarget: false)
        let original = origin.reader.page!
        let target = try await model.openDocumentLink(local, from: origin,
            readerTransport: f.readerTransport, document: f.documentTransport)
        precondition(target.pageID == 0 && target.file.id == (UInt64(1) << 63))
        precondition(target.file.rawPath == f.raw(3) && target.file.observedBytes == UInt64(f.source.count))
        precondition(target.reader.page?.identity.owner == 101 && target.reader.page?.text == "captured\n")
        precondition(model.info?.latest == 1 && model.info?.discovered == 1 && model.page?.id == 1)
        precondition(!f.calls.contains(where: { $0.hasPrefix("advance:") }) && !f.calls.contains("LABEL-OPEN"))
        precondition(origin.reader.page?.identity.matches(original.identity) == true
            && origin.reader.page?.originalHex == original.originalHex && origin.reader.documentPreview.allows(local))

        let back = await prepareLink(target.reader)
        let returned = try await model.openDocumentLink(back, from: target,
            readerTransport: f.readerTransport, document: f.documentTransport)
        precondition(returned.pageID == 1 && returned.file.id == origin.file.id)
        precondition(returned.reader.page?.identity.owner == 102 && returned.file.rawPath == origin.file.rawPath)
        precondition(f.calls.contains("link:40:101:102:0:\(target.file.id):1:1"))
        returned.reader.close(); target.reader.close(); origin.reader.close(); model.close()
    }
    @MainActor private static func evictedOriginAndTargetKeepCapturedLinkAuthority() async throws {
        let f = ProjectBrowserFixture()
        let (model, origin, local) = try await linkEnvironment(f)
        let original = origin.reader.page!, previewID = origin.reader.documentPreview.page!.id
        for _ in 0..<4 { model.advance(); await wait("metadata eviction did not finish") { !model.busy } }
        precondition(model.info?.latest == 7 && model.info?.retains(origin.pageID) == false
            && model.info?.retains(3) == false)
        let discoveries = f.calls.filter { $0.hasPrefix("advance:") }.count
        let sourceReads = f.calls.filter { $0 == "reader-window:100" }.count
        let target = try await model.openDocumentLink(local, from: origin,
            readerTransport: f.readerTransport, document: f.documentTransport)
        precondition(target.pageID == 0 && target.file.id == (UInt64(1) << 63) && target.file.rawPath == f.raw(3))
        let back = await prepareLink(target.reader)
        let returned = try await model.openDocumentLink(back, from: target,
            readerTransport: f.readerTransport, document: f.documentTransport)
        precondition(returned.pageID == 0 && returned.file.id == target.file.id + 1
            && returned.file.rawPath == origin.file.rawPath)
        precondition(model.info?.latest == 7 && model.info?.discovered == 7
            && f.calls.filter { $0.hasPrefix("advance:") }.count == discoveries)
        precondition(origin.reader.page?.identity.matches(original.identity) == true
            && origin.reader.page?.originalHex == original.originalHex && origin.reader.page?.start == original.start)
        precondition(origin.reader.documentPreview.page?.id == previewID && origin.reader.documentPreview.allows(local))
        precondition(f.calls.filter { $0 == "reader-window:100" }.count == sourceReads
            && !f.calls.contains("reader-close:100") && !f.calls.contains("LABEL-OPEN"))
        returned.reader.close(); target.reader.close(); origin.reader.close(); model.close()
    }
    @MainActor private static func forgedDirectLinkReceiptsAreRefused() async throws {
        for fault in ["link-source-authority", "link-authority-fields", "link-source-sha", "link-source-page", "link-source-file",
                      "link-target-page", "link-target-file", "link-target-zero-file", "link-target-observed-bytes",
                      "link-target-resolution", "link-target-resolution-mismatch", "link-target-path",
                      "link-reader-path", "link-source-bytes", "link-window", "link-lost"] {
            let f = ProjectBrowserFixture()
            let (model, origin, local) = try await linkEnvironment(f, retainTarget: false)
            let original = origin.reader.page!
            f.corrupt(fault)
            await refuse("forged direct-link capture was published: \(fault)") {
                _ = try await model.openDocumentLink(local, from: origin,
                    readerTransport: f.readerTransport, document: f.documentTransport)
            }
            await wait("rejected direct-link candidate failed to retire: \(fault)") { f.calls.contains("reader-close:101") }
            precondition(model.info?.latest == 1 && model.info?.discovered == 1
                && !f.calls.contains(where: { $0.hasPrefix("advance:") }))
            precondition(origin.reader.page?.identity.matches(original.identity) == true
                && origin.reader.page?.originalHex == original.originalHex && origin.reader.documentPreview.allows(local))
            precondition(!f.calls.contains("reader-close:100") && !f.calls.contains("LABEL-OPEN"))
            origin.reader.close(); model.close()
        }
    }
    @MainActor private static func verifiedLinkEnvironment(_ f: ProjectBrowserFixture)
        async throws -> (AtlasProjectBrowserModel, AtlasPagedReaderModel, AtlasReaderDocumentLinkTarget) {
        let (model, source, _) = try await linkEnvironment(f, retainTarget: false)
        let pinned = try await source.reader.fork()
        source.reader.close()
        await wait("original reader did not retire before independent verification") { f.calls.contains("reader-close:100") }
        precondition(pinned.page?.identity.owner == 101 && pinned.retainedCaptureSHA256 == f.digest)
        precondition(model.canOpenVerifiedDocumentLinks && f.calls.contains("reader-fork:100:101"))
        // A fork preserves the capture, not the catalog's private receipt.
        return (model, pinned, await prepareLink(pinned))
    }
    @MainActor private static func verifiedPinnedSourceOpensAndChainsWithoutDiscovery() async throws {
        let f = ProjectBrowserFixture()
        let (model, origin, local) = try await verifiedLinkEnvironment(f)
        let original = origin.page!, previewID = origin.documentPreview.page!.id
        let target = try await model.openVerifiedDocumentLink(local, from: origin,
            readerTransport: f.readerTransport, document: f.documentTransport)
        precondition(target.pageID == 0 && target.file.id == (UInt64(1) << 63) + 1)
        precondition(target.reader.page?.identity.owner == 102 && target.file.rawPath == f.raw(3))
        precondition(model.info?.latest == 1 && model.info?.discovered == 1
            && !f.calls.contains(where: { $0.hasPrefix("advance:") }))
        precondition(f.calls.contains("verify:40:101:102:1:1")
            && f.calls.filter { $0.hasPrefix("capture:") }.count == 1 && !f.calls.contains("LABEL-OPEN"))
        precondition(origin.page?.identity.matches(original.identity) == true
            && origin.page?.originalHex == original.originalHex && origin.page?.start == original.start)
        precondition(origin.documentPreview.page?.id == previewID && origin.documentPreview.allows(local)
            && origin.retainedCaptureSHA256 == f.digest && origin.canPin)
        precondition(!f.calls.contains("reader-close:101") && model.notice.contains("Verified this capture"))

        let back = await prepareLink(target.reader)
        let returned = try await model.openDocumentLink(back, from: target,
            readerTransport: f.readerTransport, document: f.documentTransport)
        precondition(returned.pageID == 1 && returned.file.id == 1
            && returned.reader.page?.identity.owner == 103 && returned.file.rawPath == original.identity.pathBytes)
        precondition(f.calls.filter { $0.hasPrefix("verify:") }.count == 1)
        returned.reader.close(); target.reader.close(); origin.close(); model.close()
    }
    @MainActor private static func changedVerifiedOriginKeepsRetainedReader() async throws {
        let f = ProjectBrowserFixture()
        let (model, origin, local) = try await verifiedLinkEnvironment(f)
        let original = origin.page!, previewID = origin.documentPreview.page!.id
        f.corrupt("verify-origin-changed")
        do {
            _ = try await model.openVerifiedDocumentLink(local, from: origin,
                readerTransport: f.readerTransport, document: f.documentTransport)
            preconditionFailure("a changed source authorized its old captured link")
        } catch {
            guard let error = error as? AtlasProjectBrowserError,
                  case .engine(let code) = error, code == "CATALOG_SESSION_ORIGIN_CAPTURE_CHANGED" else {
                preconditionFailure("changed source lost its qualified service error: \(error)")
            }
        }
        await wait("changed-origin candidate failed to retire") { f.calls.contains("reader-close:102") }
        precondition(model.notice.contains("differs from the retained capture") && model.notice.contains("remains readable"))
        precondition(!f.calls.contains(where: { $0.hasPrefix("link:") }) && !f.calls.contains("reader-close:101"))
        precondition(origin.page?.identity.matches(original.identity) == true
            && origin.page?.originalHex == original.originalHex && origin.documentPreview.page?.id == previewID)
        precondition(origin.documentPreview.allows(local) && origin.canPin && origin.retainedCaptureSHA256 == f.digest)
        origin.close(); model.close()
    }
    @MainActor private static func unqualifiedVerifiedLinksDoNotCreateCandidates() async throws {
        let f = ProjectBrowserFixture()
        let (model, origin, local) = try await verifiedLinkEnvironment(f)
        let allocations = f.calls.filter { $0.hasPrefix("reader-create:") }.count
        let foreign = ProjectBrowserFixture()
        await refuse("a different reader registry authorized source verification") {
            _ = try await model.openVerifiedDocumentLink(local, from: origin,
                readerTransport: foreign.readerTransport, document: foreign.documentTransport)
        }
        precondition(!foreign.calls.contains(where: { $0.hasPrefix("reader-create:") }))
        origin.documentPreview.prepareLinks()
        await wait("link replacement did not finish before verification") { !origin.documentPreview.busy }
        precondition(!origin.documentPreview.allows(local))
        await refuse("stale captured link authorized source verification") {
            _ = try await model.openVerifiedDocumentLink(local, from: origin,
                readerTransport: f.readerTransport, document: f.documentTransport)
        }
        precondition(f.calls.filter { $0.hasPrefix("reader-create:") }.count == allocations
            && !f.calls.contains(where: { $0.hasPrefix("verify:") }))

        let transport = f.transport
        let unsupported = AtlasProjectBrowserModel(transport: .init(create: transport.create, run: transport.run,
            cancel: transport.cancel, close: transport.close, retirementFailed: transport.retirementFailed,
            readerDomain: transport.readerDomain))
        try await unsupported.open(root: "/repo", accessLease: nil)
        precondition(unsupported.canOperate && !unsupported.canOpenVerifiedDocumentLinks)
        let current = origin.documentPreview.links!.target(id: 1)!
        await refuse("a missing verification capability created a target") {
            _ = try await unsupported.openVerifiedDocumentLink(current, from: origin,
                readerTransport: f.readerTransport, document: f.documentTransport)
        }
        precondition(f.calls.filter { $0.hasPrefix("reader-create:") }.count == allocations)
        unsupported.close(); origin.close(); model.close()
    }
    @MainActor private static func forgedVerifiedLinkReceiptsAreRefused() async throws {
        for fault in ["verify-command", "verify-authority", "verify-page", "verify-file", "verify-digest",
                      "verify-bytes", "verify-calls", "verify-call-limit", "verify-fields",
                      "link-generation", "link-id", "link-destination", "link-reader-path", "link-window"] {
            let f = ProjectBrowserFixture()
            let (model, origin, local) = try await verifiedLinkEnvironment(f)
            let original = origin.page!
            f.corrupt(fault)
            await refuse("unqualified verified-link capture was published: \(fault)") {
                _ = try await model.openVerifiedDocumentLink(local, from: origin,
                    readerTransport: f.readerTransport, document: f.documentTransport)
            }
            await wait("invalid verified-link candidate failed to retire: \(fault)") { f.calls.contains("reader-close:102") }
            precondition(origin.page?.identity.matches(original.identity) == true
                && origin.page?.originalHex == original.originalHex && origin.documentPreview.allows(local))
            precondition(!model.busy && !model.closed && !f.calls.contains("reader-close:101") && !f.calls.contains("LABEL-OPEN"))
            origin.close(); model.close()
        }
    }
    @MainActor private static func interruptedVerifiedLinksRetireOnlyCandidates() async throws {
        for interruption in ["cancel", "reflow", "project-close"] {
            let f = ProjectBrowserFixture()
            let (model, origin, local) = try await verifiedLinkEnvironment(f)
            let original = origin.page!
            f.blockNext()
            let pending = Task {
                try await model.openVerifiedDocumentLink(local, from: origin,
                    readerTransport: f.readerTransport, document: f.documentTransport)
            }
            await wait("verified target did not enter") { f.entered.wait(timeout: .now()) == .success }
            precondition(f.calls.contains("verify:40:101:102:1:1") && !f.calls.contains("reader-close:102"))
            switch interruption {
            case "cancel": pending.cancel()
            case "reflow":
                origin.documentPreview.prepare()
                await wait("source reflow did not finish during verified target delivery") {
                    !origin.documentPreview.busy && origin.documentPreview.page?.inventory.generation == 2
                }
            default: model.close()
            }
            f.resume.signal()
            do {
                _ = try await pending.value
                preconditionFailure("late verified target was published after \(interruption)")
            } catch {
                guard let error = error as? AtlasProjectBrowserError, case .canceled = error else {
                    preconditionFailure("verified target lost cancellation priority after \(interruption): \(error)")
                }
            }
            await wait("late verified candidate failed to retire after \(interruption)") { f.calls.contains("reader-close:102") }
            precondition(origin.page?.identity.matches(original.identity) == true
                && origin.page?.originalHex == original.originalHex && origin.canPin)
            precondition(!f.calls.contains("reader-close:101") && !f.calls.contains("LABEL-OPEN"))
            origin.close(); model.close()
        }
    }
    @MainActor private static func staleAndInterruptedDocumentLinksRetireOnlyCandidates() async throws {
        do {
            let f = ProjectBrowserFixture()
            let (model, origin, old) = try await linkEnvironment(f)
            origin.reader.documentPreview.prepareLinks()
            await wait("link-list replacement did not finish") { !origin.reader.documentPreview.busy }
            precondition(!origin.reader.documentPreview.allows(old))
            let allocations = f.calls.filter { $0.hasPrefix("reader-create:") }.count
            await refuse("old link-page token created a destination") {
                _ = try await model.openDocumentLink(old, from: origin,
                    readerTransport: f.readerTransport, document: f.documentTransport)
            }
            precondition(f.calls.filter { $0.hasPrefix("reader-create:") }.count == allocations
                && !f.calls.contains(where: { $0.hasPrefix("link:") }))
            origin.reader.close(); model.close()
        }
        for interruption in ["cancel", "reflow", "project-close"] {
            let f = ProjectBrowserFixture()
            let (model, origin, local) = try await linkEnvironment(f)
            let original = origin.reader.page!
            f.blockNext()
            let pending = Task {
                try await model.openDocumentLink(local, from: origin,
                    readerTransport: f.readerTransport, document: f.documentTransport)
            }
            await wait("local target transfer did not enter") { f.entered.wait(timeout: .now()) == .success }
            precondition(f.calls.contains("link:40:100:101:1:1:1:1") && !f.calls.contains("reader-close:101"))
            switch interruption {
            case "cancel": pending.cancel()
            case "reflow":
                origin.reader.documentPreview.prepare()
                await wait("origin reflow did not finish during delayed target delivery") {
                    !origin.reader.documentPreview.busy && origin.reader.documentPreview.page?.inventory.generation == 2
                }
                precondition(!origin.reader.documentPreview.allows(local))
            default: model.close()
            }
            f.resume.signal()
            do {
                _ = try await pending.value
                preconditionFailure("stale/canceled target was published after \(interruption)")
            } catch {
                guard let error = error as? AtlasProjectBrowserError, case .canceled = error else {
                    preconditionFailure("interrupted transfer lost cancellation priority: \(error)")
                }
            }
            await wait("late target candidate did not retire after \(interruption)") { f.calls.contains("reader-close:101") }
            precondition(origin.reader.page?.identity.matches(original.identity) == true
                && origin.reader.page?.originalHex == original.originalHex && origin.reader.canPin)
            precondition(!f.calls.contains("reader-close:100") && !f.calls.contains("LABEL-OPEN"))
            origin.reader.close(); model.close()
        }
    }
    @MainActor private static func qualifiedHeadingOffsetStartsReaderAndHistoryAtCapturedAnchor() async throws {
        let f = ProjectBrowserFixture(fragmentStatus: "resolved")
        let (model, origin, local) = try await linkEnvironment(f)
        let original = origin.reader.page!
        let target = try await model.openDocumentLink(local, from: origin,
            readerTransport: f.readerTransport, document: f.documentTransport)
        precondition(target.fragment.status == .resolved && target.fragment.slug == "details")
        precondition(target.fragment.targetRange == AtlasDocumentByteRange(start: 9, end: 20))
        precondition(target.fragment.initialOriginalOffset == 9 && !target.fragment.isFallback
            && target.unappliedFragment.isEmpty)
        precondition(target.reader.page?.start == 9 && target.reader.page?.text == "## Details\n\nTarget.\n")
        precondition(target.reader.page?.originalHex == f.hex(f.headingSource.dropFirst(9))
            && target.captureSHA256 == f.headingDigest)
        precondition(f.calls.contains("reader-window-offset:101:9")
            && !f.calls.contains("reader-window-offset:101:0"))
        precondition(!target.reader.canGoBack && !target.reader.canGoForward && target.reader.offsetInput == "9")
        target.reader.offsetInput = "0"; target.reader.goToByte()
        await wait("explicit source navigation did not leave the heading") { !target.reader.busy }
        precondition(target.reader.page?.start == 0 && target.reader.canGoBack)
        target.reader.back()
        await wait("Back did not restore the service-selected heading anchor") { !target.reader.busy }
        precondition(target.reader.page?.start == 9 && target.reader.canGoForward && !target.reader.canGoBack)
        target.reader.forward()
        await wait("Forward did not restore the explicit source page") { !target.reader.busy }
        precondition(target.reader.page?.start == 0 && target.reader.canGoBack)
        target.reader.back(); await wait("second Back did not return to the heading") { !target.reader.busy }
        let link = await prepareLink(target.reader)
        precondition(link.inventory.generation == 1 && f.calls.contains("document-prepare:101:1"))
        precondition(target.reader.page?.start == 9 && target.reader.documentPreview.canNavigate)
        precondition(origin.reader.page?.identity.matches(original.identity) == true
            && origin.reader.page?.originalHex == original.originalHex && !f.calls.contains("LABEL-OPEN"))
        target.reader.close(); origin.reader.close(); model.close()
    }
    @MainActor private static func typedFragmentFallbacksKeepReadableTargets() async throws {
        for status in ["none", "document-start", "missing-heading", "ambiguous", "limited", "unsupported", "invalid"] {
            let f = ProjectBrowserFixture(fragmentStatus: status)
            let (model, origin, local) = try await linkEnvironment(f)
            let target = try await model.openDocumentLink(local, from: origin,
                readerTransport: f.readerTransport, document: f.documentTransport)
            precondition(target.fragment.status.rawValue == status && target.fragment.targetRange == nil)
            precondition(target.fragment.initialOriginalOffset == 0 && target.reader.page?.start == 0
                && target.reader.page?.text == "captured\n")
            let hasSlug = !["none", "document-start", "invalid"].contains(status)
            precondition((target.fragment.slug != nil) == hasSlug)
            precondition(target.fragment.isFallback == !["none", "document-start"].contains(status))
            precondition(!target.fragment.navigationDescription.isEmpty && origin.reader.documentPreview.allows(local))
            target.reader.close(); origin.reader.close(); model.close()
        }
    }
    @MainActor private static func malformedQualifiedHeadingReceiptsNeverPublishTargets() async throws {
        for fault in ["link-fragment-slug", "link-fragment-fields", "link-fragment-empty-range",
                      "link-fragment-outside", "link-fragment-offset", "link-fragment-fallback-range",
                      "link-fragment-unknown", "link-window-zero", "link-window"] {
            let f = ProjectBrowserFixture(fragmentStatus: "resolved")
            let (model, origin, local) = try await linkEnvironment(f)
            let original = origin.reader.page!
            f.corrupt(fault)
            await refuse("malformed nonzero heading target was published: \(fault)") {
                _ = try await model.openDocumentLink(local, from: origin,
                    readerTransport: f.readerTransport, document: f.documentTransport)
            }
            await wait("invalid heading candidate did not retire: \(fault)") { f.calls.contains("reader-close:101") }
            precondition(origin.reader.page?.identity.matches(original.identity) == true
                && origin.reader.page?.originalHex == original.originalHex && origin.reader.documentPreview.allows(local))
            precondition(!f.calls.contains("reader-close:100") && !f.calls.contains("LABEL-OPEN"))
            origin.reader.close(); model.close()
        }
    }
}
