// Production reader/coordinator/model regressions with a deterministic foreign
// boundary. Fixed inventories are protocol fixtures, not a native token parser.
// No Rust extraction, C linkage, or physical macOS presentation is simulated.
import Foundation
import Dispatch

private enum ReferenceModelFixture {
    static let text = String(repeating: "café\n", count: 70) + "second\n"
    static let raw: [UInt8] = [255, 254] + text.utf16.flatMap { [UInt8($0 & 255), UInt8($0 >> 8)] }
    static func hex(_ bytes: some Sequence<UInt8>) -> String { bytes.map { String(format: "%02x", $0) }.joined() }
    static func json(_ value: [String: Any]) -> String {
        String(data: try! JSONSerialization.data(withJSONObject: value), encoding: .utf8)!
    }
    static func range(_ start: UInt64, _ end: UInt64) -> [String: String] {
        ["start": String(start), "end": String(end)]
    }
    static func header(_ command: String, owner: UInt64, origin: String) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": command,
         "owner": String(owner), "file_id": "1", "source_revision": "1",
         "captured_bytes": String(raw.count), "encoding": "utf16le",
         "additional_source_bytes_read": "0", "native_presented": false, "capture_origin": origin,
         "accepted_query_generation": NSNull(), "accepted_outline_generation": NSNull(),
         "accepted_document_generation": NSNull(), "accepted_reference_generation": NSNull(),
         "path": ["encoding": "unix-bytes", "hex": hex("/project/lib.rs".utf8), "display": "/project/lib.rs"]]
    }
    static func count(_ name: String) -> UInt64 {
        if name.utf8.elementsEqual("café".utf8) { return 70 }
        return name.utf8.elementsEqual("second".utf8) ? 1 : 0
    }
    static func original(_ name: String, _ id: UInt64) -> (UInt64, UInt64) {
        let start: UInt64 = name.utf8.elementsEqual("second".utf8) ? 702 : 2 + (id - 1) * 10
        return (start, start + UInt64(name.utf16.count) * 2)
    }
    static func line(_ name: String, _ id: UInt64) -> UInt64 {
        name.utf8.elementsEqual("second".utf8) ? 71 : id
    }
    static func page(owner: UInt64, origin: String, generation: UInt64, name: String,
        preparing: Bool, start: UInt64 = 0, limit: UInt64 = 64) -> String {
        var out = header(preparing ? "references" : "reference-page", owner: owner, origin: origin)
        let total = count(name), end = min(total, start + limit)
        out["reference_generation"] = String(generation); out["name"] = name; out["name_case"] = "sensitive"
        out["selection_namespace"] = "reference"; out["evidence_level"] = "whole-token-text-candidate"
        out["includes_comments_and_literals"] = true; out["compiler_resolved"] = false; out["semantic_complete"] = false
        out["search_complete"] = true; out["count_complete"] = true; out["output_limited"] = false
        out["retained_references"] = String(total); out["matches_counted"] = String(total)
        out["max_items"] = "4096"; out["max_source_bytes"] = "524288"; out["line_semantics"] = "LF-counted-source-lines"
        out["references"] = (start..<end).map { offset -> [String: Any] in
            let id = offset + 1, span = original(name, id)
            return ["reference_id": String(id), "line": String(line(name, id)), "original_range": range(span.0, span.1)]
        }
        out["next_offset"] = end < total ? String(end) as Any : NSNull()
        return json(out)
    }
    static func window(owner: UInt64, origin: String, offset: UInt64 = 0, generation: UInt64? = nil,
        name: String = "", id: UInt64 = 0) -> String {
        var out = header(generation == nil ? "window" : "reference", owner: owner, origin: origin)
        out["text_kind"] = "logical-captured-text-not-shaped"
        out["requested_original_range"] = range(offset, UInt64(raw.count)); out["visible_range"] = range(2, UInt64(raw.count))
        out["range_limited"] = false; out["boundaries_adjusted"] = offset != 2; out["has_replacements"] = false
        out["first_physical_line"] = NSNull(); out["next_offset"] = NSNull()
        out["text"] = text; out["original_hex"] = hex(raw.dropFirst(2)); out["selection"] = NSNull()
        if let generation {
            let span = original(name, id)
            let decoded: UInt64 = name.utf8.elementsEqual("second".utf8) ? 420 : (id - 1) * 6
            out["selection"] = ["reference_generation": String(generation), "reference_id": String(id),
                "reference_line": String(line(name, id)), "selection_namespace": "reference",
                "evidence_level": "whole-token-text-candidate", "includes_comments_and_literals": true,
                "compiler_resolved": false, "original_range": range(span.0, span.1),
                "window_utf8_range": range(decoded, decoded + UInt64(name.utf8.count)),
                "original_hex": hex(raw[Int(span.0)..<Int(span.1)])] as [String: Any]
        }
        return json(out)
    }
}

private final class ReferenceModelDriver: @unchecked Sendable {
    private let lock = NSLock()
    private var next: UInt64 = 7
    private var calls: [String] = []
    private var origins: [UInt64: String] = [:]
    private var accepted: [UInt64: (generation: UInt64, name: String)] = [:]
    private var paused: String?
    private var failure: String?
    let entered = DispatchSemaphore(value: 0), gate = DispatchSemaphore(value: 0)
    var log: [String] { lock.lock(); defer { lock.unlock() }; return calls }
    func pause(_ command: String) { lock.lock(); paused = command; lock.unlock() }
    func failNext(_ code: String) { lock.lock(); failure = code; lock.unlock() }
    private func record(_ value: String) { lock.lock(); calls.append(value); lock.unlock() }
    private func origin(_ owner: UInt64) -> String {
        lock.lock(); defer { lock.unlock() }
        return origins[owner] ?? "regular-file-observation-not-atomic"
    }
    private func snapshot(_ owner: UInt64, generation: UInt64) -> String? {
        lock.lock(); defer { lock.unlock() }
        guard let inventory = accepted[owner], inventory.generation == generation else { return nil }
        return inventory.name
    }
    private func step(_ command: String, owner: UInt64) -> String? {
        precondition(!Thread.isMainThread)
        lock.lock(); let wait = paused == command; if wait { paused = nil }
        let fail = failure; failure = nil; lock.unlock()
        if wait { entered.signal(); precondition(gate.wait(timeout: .now() + .seconds(5)) == .success) }
        return fail.map { ReferenceModelFixture.json(["schema": "fcb.reader-session/1", "owner": String(owner),
            "status": "error", "error": ["code": $0]]) }
    }
    var references: AtlasReaderReferencesTransport {
        AtlasReaderReferencesTransport(prepare: { [self] owner, generation, name, cap in
            precondition(cap == 4096); record("prepare:\(owner):\(generation):\(name)")
            let failure = step("prepare", owner: owner)
            defer { record("returned-prepare:\(owner):\(generation)") }
            if let failure { return failure }
            lock.lock(); accepted[owner] = (generation, name); lock.unlock()
            return ReferenceModelFixture.page(owner: owner, origin: origin(owner), generation: generation,
                name: name, preparing: true)
        }, page: { [self] owner, generation, start, limit in
            record("page:\(owner):\(generation):\(start)")
            if let failure = step("page", owner: owner) { return failure }
            guard let name = snapshot(owner, generation: generation) else { return nil }
            return ReferenceModelFixture.page(owner: owner, origin: origin(owner), generation: generation,
                name: name, preparing: false, start: start, limit: limit)
        }, reference: { [self] owner, generation, id, context in
            precondition(context == 2048); record("reference:\(owner):\(generation):\(id)")
            if let failure = step("reference", owner: owner) { return failure }
            guard let name = snapshot(owner, generation: generation), id > 0, id <= ReferenceModelFixture.count(name) else { return nil }
            return ReferenceModelFixture.window(owner: owner, origin: origin(owner), generation: generation, name: name, id: id)
        })
    }
    var transport: AtlasReaderTransport {
        AtlasReaderTransport(create: { [self] in
            lock.lock(); defer { lock.unlock() }; let owner = next; next += 1; return owner
        }, open: { [self] owner, _, _ in
            precondition(!Thread.isMainThread)
            lock.lock(); origins[owner] = "regular-file-observation-not-atomic"; lock.unlock()
            record("open:\(owner)")
            return ReferenceModelFixture.json(ReferenceModelFixture.header("info", owner: owner, origin: origin(owner)))
        }, window: { [self] owner, offset, limit in
            precondition(!Thread.isMainThread && offset <= 2 && limit >= UInt64(ReferenceModelFixture.raw.count) - offset)
            record("window:\(owner)")
            return ReferenceModelFixture.window(owner: owner, origin: origin(owner), offset: offset)
        }, lines: { _, _, _, _ in nil }, cancel: { [self] owner in record("cancel:\(owner)"); return true },
        close: { [self] owner in
            precondition(!Thread.isMainThread)
            lock.lock(); origins[owner] = nil; accepted[owner] = nil; calls.append("close:\(owner)"); lock.unlock(); return true
        }, retirementFailed: { fatalError("reader retirement failed") }, fork: { [self] source, destination in
            precondition(!Thread.isMainThread)
            lock.lock(); let exists = origins[source] != nil
            if exists { origins[destination] = "host-supplied" }; lock.unlock()
            guard exists else { return nil }
            record("fork:\(source):\(destination)")
            return ReferenceModelFixture.json(ReferenceModelFixture.header("info", owner: destination, origin: "host-supplied"))
        }, references: references)
    }
}

@main struct AtlasReaderReferencesModelTests {
    @MainActor private static func check(_ condition: @autoclosure () -> Bool, _ reason: String) {
        precondition(condition(), reason)
    }
    @MainActor private static func wait(_ reason: String, _ predicate: () -> Bool) {
        let deadline = Date().addingTimeInterval(5)
        var done = predicate()
        while !done, Date() < deadline {
            _ = RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.005))
            done = predicate()
        }
        check(done, reason)
    }
    @MainActor private static func opened(_ driver: ReferenceModelDriver) -> AtlasPagedReaderModel {
        let model = AtlasPagedReaderModel(transport: driver.transport)
        model.open(root: "/project", path: "lib.rs", accessLease: nil)
        wait("source did not open") { !model.busy && model.page != nil }
        return model
    }
    @MainActor private static func find(_ model: AtlasPagedReaderModel, _ name: String = "café") {
        model.referenceName = name; model.findReferences()
        wait("occurrence inventory did not arrive") { !model.busy && model.referencesAreCurrent }
    }
    @MainActor static func main() throws {
        check(ReferenceModelFixture.raw.count == 716 && ReferenceModelFixture.text.utf8.count == 427,
            "fixture byte domains")
        let input = try AtlasReaderInput(root: "/project", path: "lib.rs")

        // Equal identifier spelling is insufficient proof of which occurrence
        // a decoded span represents. Original bytes and both offset domains
        // must agree, including capture-leading BOM exclusion.
        do {
            let origin = "regular-file-observation-not-atomic"
            let info = try AtlasReaderInfo.decode(ReferenceModelFixture.json(
                ReferenceModelFixture.header("info", owner: 7, origin: origin)), handle: 7, path: input.fullPath)
            let inventory = try AtlasReferencePage.decode(ReferenceModelFixture.page(owner: 7, origin: origin,
                generation: 1, name: "café", preparing: true), info: info, generation: 1,
                name: "café", request: .initial, preparing: true)
            let target = inventory.target(id: 1)!
            let response = ReferenceModelFixture.window(owner: 7, origin: origin, generation: 1, name: "café", id: 1)
            let valid = try AtlasReaderPage.decode(response, info: info, request: .reference(target))
            check(valid.referenceSelection?.originalHex == "630061006600e900" && valid.start == 2,
                "reference original-byte evidence")
            let original = try JSONSerialization.jsonObject(with: Data(response.utf8)) as! [String: Any]
            for (key, value) in [("reference_generation", "2"), ("reference_id", "2"),
                ("reference_line", "2"), ("selection_namespace", "outline"), ("query_generation", "1"),
                ("evidence_level", "compiler-resolved"), ("original_hex", "0000000000000000")] {
                var bad = original, selection = bad["selection"] as! [String: Any]
                selection[key] = value; bad["selection"] = selection
                do {
                    _ = try AtlasReaderPage.decode(ReferenceModelFixture.json(bad), info: info, request: .reference(target))
                    fatalError("foreign reference selection admitted: \(key)")
                } catch AtlasReaderError.invalidResponse { }
            }
            var shifted = original, selection = shifted["selection"] as! [String: Any]
            selection["window_utf8_range"] = ReferenceModelFixture.range(6, 11); shifted["selection"] = selection
            do {
                _ = try AtlasReaderPage.decode(ReferenceModelFixture.json(shifted), info: info, request: .reference(target))
                fatalError("second identical occurrence impersonated the selected original bytes")
            } catch AtlasReaderError.invalidResponse { }
        }

        // Local page authority is checked both before transport admission and
        // after an in-flight response drains, even without cooperative cancel.
        do {
            let driver = ReferenceModelDriver(), coordinator = AtlasReaderCoordinator(transport: driver.transport)
            var inventory: AtlasReferencePage?
            Task { _ = try! await coordinator.open(input: input); inventory = try! await coordinator.prepareReferences("café") }
            wait("coordinator inventory") { inventory != nil }
            let target = inventory!.target(id: 1)!
            driver.pause("reference")
            var refused = false
            Task {
                do { _ = try await coordinator.read(.reference(target)); fatalError("revoked occurrence delivered") }
                catch AtlasReaderError.invalidInput { refused = true }
                catch { fatalError("unexpected revocation error: \(error)") }
            }
            wait("selection did not enter foreign call") { driver.entered.wait(timeout: .now()) == .success }
            coordinator.revokeReferencePage(); driver.gate.signal()
            wait("revoked selection did not drain") { refused }
            var staleRefused = false
            Task {
                do { _ = try await coordinator.read(.reference(target)); fatalError("revoked row reached transport") }
                catch AtlasReaderError.invalidInput { staleRefused = true }
                catch { fatalError("unexpected stale-row error: \(error)") }
            }
            wait("stale admission did not return") { staleRefused }
            check(driver.log.filter { $0.hasPrefix("reference:") }.count == 1, "stale row crossed the foreign boundary")
            coordinator.close(); wait("coordinator retirement") { driver.log.contains("close:7") }
        }

        do {
            let driver = ReferenceModelDriver(), model = opened(driver)
            find(model)
            let original = model.referencePage!, oldTarget = original.target(id: 1)!
            check(original.rows.count == 64 && original.nextOffset == 64, "initial reference page")
            model.openReference(oldTarget)
            wait("first occurrence selection") { !model.busy && model.referenceHex != nil }
            check(model.referenceHex == "630061006600e900" && model.nativeSelectionRange == NSRange(location: 0, length: 4),
                "original UTF-16 bytes and native selection were conflated")
            model.fileQuery = "unsubmitted literal"; model.outlineNeedle = "unsubmitted filter"
            check(model.referenceHex != nil, "independent Find/Outline edit revoked occurrence selection")
            model.nextReferencePage()
            wait("next reference page") { !model.busy && model.referencePage?.request.start == 64 }
            check(model.referenceHex == nil && model.referencePage?.rows.first?.id == 65 && model.canReferenceBack,
                "next page reused previous selection or renumbered IDs")
            model.openReference(model.referencePage!.target(id: 65)!)
            wait("retained occurrence 65") { !model.busy && model.referenceHex != nil }
            check(model.nativeSelectionRange == NSRange(location: 320, length: 4)
                && driver.log.contains("reference:7:1:65"), "row zero became occurrence one")
            model.previousReferencePage()
            wait("previous reference page") { !model.busy && model.referencePage?.request == .initial }
            check(model.referencePage?.id != original.id && !model.canReferenceBack, "page UUID/history not renewed")
            let calls = driver.log.filter { $0.hasPrefix("reference:") }.count
            model.openReference(oldTarget)
            check(!model.busy && model.referenceHex == nil
                && driver.log.filter { $0.hasPrefix("reference:") }.count == calls, "old row event acquired a new page's authority")
            model.openReference(model.referencePage!.target(id: 1)!)
            wait("renewed selection") { !model.busy && model.referenceHex != nil }

            // A new pin starts with source only. Parent generations and row
            // tokens cannot select or copy in that independently owned reader.
            var child: AtlasPagedReaderModel?
            Task {
                do { child = try await model.fork() }
                catch { fatalError("retained fork: \(error)") }
            }
            wait("fork did not finish") { child != nil }
            let pinned = child!
            check(pinned.page?.identity.owner == 8 && pinned.referencePage == nil && pinned.referenceHex == nil
                && pinned.nativeSelectionRange == nil && pinned.canFindReferences, "fork inherited reference authority or lost capability")
            pinned.openReference(model.referencePage!.target(id: 1)!)
            check(!pinned.busy && pinned.referenceHex == nil, "parent occurrence selected in child")
            find(pinned, "second")
            check(pinned.referencePage?.inventory.generation == 1 && model.referenceHex != nil,
                "forked query altered parent occurrence state")
            let retained = model.source
            model.referenceName = "cafe\u{301}"
            check(model.referencePage == nil && model.referenceHex == nil && model.nativeSelectionRange == nil
                && model.source === retained, "exact-spelling edit failed to revoke only occurrence authority")
            model.close(); pinned.close()
            wait("parent and child retirement") { driver.log.contains("close:7") && driver.log.contains("close:8") }
            check(driver.log.filter { $0.hasPrefix("open:") }.count == 1, "occurrence navigation/fork reopened a live path")
        }

        // Failed replacements consume generations and preserve the displayed
        // source, while old rows lose activation/copy authority synchronously.
        do {
            let driver = ReferenceModelDriver(), model = opened(driver)
            find(model); let target = model.referencePage!.target(id: 1)!, retained = model.source
            driver.failNext("REFERENCE_INVALID_NAME"); model.findReferences()
            wait("failed query did not finish") { !model.busy }
            check(!model.referencesAreCurrent && model.referenceHex == nil && model.source === retained,
                "failed replacement retained activation authority or replaced source")
            model.openReference(target)
            check(!model.busy && !driver.log.contains("reference:7:1:1"), "failed replacement reactivated old result")
            find(model, "second")
            check(model.referencePage?.inventory.generation == 3 && model.referencePage?.rows.first?.line == 71,
                "failed attempt recycled generation or changed LF lines")
            model.close(); wait("failed-query retirement") { driver.log.contains("close:7") }
        }

        for action in ["edit", "cancel", "replace", "close"] {
            let driver = ReferenceModelDriver(), model = opened(driver), retained = model.source
            model.referenceName = "café"; driver.pause("prepare"); model.findReferences()
            wait("blocked query did not enter") { driver.entered.wait(timeout: .now()) == .success }
            switch action {
            case "edit": model.referenceName = "cafe\u{301}"
            case "cancel": model.cancel()
            case "replace": model.referenceName = "second"; model.findReferences()
            default: model.close()
            }
            driver.gate.signal()
            wait("old occurrence response did not drain") { driver.log.contains("returned-prepare:7:1") && !model.busy }
            if action == "replace" {
                check(model.referencesAreCurrent && model.referencePage?.inventory.generation == 2
                    && model.referencePage?.inventory.name == "second", "late query replaced newest inventory")
            } else {
                check(model.referencePage == nil && model.referenceHex == nil && !model.referencesAreCurrent,
                    "canceled/edited/closed query published late")
            }
            check(action == "close" ? model.source == nil : model.source === retained,
                "query lifecycle replaced or revived displayed source")
            model.close(); wait("query race retirement") { driver.log.contains("close:7") }
        }
        print("Occurrence coordinator, paging, exact copy, query races, and independent fork regressions passed.")
    }
}
