// Portable production outline/reader tests; only the foreign C boundary is
// injected. Link AtlasSearch.swift, AtlasSearchCoordinator.swift, AtlasProjectIO,
// AtlasSource, AtlasReader, AtlasReaderSearch, AtlasReaderOutline,
// AtlasReaderCoordinator and AtlasPagedReaderModel. No Apple UI or parser mock.
import Foundation
import Dispatch

private enum Fixture {
    static let text = "fn café() {}\nfn second() {}\n"
    static let raw: [UInt8] = [255, 254] + text.utf16.flatMap { [UInt8($0 & 255), UInt8($0 >> 8)] }
    static func hex(_ bytes: some Sequence<UInt8>) -> String { bytes.map { String(format: "%02x", $0) }.joined() }
    static func json(_ value: [String: Any]) -> String {
        String(data: try! JSONSerialization.data(withJSONObject: value), encoding: .utf8)!
    }
    static func range(_ start: UInt64, _ end: UInt64) -> [String: String] {
        ["start": String(start), "end": String(end)]
    }
    static func name(_ id: UInt64) -> String { id == 2 ? "second" : "café" }
    static func nameBytes(_ id: UInt64) -> (UInt64, UInt64) {
        id == 2 ? (34, 46) : (8, 16)
    }
    static func evidence(_ id: UInt64) -> (UInt64, UInt64) {
        id == 2 ? (28, 58) : (2, 28)
    }
    static func decodedName(_ id: UInt64) -> (UInt64, UInt64) {
        id == 2 ? (17, 23) : (3, 8)
    }
    static func header(_ command: String, owner: UInt64 = 7) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": command,
         "owner": String(owner), "file_id": "1", "source_revision": "1",
         "captured_bytes": String(raw.count), "encoding": "utf16le",
         "additional_source_bytes_read": "0", "native_presented": false,
         "capture_origin": "regular-file-observation-not-atomic",
         "path": ["encoding": "unix-bytes", "hex": hex("/project/lib.rs".utf8), "display": "/project/lib.rs"]]
    }
    static func row(_ id: UInt64, fallback: Bool = false) -> [String: Any] {
        let evidence = evidence(id), span = nameBytes(id)
        return ["symbol_id": String(id), "parent_id": NSNull(), "depth": "0",
            "name": name(id), "kind": "function", "declaration_line": id == 2 ? "2" : "1",
            "evidence_range": range(evidence.0, evidence.1),
            "name_range": fallback ? NSNull() : range(span.0, span.1) as Any]
    }
    static func outline(owner: UInt64 = 7, generation: UInt64 = 1,
        command: String = "outline", request: AtlasOutlinePageRequest = .initial,
        total: UInt64 = 2, fallback: Bool = false, language: String = "rust") -> [String: Any] {
        var out = header(command, owner: owner)
        // Fixed fixture inventories; filtering is deliberately not a parser or
        // a substitute for the production Rust name matcher.
        let ids: [UInt64]
        if request.needle.isEmpty { ids = total == 0 ? [] : Array(1...total) }
        else if request.needle == "second" { ids = [2] }
        else { ids = [] }
        let first = min(Int(request.start), ids.count)
        let end = min(first + Int(request.limit), ids.count)
        out["outline_generation"] = String(generation); out["language"] = language
        out["evidence_level"] = "heuristic-outline-candidate"; out["semantic_complete"] = false
        out["output_limited"] = false; out["no_recognized_declarations"] = total == 0
        out["retained_symbols"] = String(total); out["matched_symbols"] = String(ids.count)
        out["count_basis"] = "retained-candidates"; out["name_case"] = "sensitive"
        out["needle"] = request.needle; out["name_mode"] = request.mode.rawValue
        out["symbols"] = ids[first..<end].map { row($0, fallback: fallback) }
        out["next_offset"] = end < ids.count ? String(end) as Any : NSNull()
        return out
    }
    static func window(owner: UInt64 = 7, command: String = "window", generation: UInt64 = 1,
        symbol id: UInt64 = 1, fallback: Bool = false) -> [String: Any] {
        var out = header(command, owner: owner)
        out["text_kind"] = "logical-captured-text-not-shaped"
        out["requested_original_range"] = range(0, UInt64(raw.count))
        out["visible_range"] = range(0, UInt64(raw.count))
        out["range_limited"] = false; out["boundaries_adjusted"] = false
        out["has_replacements"] = false; out["first_physical_line"] = NSNull()
        out["next_offset"] = NSNull(); out["text"] = text; out["original_hex"] = hex(raw)
        out["selection"] = NSNull()
        if command == "symbol" || command == "hit" {
            let span = fallback ? evidence(id) : nameBytes(id)
            let decoded: (UInt64, UInt64) = fallback ? (id == 2 ? (14, 29) : (0, 14)) : decodedName(id)
            var selected: [String: Any] = ["original_range": range(span.0, span.1),
                "window_utf8_range": range(decoded.0, decoded.1),
                "original_hex": hex(raw[Int(span.0)..<Int(span.1)])]
            if command == "symbol" {
                selected["outline_generation"] = String(generation)
                selected["symbol_id"] = String(id); selected["declaration_line"] = id == 2 ? "2" : "1"
                selected["selection_namespace"] = "outline"
                selected["evidence_level"] = "heuristic-outline-candidate"
                selected["semantic_resolution"] = false
            } else { selected["query_generation"] = String(generation) }
            out["selection"] = selected
        }
        return out
    }
    static func find(owner: UInt64, generation: UInt64) -> String {
        var out = header("find", owner: owner)
        out["query_generation"] = String(generation); out["needle"] = "café"
        out["mode"] = "exact-decoded-literal"; out["state"] = "complete-observed-input"
        out["search_complete"] = true; out["scanned_bytes"] = String(raw.count)
        out["matches_seen"] = "1"; out["retained_hits"] = "1"
        out["literal_original_hex"] = hex(raw[8..<16]); out["unsupported_at"] = NSNull()
        out["hits"] = [["hit_index": "0", "occurrence_id": "1", "original_range": range(8, 16)]]
        return json(out)
    }
}

private final class Driver: @unchecked Sendable {
    private let lock = NSLock()
    private var calls: [String] = []
    private var next: UInt64 = 7
    private var paused: String?
    private var failure: String?
    let total: UInt64
    let fallback: Bool
    let entered = DispatchSemaphore(value: 0), gate = DispatchSemaphore(value: 0)
    init(total: UInt64 = 2, fallback: Bool = false) { self.total = total; self.fallback = fallback }
    var log: [String] { lock.lock(); defer { lock.unlock() }; return calls }
    func pause(_ command: String) { lock.lock(); paused = command; lock.unlock() }
    func failNext(_ code: String) { lock.lock(); failure = code; lock.unlock() }
    private func record(_ name: String) { lock.lock(); calls.append(name); lock.unlock() }
    private func step(_ command: String, owner: UInt64) -> String? {
        precondition(!Thread.isMainThread)
        lock.lock(); let wait = paused == command; if wait { paused = nil }
        let fail = failure; failure = nil; lock.unlock()
        if wait { entered.signal(); precondition(gate.wait(timeout: .now() + .seconds(5)) == .success) }
        return fail.map { Fixture.json(["schema": "fcb.reader-session/1", "owner": String(owner),
            "status": "error", "error": ["code": $0]]) }
    }
    var source: AtlasReaderTransport {
        AtlasReaderTransport(create: { [self] in
            lock.lock(); defer { lock.unlock() }; let id = next; next += 1; return id
        }, open: { [self] owner, _, _ in
            record("open:\(owner)"); return Fixture.json(Fixture.header("info", owner: owner))
        }, window: { [self] owner, _, _ in
            record("window:\(owner)"); return Fixture.json(Fixture.window(owner: owner))
        }, lines: { _, _, _, _ in nil }, cancel: { [self] owner in record("cancel:\(owner)"); return true },
        close: { [self] owner in precondition(!Thread.isMainThread); record("close:\(owner)"); return true },
        retirementFailed: { fatalError("retirement failed") })
    }
    var outline: AtlasReaderOutlineTransport {
        AtlasReaderOutlineTransport(prepare: { [self] owner, generation, language, items in
            precondition(items == 4096); record("prepare:\(generation)")
            if let failed = step("prepare", owner: owner) { return failed }
            return Fixture.json(Fixture.outline(owner: owner, generation: generation, total: total,
                fallback: fallback, language: language ?? "rust"))
        }, symbols: { [self] owner, generation, needle, mode, start, limit in
            record("symbols:\(generation):\(needle):\(start)")
            if let failed = step("symbols", owner: owner) { return failed }
            let mode: AtlasOutlineNameMode = mode == 0 ? .exact : (mode == 1 ? .prefix : .contains)
            return Fixture.json(Fixture.outline(owner: owner, generation: generation, command: "symbols",
                request: .init(needle: needle, mode: mode, start: start, limit: limit), total: total, fallback: fallback))
        }, symbol: { [self] owner, generation, id, context in
            precondition(context == 2048); record("symbol:\(generation):\(id)")
            if let failed = step("symbol", owner: owner) { return failed }
            return Fixture.json(Fixture.window(owner: owner, command: "symbol", generation: generation,
                symbol: id, fallback: fallback))
        })
    }
    var search: AtlasReaderSearchTransport {
        AtlasReaderSearchTransport(find: { [self] owner, generation, _, _, _ in
            record("find:\(generation)"); return Fixture.find(owner: owner, generation: generation)
        }, hit: { [self] owner, generation, _, _ in
            record("hit:\(generation)"); return Fixture.json(Fixture.window(owner: owner, command: "hit", generation: generation))
        })
    }
}

@main struct AtlasReaderOutlineTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ condition: @autoclosure () -> Bool, _ reason: String) {
        checks += 1; precondition(condition(), reason)
    }
    @MainActor private static func rejects(_ reason: String, _ body: () throws -> Void) {
        do { try body(); fatalError(reason) } catch { checks += 1 }
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
    @MainActor static func main() throws {
        check(Fixture.raw.count == 58 && Fixture.text.utf8.count == 29, "fixture byte domains")
        let input = try AtlasReaderInput(root: "/project", path: "lib.rs")
        let info = try AtlasReaderInfo.decode(Fixture.json(Fixture.header("info")), handle: 7, path: input.fullPath)
        func decode(_ object: [String: Any]) throws -> AtlasOutlinePage {
            try AtlasOutlinePage.decode(Fixture.json(object), info: info, generation: 1, request: .initial, preparing: true)
        }
        let outline = try decode(Fixture.outline())
        check(outline.rows.count == 2 && outline.inventory.generation == 1, "outline admission")
        check(outline.target(id: 0) == nil && outline.target(id: 3) == nil, "row offset used as symbol ID")
        check(outline.rows[0].name == "café" && outline.rows[0].nameRange == AtlasOutlineRange(start: 8, end: 16), "UTF-16 identity span")
        let target = outline.target(id: 1)!
        let selected = try AtlasReaderPage.decode(Fixture.json(Fixture.window(command: "symbol")), info: info, request: .symbol(target))
        check(selected.selection == nil && selected.symbolSelection?.target == target, "symbol/search namespaces conflated")
        let logical = AtlasSource(path: "label", text: selected.text)
        check(logical.utf16Range(byteStart: selected.symbolSelection!.utf8Start, byteEnd: selected.symbolSelection!.utf8End)
            == NSRange(location: 3, length: 4), "UTF-16 original bytes used as native offsets")
        check(selected.symbolSelection?.originalHex == "630061006600e900", "original symbol bytes lost")
        var parented = Fixture.outline(); var parentRows = parented["symbols"] as! [[String: Any]]
        parentRows[1]["parent_id"] = "1"; parentRows[1]["depth"] = "1"; parented["symbols"] = parentRows
        let nested = try decode(parented)
        check(nested.rows[1].parent == 1 && nested.rows[1].depth == 1, "hierarchy discarded")
        let fallback = try decode(Fixture.outline(fallback: true))
        let fallbackPage = try AtlasReaderPage.decode(Fixture.json(Fixture.window(command: "symbol", fallback: true)),
            info: info, request: .symbol(fallback.target(id: 1)!))
        check(fallbackPage.symbolSelection?.target.symbol.nameRange == nil
            && fallbackPage.symbolSelection?.originalHex == Fixture.hex(Fixture.raw[2..<28]), "declaration fallback not disclosed")

        for (key, value) in [("owner", "8"), ("source_revision", "2"), ("outline_generation", "01"),
            ("language", "markdown"), ("evidence_level", "compiler-proven"), ("count_basis", "all-symbols"),
            ("name_case", "insensitive"), ("needle", "other"), ("name_mode", "prefix"),
            ("retained_symbols", "4097"), ("matched_symbols", "3"), ("next_offset", "2"), ("command", "find")] {
            var bad = Fixture.outline(); bad[key] = value
            rejects("invalid outline header: \(key)") { _ = try decode(bad) }
        }
        var semantic = Fixture.outline(); semantic["semantic_complete"] = true
        rejects("heuristic became semantic inventory") { _ = try decode(semantic) }
        var noDeclarations = Fixture.outline(); noDeclarations["no_recognized_declarations"] = true
        rejects("nonempty fallback inventory") { _ = try decode(noDeclarations) }
        for (key, value) in [("symbol_id", "0"), ("symbol_id", "3"), ("parent_id", "1"),
            ("depth", "1"), ("declaration_line", "0"), ("declaration_line", "4097"), ("name", ""), ("kind", "")] {
            var bad = Fixture.outline(); var rows = bad["symbols"] as! [[String: Any]]
            rows[0][key] = value; bad["symbols"] = rows
            rejects("invalid outline row: \(key)") { _ = try decode(bad) }
        }
        var duplicate = Fixture.outline(); duplicate["symbols"] = [Fixture.row(1), Fixture.row(1)]
        rejects("duplicate symbol ID admitted") { _ = try decode(duplicate) }
        for span in [Fixture.range(1, 9), Fixture.range(9, 8), Fixture.range(8, 59)] {
            var bad = Fixture.outline(); var rows = bad["symbols"] as! [[String: Any]]
            rows[0]["name_range"] = span; bad["symbols"] = rows
            rejects("unbounded or detached symbol span") { _ = try decode(bad) }
        }
        var overArray = Fixture.outline(total: 130); overArray["symbols"] = Array(repeating: Fixture.row(1), count: 129)
        rejects("oversized symbol page allocated") { _ = try decode(overArray) }
        var limited = Fixture.outline(); limited["output_limited"] = true
        let partial = try decode(limited)
        check(partial.summary.contains("Partial") && partial.rows.count == 2, "partial outline hidden/promoted")
        let empty = try decode(Fixture.outline(total: 0))
        check(empty.rows.isEmpty && empty.summary.contains("not proof"), "no recognized symbols became exhaustive no-symbol claim")
        let large = try decode(Fixture.outline(total: 130))
        check(large.rows.count == 64 && large.nextOffset == 64, "initial outline page bound")
        let tailRequest = AtlasOutlinePageRequest(needle: "", mode: .contains, start: 64, limit: 128)
        let tail = try AtlasOutlinePage.decode(Fixture.json(Fixture.outline(command: "symbols", request: tailRequest, total: 130)),
            info: info, generation: 1, request: tailRequest, inventory: large.inventory)
        check(tail.rows.first?.id == 65 && tail.rows.count == 66 && tail.nextOffset == nil, "filtered offsets confused with IDs")
        var missingNext = Fixture.outline(total: 130); missingNext["next_offset"] = NSNull()
        rejects("stranded pagination admitted") { _ = try decode(missingNext) }
        var short = Fixture.outline(); short["symbols"] = [Fixture.row(1)]
        rejects("short page silently accepted") { _ = try decode(short) }
        let filter = AtlasOutlinePageRequest(needle: "second", mode: .prefix, start: 0, limit: 128)
        let filtered = try AtlasOutlinePage.decode(Fixture.json(Fixture.outline(command: "symbols", request: filter)),
            info: info, generation: 1, request: filter, inventory: outline.inventory)
        check(filtered.rows.count == 1 && filtered.rows[0].id == 2 && filtered.inventory.generation == 1, "filter rebuilt or reindexed symbols")
        rejects("changed retained inventory between pages") {
            _ = try AtlasOutlinePage.decode(Fixture.json(Fixture.outline(command: "symbols", request: filter, total: 3)),
                info: info, generation: 1, request: filter, inventory: outline.inventory)
        }
        let composed = AtlasOutlinePageRequest(needle: "é", mode: .exact, start: 0, limit: 128)
        let decomposed = AtlasOutlinePageRequest(needle: "e\u{301}", mode: .exact, start: 0, limit: 128)
        check(composed != decomposed, "filter identity used canonical String equality")
        rejects("canonical filter alias admitted") {
            _ = try AtlasOutlinePage.decode(Fixture.json(Fixture.outline(command: "symbols", request: decomposed)),
                info: info, generation: 1, request: composed, inventory: outline.inventory)
        }
        for request in [AtlasOutlinePageRequest(needle: "a\0b", mode: .contains, start: 0, limit: 128),
            .init(needle: String(repeating: "a", count: 257), mode: .exact, start: 0, limit: 128),
            .init(needle: "", mode: .exact, start: UInt64.max, limit: 128),
            .init(needle: "", mode: .exact, start: 0, limit: 0),
            .init(needle: "", mode: .exact, start: 0, limit: 129)] {
            rejects("invalid filter limit") { try request.validate() }
        }
        for (key, value) in [("outline_generation", "2"), ("symbol_id", "2"), ("declaration_line", "2"),
            ("selection_namespace", "document"), ("query_generation", "1"), ("document_generation", "1"),
            ("evidence_level", "compiler-proven"), ("original_hex", "0000000000000000")] {
            var bad = Fixture.window(command: "symbol"); var selected = bad["selection"] as! [String: Any]
            selected[key] = value; bad["selection"] = selected
            rejects("foreign symbol selection: \(key)") {
                _ = try AtlasReaderPage.decode(Fixture.json(bad), info: info, request: .symbol(target))
            }
        }
        var semanticSelection = Fixture.window(command: "symbol"); var selection = semanticSelection["selection"] as! [String: Any]
        selection["semantic_resolution"] = true; semanticSelection["selection"] = selection
        rejects("symbol selection claimed semantic resolution") {
            _ = try AtlasReaderPage.decode(Fixture.json(semanticSelection), info: info, request: .symbol(target))
        }
        var changedText = Fixture.window(command: "symbol"); changedText["text"] = Fixture.text.replacingOccurrences(of: "café", with: "cafe")
        rejects("changed decoded identifier accepted") {
            _ = try AtlasReaderPage.decode(Fixture.json(changedText), info: info, request: .symbol(target))
        }
        var changedBytes = Fixture.window(command: "symbol"); changedBytes["original_hex"] = String(repeating: "00", count: Fixture.raw.count)
        rejects("changed original identifier accepted") {
            _ = try AtlasReaderPage.decode(Fixture.json(changedBytes), info: info, request: .symbol(target))
        }
        var scalar = Fixture.window(command: "symbol", fallback: true); var scalarSelection = scalar["selection"] as! [String: Any]
        scalarSelection["window_utf8_range"] = Fixture.range(0, 7); scalar["selection"] = scalarSelection
        rejects("split UTF-8 selection admitted") {
            _ = try AtlasReaderPage.decode(Fixture.json(scalar), info: info, request: .symbol(fallback.target(id: 1)!))
        }
        rejects("symbol authority borrowed by plain page") {
            _ = try AtlasReaderPage.decode(Fixture.json(Fixture.window(command: "symbol")), info: info, request: .firstPage)
        }
        rejects("search namespace borrowed by symbol") {
            _ = try AtlasReaderPage.decode(Fixture.json(Fixture.window(command: "hit")), info: info, request: .symbol(target))
        }

        // Real coordinator composition over one handle: distinct generations,
        // stable symbol IDs across filters, and no source reopens for analysis.
        do {
            let driver = Driver()
            let coordinator = AtlasReaderCoordinator(transport: driver.source, search: driver.search, outline: driver.outline)
            var done = false
            Task {
                do {
                    _ = try await coordinator.open(input: input)
                    let found = try await coordinator.find("café")
                    let original = try await coordinator.prepareOutline()
                    check(found.generation == original.inventory.generation, "namespace test must use equal counters")
                    _ = try await coordinator.read(.hit(found.target(at: 0)!))
                    _ = try await coordinator.read(.symbol(original.target(id: 1)!))
                    let filtered = try await coordinator.outlineSymbols(filter)
                    do { _ = try await coordinator.read(.symbol(original.target(id: 1)!)); fatalError("old page target reached engine") }
                    catch AtlasReaderError.invalidInput { checks += 1 }
                    _ = try await coordinator.read(.symbol(filtered.target(id: 2)!))
                    let rebuilt = try await coordinator.prepareOutline()
                    check(rebuilt.inventory.generation == 2, "outline generation reuse")
                    do { _ = try await coordinator.read(.symbol(filtered.target(id: 2)!)); fatalError("old generation reached engine") }
                    catch AtlasReaderError.invalidInput { checks += 1 }
                    _ = try await coordinator.read(.hit(found.target(at: 0)!))
                    coordinator.resetOutline()
                    _ = try await coordinator.read(.firstPage)
                    coordinator.close()
                    _ = try await coordinator.open(input: input)
                    let reopened = try await coordinator.prepareOutline()
                    check(reopened.inventory.identity.owner == 8 && reopened.inventory.generation == 1, "fresh session identity")
                    do { _ = try await coordinator.read(.symbol(original.target(id: 1)!)); fatalError("old session target reached engine") }
                    catch AtlasReaderError.invalidInput { checks += 1 }
                    coordinator.close(); done = true
                } catch { fatalError("coordinator outline workflow: \(error)") }
            }
            wait("coordinator workflow did not finish") { done }
            wait("coordinator handles did not retire") { driver.log.filter { $0.hasPrefix("close:") }.count == 2 }
            check(driver.log.filter { $0.hasPrefix("symbol:") }.count == 2, "revoked target crossed C boundary")
            check(driver.log.contains("symbol:1:2"), "filtered row zero became symbol one")
            check(driver.log.filter { $0.hasPrefix("open:") }.count == 2, "analysis reopened files")
        }

        // Authority can be revoked while a foreign selection is draining,
        // independently of cooperative cancellation. Delivery must recheck it.
        do {
            let driver = Driver()
            let coordinator = AtlasReaderCoordinator(transport: driver.source, outline: driver.outline)
            var page: AtlasOutlinePage?
            Task {
                _ = try! await coordinator.open(input: input)
                page = try! await coordinator.prepareOutline()
            }
            wait("revocation setup") { page != nil }
            driver.pause("symbol")
            var refused = false
            let selected = page!.target(id: 1)!
            Task {
                do { _ = try await coordinator.read(.symbol(selected)); fatalError("revoked selection delivered") }
                catch AtlasReaderError.invalidInput { refused = true }
                catch { fatalError("wrong revocation failure: \(error)") }
            }
            wait("revoked selection entered") { driver.entered.wait(timeout: .now()) == .success }
            coordinator.revokeOutlinePage()
            driver.gate.signal()
            wait("revoked selection was published") { refused }
            coordinator.close()
            wait("revocation retirement") { driver.log.contains("close:7") }
        }

        // Observable model: user-visible paging and independent copy/selection
        // authority. A filter edit cannot replace source or preserve stale rows.
        do {
            let driver = Driver(total: 130)
            let model = AtlasPagedReaderModel(transport: driver.source, search: driver.search, outline: driver.outline)
            model.open(root: "/project", path: "lib.rs", accessLease: nil)
            wait("model source open") { !model.busy && model.page != nil }
            model.buildOutline()
            wait("model outline prepare") { !model.busy && model.outlineIsCurrent }
            let old = model.outlinePage!.target(id: 1)!
            model.openSymbol(old)
            wait("model source symbol") { !model.busy && model.symbolHex != nil }
            check(model.nativeSelectionRange == NSRange(location: 3, length: 4), "model exact name selection")
            model.fileQuery = "not submitted"
            check(model.symbolHex != nil && model.nativeSelectionRange != nil, "search edit revoked independent outline selection")
            model.nextOutlinePage()
            wait("model next outline page") { !model.busy && model.outlinePage?.request.start == 64 }
            check(model.symbolHex == nil && model.canOutlineBack, "new page retained old row authority")
            model.previousOutlinePage()
            wait("model back outline page") { !model.busy && model.outlinePage?.request.start == 0 }
            check(model.outlinePage?.rows.count == 64 && !model.canOutlineBack, "initial 64-row page descriptor lost")
            model.openSymbol(old)
            check(!model.busy && model.symbolHex == nil, "old UI event rebound to new page token")
            let retained = model.source
            model.outlineNeedle = "second"
            check(model.outlinePage == nil && model.symbolHex == nil && model.source === retained, "filter edit failed to revoke rows safely")
            model.filterOutline()
            wait("model filtered rows") { !model.busy && model.outlineIsCurrent }
            check(model.outlinePage?.rows.first?.id == 2 && model.outlinePage?.rows.count == 1, "filter did not reuse candidate IDs")
            model.openSymbol(model.outlinePage!.target(id: 2)!)
            wait("model second symbol") { !model.busy && model.symbolHex != nil }
            check(model.nativeSelectionRange == NSRange(location: 16, length: 6), "second name mapped to wrong native offsets")
            model.fileQuery = "café"; model.find()
            wait("model exact search after outline") { !model.busy && model.matchHex != nil }
            model.outlineNeedle = "other"
            check(model.matchHex != nil && model.nativeSelectionRange != nil, "outline edit revoked independent literal selection")
            model.outlineLanguage = .python
            check(!model.hasOutline && model.matchHex != nil, "language change destroyed retained search")
            model.back()
            wait("model source history") { !model.busy && model.nativeSelectionRange == nil }
            check(model.symbolHex == nil && model.matchHex == nil, "history reauthorized an old symbol or hit")
            model.close()
            wait("model retirement") { driver.log.contains("close:7") }
            check(driver.log.filter { $0.hasPrefix("prepare:") }.count == 1, "filter/paging re-extracted source")
        }
        do {
            let driver = Driver(fallback: true)
            let model = AtlasPagedReaderModel(transport: driver.source, outline: driver.outline)
            model.open(root: "/project", path: "lib.rs", accessLease: nil)
            wait("fallback model source") { !model.busy && model.page != nil }
            model.buildOutline()
            wait("fallback model outline") { !model.busy && model.outlineIsCurrent }
            model.openSymbol(model.outlinePage!.target(id: 1)!)
            wait("fallback model selection") { !model.busy && model.symbolHex != nil }
            check(model.symbolSelectionLabel?.contains("no exact name span") == true, "fallback presented as exact identifier")
            model.close(); wait("fallback retirement") { driver.log.contains("close:7") }
        }

        // Cancellation/coalescing and failure keep source data usable. Edits
        // synchronously invalidate old tokens before blocked workers finish.
        do {
            let driver = Driver()
            let model = AtlasPagedReaderModel(transport: driver.source, outline: driver.outline)
            model.open(root: "/project", path: "lib.rs", accessLease: nil)
            wait("cancellation model source") { !model.busy && model.page != nil }
            let retained = model.source
            driver.failNext("SYMBOL_SOURCE_LIMIT"); model.buildOutline()
            wait("extractor limit terminal state") { !model.busy }
            check(!model.hasOutline && model.source === retained && model.notice.contains("SYMBOL_SOURCE_LIMIT"), "extractor refusal hid source")
            driver.pause("prepare"); model.buildOutline()
            wait("prepare entered") { driver.entered.wait(timeout: .now()) == .success }
            model.outlineLanguage = .python
            check(!model.busy && !model.hasOutline && model.source === retained, "language edit did not cancel extraction")
            model.outlineLanguage = .automatic; model.buildOutline(); driver.gate.signal()
            wait("replacement extraction") { !model.busy && model.outlineIsCurrent }
            check(model.outlinePage?.inventory.generation == 3, "canceled extraction reused generation")
            driver.pause("symbols"); model.outlineNeedle = "absent"; model.filterOutline()
            wait("filter entered") { driver.entered.wait(timeout: .now()) == .success }
            model.outlineNeedle = "second"; model.filterOutline(); driver.gate.signal()
            wait("newest filter accepted") { !model.busy && model.outlineIsCurrent }
            check(model.outlinePage?.request.needle == "second" && model.outlinePage?.rows.first?.id == 2, "old filter published late")
            let target = model.outlinePage!.target(id: 2)!
            driver.pause("symbol"); model.openSymbol(target)
            wait("symbol entered") { driver.entered.wait(timeout: .now()) == .success }
            model.outlineNeedle = ""; driver.gate.signal()
            check(!model.busy && model.symbolHex == nil && model.source === retained, "edited outline kept in-flight selection")
            model.close()
            wait("in-flight outline retirement") { driver.log.contains("close:7") }
            check(model.source == nil && model.outlinePage == nil, "closed model republished late outline")
        }
        print("PASS: \(checks) outline decoding, paging, source selection, identity, cancellation and model checks")
    }
}
