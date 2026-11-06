import Foundation
import Dispatch

/// Only the foreign boundary is injected. Production page admission, current
/// row authority, recovery and retained-reader scheduling remain in use.
private final class ProjectBrowserFixture: @unchecked Sendable {
    struct Session { var latest: UInt64 = 1; var paused = false }
    let domain = UUID(), entered = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
    let source = Array("captured\n".utf8)
    let digest = "af9f5d825b4a9e493a23ea438db355287c4320107eb98c3542879fef26b82628"
    let emptyFirst: Bool
    private let lock = NSLock()
    private var sessions: [UInt64: Session] = [:], readers: [UInt64: [UInt8]] = [:]
    private var documents: [UInt64: (generation: UInt64, width: UInt64)] = [:]
    private var nextOwner: UInt64 = 40, nextReader: UInt64 = 100
    private var fault: String?, blocked = false
    private var log: [String] = []
    init(emptyFirst: Bool = false) { self.emptyFirst = emptyFirst }
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
    func raw(_ page: UInt64) -> [UInt8] { page == 2 ? [255, 46, 114, 115] : Array("src/p\(page).rs".utf8) }
    func row(_ page: UInt64) -> [String: Any] {
        ["file_id": String(page - (emptyFirst ? 1 : 0)), "path": path(raw(page)), "observed_bytes": "4096"]
    }
    func header(_ owner: UInt64 = 40, command: String = "info", latest: UInt64 = 1, paused: Bool = false) -> [String: Any] {
        let first = latest > 4 ? latest - 3 : 1
        let pages = Array(first...latest), files = pages.filter { !(emptyFirst && $0 == 1) }
        let count = latest - (emptyFirst ? 1 : 0)
        return ["schema": "fcb.project-catalog-session/1", "status": "ok", "command": command, "owner": String(owner),
            "identity_scope": "retained-session", "ordering": "provisional-discovery-order",
            "policy": "product-defaults/no-rule-files-v1", "native_presented": false,
            "discovery_source_bytes_read": "0", "discovery_source_read_calls": "0",
            "state": paused ? "paused" : "discovering", "more": true, "paused": paused, "discovery_complete": false,
            "latest_page": String(latest), "first_retained_page": String(first), "retained_pages": String(pages.count),
            "retained_files": String(files.count), "retained_path_bytes": String(files.reduce(0) { $0 + raw($1).count }),
            "catalogued_files": String(count),
            "limits": ["page_entries": "256", "page_path_bytes": "67584", "max_path_bytes": "2048",
                "retained_pages": "4", "retained_files": "1024", "retained_path_bytes": "270336"],
            "discovery": ["files": String(count), "directories": String(latest), "excluded_entries": "0",
                "symlinks_not_followed": "0", "special_objects": "0", "unavailable": "0", "cycles": "0",
                "path_limited": "0", "depth_limited": "0", "queue_refused": "0"]]
    }
    func page(_ id: UInt64 = 1, owner: UInt64 = 40, latest: UInt64 = 1, paused: Bool = false) -> [String: Any] {
        var out = header(owner, command: "page", latest: latest, paused: paused)
        out["page_id"] = String(id); out["after_page"] = String(id - 1)
        out["files"] = emptyFirst && id == 1 ? [] : [row(id)]
        return out
    }
    private func readerHeader(_ owner: UInt64, raw: [UInt8], command: String) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": command, "owner": String(owner),
            "file_id": "1", "source_revision": "1", "captured_bytes": String(source.count), "encoding": "utf8",
            "path": path(raw), "capture_origin": "host-supplied", "capture_sha256": digest,
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
    private func documentDestination(_ raw: [UInt8]) -> String { raw == self.raw(3) ? "p1.rs" : "p3.rs#details" }
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
                    "enclosing_original_range": ["start": "0", "end": "9"]] as [String: Any]]
                out["headings"] = [] as [String]; out["next_heading"] = NSNull()
            } else {
                let destination = documentDestination(raw)
                let link: [String: Any] = ["link_id": "1", "block_index": "0",
                    "evidence_range": ["start": "0", "end": "9"], "destination": destination,
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
                case .advance, .openReader, .openDocumentLink:
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
                        var out = header(owner, command: "open-reader", latest: session.latest, paused: session.paused)
                        out["page_id"] = String(pageID); out["file_id"] = String(file); out["path"] = path(raw)
                        out["reader_owner"] = String(reader); out["source_observation"] = "new-capture-after-metadata-selection"
                        out["source_bytes_read"] = String(source.count); out["source_read_calls"] = "1"
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
                              pageID > 0, pageID <= session.latest, session.latest - pageID < 4,
                              file == pageID - (emptyFirst ? 1 : 0), raw == self.raw(pageID),
                              link == 1, bytes == AtlasReaderLimits.captureBytes, readers[destination] == nil else { return nil }
                        log.append("link:\(owner):\(origin):\(destination):\(pageID):\(file):\(generation):\(link)")
                        let targetPage: UInt64 = raw == self.raw(3) ? 1 : 3
                        guard targetPage <= session.latest, session.latest - targetPage < 4 else {
                            return json(["schema": "fcb.project-catalog-session/1", "status": "error",
                                "owner": String(owner), "exit_code": "2",
                                "error": ["code": "CATALOG_SESSION_LINK_TARGET_NOT_RETAINED"]])
                        }
                        let targetRaw = self.raw(targetPage), destinationText = documentDestination(raw)
                        let suffix = targetPage == 3 ? "#details" : ""
                        readers[destination] = targetRaw
                        var out = header(owner, command: "open-document-link", latest: session.latest, paused: session.paused)
                        out["source_page_id"] = String(pageID); out["source_file_id"] = String(file)
                        out["source_reader_owner"] = String(origin); out["source_capture_sha256"] = digest
                        out["document_generation"] = String(generation); out["link_id"] = String(link)
                        out["target_page_id"] = String(targetPage); out["target_file"] = row(targetPage)
                        out["reader_owner"] = String(destination); out["source_observation"] = "new-capture-after-document-link"
                        out["source_bytes_read"] = String(source.count); out["source_read_calls"] = "1"
                        out["destination"] = destinationText; out["destination_suffix"] = suffix
                        out["fragment_status"] = suffix.isEmpty ? "none" : "not-applied"
                        out["fragment_suffix"] = suffix; out["initial_original_offset"] = "0"
                        var nested = readerInfo(destination, raw: targetRaw)
                        var first = window(destination, raw: targetRaw, start: 0, limit: 16_384)
                        if let fault {
                            self.fault = nil
                            switch fault {
                            case "link-lost": return nil
                            case "link-catalog": out["owner"] = String(owner + 1)
                            case "link-source-sha": out["source_capture_sha256"] = String(repeating: "0", count: 64)
                            case "link-source-owner": out["source_reader_owner"] = String(origin + 99)
                            case "link-source-page": out["source_page_id"] = String(pageID + 1)
                            case "link-source-file": out["source_file_id"] = String(file + 1)
                            case "link-generation": out["document_generation"] = String(generation + 1)
                            case "link-id": out["link_id"] = String(link + 1)
                            case "link-destination": out["destination"] = "other.rs"
                            case "link-target-page": out["target_page_id"] = String(session.latest + 1)
                            case "link-target-file":
                                var target = row(targetPage); target["file_id"] = String(session.latest + 1); out["target_file"] = target
                            case "link-target-path":
                                var target = row(targetPage); target["path"] = path(Array("other.rs".utf8)); out["target_file"] = target
                            case "link-reader-owner": out["reader_owner"] = String(destination + 99)
                            case "link-reader-path": nested["path"] = path(Array("other.rs".utf8))
                            case "link-reader-generation": nested["accepted_document_generation"] = "1"
                            case "link-source-bytes": out["source_bytes_read"] = String(source.count + 1)
                            case "link-observation": out["source_observation"] = "metadata-snapshot"
                            case "link-fragment": out["fragment_suffix"] = "#invented"
                            case "link-fragment-resolved": out["fragment_status"] = "resolved"
                            case "link-fragment-offset": out["initial_original_offset"] = "1"
                            case "link-window":
                                // Valid reader/window metadata with different
                                // original bytes must fail the second-window
                                // round trip after destination initialization.
                                first["text"] = "tampered\n"; first["original_hex"] = hex("tampered\n".utf8)
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
            retirementFailed: { fatalError("catalog retirement failed") }, readerDomain: domain)
    }
    var readerTransport: AtlasReaderTransport {
        AtlasReaderTransport(create: { [self] in locked { let id = nextReader; nextReader += 1; log.append("reader-create:\(id)"); return id } },
            open: { [self] _, _, _ in locked { log.append("LABEL-OPEN") }; return nil },
            window: { [self] owner, start, limit in locked {
                guard let raw = readers[owner] else { return nil }
                log.append("reader-window:\(owner)")
                return json(window(owner, raw: raw, start: start, limit: limit))
            } }, lines: { _, _, _, _ in nil },
            cancel: { [self] owner in locked { log.append("reader-cancel:\(owner)") }; return true },
            close: { [self] owner in locked {
                readers.removeValue(forKey: owner); documents.removeValue(forKey: owner); log.append("reader-close:\(owner)")
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
        try await emptyPagesLostRepliesPauseAndEviction()
        try await exactRawFileCaptureAndForgedReceipts()
        try await closeDrainsGrantAndLateCapture()
        try await capturedDocumentLinkOpensIndependentChain()
        try await forgedDocumentLinkReceiptsAreRefused()
        try await unobservedLinkTargetRemainsRecoverable()
        try await staleAndInterruptedDocumentLinksRetireOnlyCandidates()
        print("Progressive project pages, recovery, raw-file capture, captured document-link proofs, independent targets and grant-lifetime regressions passed.")
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
        var unordered = f.page(2, latest: 2)
        unordered["files"] = [f.row(2), f.row(1)]
        reject("non-increasing file IDs were accepted") { _ = try AtlasProjectBrowserPage.decode(f.json(unordered), owner: 40) }
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
                      "link-generation", "link-id", "link-destination", "link-target-page", "link-target-file",
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
    @MainActor private static func unobservedLinkTargetRemainsRecoverable() async throws {
        let f = ProjectBrowserFixture()
        let (model, origin, local) = try await linkEnvironment(f, retainTarget: false)
        do {
            _ = try await model.openDocumentLink(local, from: origin,
                readerTransport: f.readerTransport, document: f.documentTransport)
            preconditionFailure("an unobserved destination was opened")
        } catch {
            guard let error = error as? AtlasProjectBrowserError,
                  case .engine(let code) = error, code == "CATALOG_SESSION_LINK_TARGET_NOT_RETAINED" else {
                preconditionFailure("membership failure lost its exporting service error: \(error)")
            }
        }
        precondition(model.notice.contains("outside the retained project pages")
            && model.notice.contains("does not mean the file is missing"))
        await wait("unobserved destination's empty reader did not retire") { f.calls.contains("reader-close:101") }
        precondition(origin.reader.documentPreview.allows(local) && !f.calls.contains("LABEL-OPEN"))
        for _ in 0..<2 { model.advance(); await wait("additional metadata discovery did not finish") { !model.busy } }
        let target = try await model.openDocumentLink(local, from: origin,
            readerTransport: f.readerTransport, document: f.documentTransport)
        precondition(target.pageID == 3 && target.reader.page?.identity.owner == 102
            && origin.reader.page?.identity.owner == 100)
        target.reader.close(); origin.reader.close(); model.close()
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
}
