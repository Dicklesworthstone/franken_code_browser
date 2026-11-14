// Deferred native regressions for retained-capture expression Find. These
// injected receipts exercise production admission, generation and navigation;
// Rust remains the only expression parser, matcher and source decoder.
import Foundation
import Dispatch

private let expressionSource = "😀 one 😀"
private let expressionBytes: [UInt8] = [255, 254]
    + expressionSource.utf16.flatMap { [UInt8($0 & 255), UInt8($0 >> 8)] }
private let expressionQuery = "\"😀\" one -missing"
private let expressionOptions = AtlasReaderFindOptions(mode: .expression)
private func expressionHex(_ bytes: some Sequence<UInt8>) -> String {
    bytes.map { String(format: "%02x", $0) }.joined()
}
private func expressionJSON(_ value: [String: Any]) -> String {
    String(data: try! JSONSerialization.data(withJSONObject: value), encoding: .utf8)!
}
private func expressionHeader(_ command: String) -> [String: Any] {
    ["schema": "fcb.reader-session/1", "status": "ok", "command": command, "owner": "7",
     "file_id": "1", "source_revision": "1", "captured_bytes": String(expressionBytes.count),
     "encoding": "utf16le", "additional_source_bytes_read": "0", "native_presented": false,
     "path": ["encoding": "unix-bytes", "hex": expressionHex("/project/unicode".utf8), "display": "/project/unicode"],
     "capture_origin": "regular-file-observation-not-atomic"]
}
/// Explicit source coordinates for the fixture, not a substitute matcher.
private func expressionRanges(_ primary: String) -> [(UInt64, UInt64, UInt64, UInt64)] {
    primary == "😀" ? [(2, 6, 0, 4), (16, 20, 9, 13)] : [(8, 14, 5, 8)]
}
private let expressionKeys = ["requested_mode", "realized_mode", "expression", "primary_needle",
    "query_syntax", "predicate_scope", "expression_qualified"]
private func expressionPacket(generation: UInt64 = 1, query: String = expressionQuery,
                              primary: String = "😀", startAfter: UInt64 = 0,
                              options: AtlasReaderFindOptions = expressionOptions,
                              qualified: Bool? = true) -> [String: Any] {
    let all = qualified == true ? expressionRanges(primary) : []
    let skipped = min(startAfter, UInt64(all.count))
    let selected = Array(all.dropFirst(Int(skipped)).prefix(1))
    var out = expressionHeader("find")
    out["query_generation"] = String(generation); out["needle"] = query
    out["mode"] = "decoded-text-expression"
    out["requested_mode"] = "expression"; out["realized_mode"] = "decoded-text-expression"
    out["expression"] = query; out["primary_needle"] = primary
    out["query_syntax"] = "fcb-bounded-expression/1"; out["predicate_scope"] = "retained-source-capture"
    out["expression_qualified"] = qualified.map { $0 as Any } ?? NSNull()
    out["case_sensitive"] = options.caseSensitive; out["count_all_matches"] = options.countAllMatches
    out["retained_limit"] = "1"; out["page_start_after"] = String(startAfter)
    out["skipped_matches"] = String(skipped); out["has_more_matches"] = UInt64(all.count) - skipped > UInt64(selected.count)
    out["state"] = qualified == nil ? "byte-limit" : "complete-observed-input"
    out["search_complete"] = qualified != nil; out["match_count_complete"] = qualified != nil
    out["scanned_bytes"] = qualified == nil ? "10" : String(expressionBytes.count)
    out["matches_seen"] = String(all.count); out["retained_hits"] = String(selected.count)
    out["rows_truncated"] = all.count > selected.count; out["unsupported_at"] = NSNull()
    out["literal_original_hex"] = options.caseSensitive
        ? (selected.first.map { expressionHex(expressionBytes[Int($0.0)..<Int($0.1)]) as Any } ?? NSNull()) : NSNull()
    out["hits"] = selected.enumerated().map { index, hit in
        var row: [String: Any] = ["hit_index": String(index), "occurrence_id": String(skipped + UInt64(index) + 1),
            "original_range": ["start": String(hit.0), "end": String(hit.1)]]
        if !options.caseSensitive {
            row["matched_text"] = primary == "ONE" ? "one" : primary
            row["original_hex"] = expressionHex(expressionBytes[Int(hit.0)..<Int(hit.1)])
        }
        return row
    }
    return out
}
private func literalExpressionFixture(generation: UInt64 = 1, needle: String = "one",
                                      startAfter: UInt64 = 0,
                                      options: AtlasReaderFindOptions = .init()) -> [String: Any] {
    var packet = expressionPacket(generation: generation, query: needle, primary: needle,
        startAfter: startAfter, options: options)
    for key in expressionKeys { packet.removeValue(forKey: key) }
    packet["mode"] = options.caseSensitive ? "exact-decoded-literal" : "unicode-lowercase-literal"
    if packet["has_more_matches"] as? Bool == true && !options.countAllMatches {
        packet["state"] = "match-limit"; packet["search_complete"] = false; packet["match_count_complete"] = false
    }
    return packet
}
private func expressionPage(generation: UInt64? = nil, primary: String = "😀", index: UInt64 = 0,
                            offset: UInt64 = 0, bytes: UInt64 = AtlasReaderLimits.pageBytes) -> [String: Any] {
    var packet = expressionHeader(generation == nil ? "window" : "hit")
    let end = min(UInt64(expressionBytes.count), offset + bytes)
    packet["text_kind"] = "logical-captured-text-not-shaped"
    packet["requested_original_range"] = ["start": String(offset), "end": String(end)]
    packet["visible_range"] = ["start": String(offset), "end": String(end)]
    packet["text"] = expressionSource
    packet["original_hex"] = expressionHex(expressionBytes[Int(offset)..<Int(end)])
    packet["range_limited"] = false; packet["boundaries_adjusted"] = false; packet["has_replacements"] = false
    packet["first_physical_line"] = NSNull(); packet["next_offset"] = NSNull(); packet["selection"] = NSNull()
    if let generation {
        let hit = expressionRanges(primary)[Int(index)]
        packet["selection"] = ["query_generation": String(generation),
            "original_range": ["start": String(hit.0), "end": String(hit.1)],
            "window_utf8_range": ["start": String(hit.2), "end": String(hit.3)],
            "original_hex": expressionHex(expressionBytes[Int(hit.0)..<Int(hit.1)])]
    }
    return packet
}

private final class ExpressionTransport: @unchecked Sendable {
    private let lock = NSLock()
    private var calls: [String] = []
    private var primary = "😀"
    private var pageStart: UInt64 = 0
    private var paused = false
    private var malformed = false
    let entered = DispatchSemaphore(value: 0), gate = DispatchSemaphore(value: 0)
    var log: [String] { lock.lock(); defer { lock.unlock() }; return calls }
    func pause() { lock.lock(); paused = true; lock.unlock() }
    func reject() { lock.lock(); malformed = true; lock.unlock() }
    private func record(_ value: String) { lock.lock(); calls.append(value); lock.unlock() }
    var source: AtlasReaderTransport {
        .init(create: { 7 }, open: { [self] _, _, _ in record("open"); return expressionJSON(expressionHeader("info")) },
              window: { _, offset, bytes in expressionJSON(expressionPage(offset: offset, bytes: bytes)) },
              lines: { _, _, _, _ in nil }, cancel: { [self] _ in record("cancel"); return true },
              close: { [self] _ in precondition(!Thread.isMainThread); record("close"); return true },
              retirementFailed: { preconditionFailure("unexpected reader retirement failure") })
    }
    func search(literalPages: Bool = true) -> AtlasReaderSearchTransport {
        var transport = AtlasReaderSearchTransport(find: { [self] _, _, _, _, _ in
            record("legacy"); return nil
        }, hit: { [self] _, generation, index, context in
            precondition(!Thread.isMainThread && context == AtlasReaderHitTarget.contextBytes)
            lock.lock(); let value = primary, originalIndex = pageStart + index
            calls.append("hit:\(generation):\(index)"); lock.unlock()
            return expressionJSON(expressionPage(generation: generation, primary: value, index: originalIndex))
        }, findExpressionPage: { [self] _, generation, query, start, count, bytes, flags in
            find(generation: generation, query: query, start: start, count: count, bytes: bytes, flags: flags, expression: true)
        })
        if literalPages {
            transport.findPage = { [self] _, generation, query, start, count, bytes, flags in
                find(generation: generation, query: query, start: start, count: count, bytes: bytes, flags: flags, expression: false)
            }
        }
        return transport
    }
    private func find(generation: UInt64, query: String, start: UInt64, count: UInt64,
                      bytes: UInt64, flags: UInt64, expression: Bool) -> String {
        precondition(!Thread.isMainThread && count == AtlasReaderFindReport.maxHits
            && bytes == UInt64(expressionBytes.count) && flags <= 3)
        let options = AtlasReaderFindOptions(caseSensitive: flags & 1 == 0, countAllMatches: flags & 2 != 0,
            mode: expression ? .expression : .literal)
        // Each query used below has an explicit fixture primary; no native parsing.
        let value = expression ? (query == "\"ONE\" -missing" ? "ONE" : "😀") : query
        lock.lock(); primary = value; pageStart = start
        let block = paused, bad = malformed; paused = false; malformed = false
        calls.append("\(expression ? "expression" : "literal"):\(generation):\(start)")
        calls.append("flags:\(flags)"); lock.unlock()
        if block { entered.signal(); precondition(gate.wait(timeout: .now() + .seconds(5)) == .success) }
        var packet = expression
            ? expressionPacket(generation: generation, query: query, primary: value, startAfter: start, options: options)
            : literalExpressionFixture(generation: generation, needle: query, startAfter: start, options: options)
        if bad { packet["expression_qualified"] = false }
        return expressionJSON(packet)
    }
}

@MainActor enum AtlasReaderExpressionTests {
    private static var checks = 0
    private static func check(_ value: @autoclosure () -> Bool, _ message: String) {
        checks += 1; precondition(value(), message)
    }
    private static func rejects(_ message: String, _ body: () throws -> Void) {
        do { try body(); check(false, message) }
        catch let error as AtlasReaderError {
            switch error {
            case .invalidResponse, .invalidInput, .unavailable: checks += 1
            default: check(false, "unexpected refusal: \(error)")
            }
        } catch { check(false, "unexpected error: \(error)") }
    }
    private static func wait(_ message: String, _ predicate: () -> Bool) {
        let deadline = Date().addingTimeInterval(5)
        var result = predicate()
        while !result, Date() < deadline {
            _ = RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.005))
            result = predicate()
        }
        check(result, message)
    }
    static func run() throws -> Int {
        let info = try AtlasReaderInfo.decode(expressionJSON(expressionHeader("info")), handle: 7, path: "/project/unicode")
        func decode(_ packet: [String: Any], query: String = expressionQuery, generation: UInt64 = 1,
                    options: AtlasReaderFindOptions = expressionOptions, start: UInt64 = 0) throws -> AtlasReaderFindReport {
            try AtlasReaderFindReport.decode(expressionJSON(packet), info: info, generation: generation,
                needle: query, options: options, startAfter: start)
        }
        let report = try decode(expressionPacket())
        check(report.complete && report.matchesSeen == 2 && report.hits.count == 1 && report.hasMoreMatches,
              "expression qualification did not count all primary matches with count flag off")
        check(report.needle == expressionQuery && report.expression?.primaryNeedle == "😀"
            && report.expression?.qualified == true && report.target(at: 0)?.needle == "😀",
            "raw expression became the source witness or lost its predicate receipt")
        check(report.summary.contains("primary-term") && report.summary.contains("Matches 1–1"),
              "expression count did not explain primary-term navigation")
        let second = try decode(expressionPacket(generation: 2, startAfter: 1), generation: 2, start: 1)
        let target = second.target(at: 0)!
        let page = try AtlasReaderPage.decode(expressionJSON(expressionPage(generation: 2, index: 1)),
            info: info, request: .hit(target))
        check(second.complete && !second.hasMoreMatches && target.index == 0 && second.hits[0].occurrence == 2
            && target.start == 16 && target.end == 20 && target.originalHex == "3dd800de",
            "later expression page lost local/global occurrence or UTF-16 bytes")
        let source = AtlasSource(path: "fixture", text: page.text)
        check(page.selection?.utf8Start == 9 && page.selection?.utf8End == 13
            && source.utf16Range(byteStart: 9, byteEnd: 13) == NSRange(location: 7, length: 2),
            "expression hit selected query syntax instead of primary source characters")
        let lowerOptions = AtlasReaderFindOptions(caseSensitive: false, mode: .expression)
        let lower = try decode(expressionPacket(query: "\"ONE\" -missing", primary: "ONE", options: lowerOptions),
            query: "\"ONE\" -missing", options: lowerOptions)
        check(lower.target(at: 0)?.needle == "one" && lower.target(at: 0)?.originalHex == "6f006e006500",
              "case-insensitive expression lost the exact original spelling")
        let excluded = try decode(expressionPacket(qualified: false))
        check(excluded.complete && excluded.hits.isEmpty && excluded.matchesSeen == 0
            && excluded.summary.hasPrefix("No matches satisfy"), "excluded source published primary matches")
        let incomplete = try decode(expressionPacket(qualified: nil))
        check(!incomplete.complete && incomplete.hits.isEmpty && incomplete.matchesSeen == 0
            && incomplete.summary.contains("No matches are available until"),
            "unknown qualification became a complete negative result or navigable primary")
        let beyond = try decode(expressionPacket(startAfter: 2), start: 2)
        check(beyond.complete && beyond.hits.isEmpty && beyond.skippedMatches == 2 && !beyond.hasMoreMatches,
              "empty later expression page lost complete qualification")
        for key in expressionKeys {
            var bad = expressionPacket(); bad.removeValue(forKey: key)
            rejects("expression accepted missing \(key)") { _ = try decode(bad) }
        }
        let badFields: [(String, Any)] = [
            ("mode", "exact-decoded-literal"), ("requested_mode", "text"), ("realized_mode", "exact-decoded-literal"),
            ("expression", "\"😀\" -other"), ("needle", "😀"), ("primary_needle", ""),
            ("primary_needle", String(repeating: "x", count: 1025)), ("primary_needle", "bad\u{0}term"),
            ("query_syntax", "regex/1"), ("predicate_scope", "original-search-observation"),
            ("expression_qualified", false), ("expression_qualified", NSNull()),
            ("expression_revalidated", false), ("state", "match-limit"),
            ("page_start_after", "1"), ("count_all_matches", true), ("case_sensitive", false)
        ]
        for (key, value) in badFields {
            var bad = expressionPacket(); bad[key] = value
            rejects("expression accepted forged \(key)") { _ = try decode(bad) }
        }
        var falsePositive = expressionPacket(qualified: false); falsePositive["expression_qualified"] = true
        rejects("qualified expression with no primary match accepted") { _ = try decode(falsePositive) }
        var incompleteZero = expressionPacket(qualified: nil); incompleteZero["expression_qualified"] = false
        rejects("incomplete capture certified an exclusion") { _ = try decode(incompleteZero) }
        for key in expressionKeys + ["expression_revalidated"] {
            var contaminated = literalExpressionFixture(); contaminated[key] = NSNull()
            rejects("literal receipt borrowed expression authority through \(key)") {
                _ = try decode(contaminated, query: "one", options: .init())
            }
        }
        let literal = try decode(literalExpressionFixture(), query: "one", options: .init())
        check(literal.expression == nil && literal.target(at: 0)?.needle == "one", "literal receipt changed meaning")
        let decomposed = "\"e\u{301}\" -missing", composed = "\"é\" -missing"
        rejects("canonically equivalent raw query replaced expression identity") {
            _ = try decode(expressionPacket(query: decomposed), query: composed)
        }
        try AtlasReaderFindReport.validateNeedle(String(repeating: "x", count: 1024), mode: .expression)
        try AtlasReaderFindReport.validateNeedle(String(repeating: "x", count: 4096))
        for value in ["", "bad\u{0}term", String(repeating: "x", count: 1025)] {
            rejects("invalid or oversized expression admitted") {
                try AtlasReaderFindReport.validateNeedle(value, mode: .expression)
            }
        }
        check(AtlasReaderFindOptions(caseSensitive: false, countAllMatches: true, mode: .expression).flags == 3,
              "expression changed existing case/count flag bits")
        let legacyDriver = ExpressionTransport()
        var legacy = legacyDriver.search(); legacy.findExpressionPage = nil
        rejects("legacy host silently downgraded expression to literal Find") {
            _ = try legacy.performFind(7, 1, expressionQuery, 4096, UInt64(expressionBytes.count), options: expressionOptions)
        }
        check(legacyDriver.log.isEmpty, "unsupported expression reached a literal transport")
        check(AtlasReaderError.engine("READER_SESSION_EXPRESSION_SCOPE_UNSUPPORTED").message.contains("project search")
            && AtlasReaderError.engine("QUERY_REGEX_UNQUALIFIED").message.contains("Regular expressions"),
            "bounded expression refusal lost its actionable explanation")
        navigation()
        cancellation()
        return checks
    }

    private static func navigation() {
        for literalPages in [false, true] {
            let driver = ExpressionTransport()
            let model = AtlasPagedReaderModel(transport: driver.source, search: driver.search(literalPages: literalPages))
            model.open(root: "/project", path: "unicode", accessLease: nil)
            wait("expression reader failed to open") { !model.busy && model.page != nil }
            model.findOptions = expressionOptions
            check(model.canFindExpression && model.canFind && model.canConfigureFind, "expression-only capability was ignored")
            model.fileQuery = expressionQuery; model.find()
            wait("first expression hit not selected") { !model.busy && model.findReport?.generation == 1 && model.selectedHitIndex == 0 }
            check(model.findPositionLabel == "Match 1 of 2" && model.matchHex == "3dd800de"
                && model.nativeSelectionRange == NSRange(location: 0, length: 2), "first expression hit selection or count")
            model.moveHit(backwards: false)
            wait("later expression page not selected") { !model.busy && model.findReport?.generation == 2 && model.selectedHitIndex == 0 }
            check(model.findPositionLabel == "Match 2 of 2" && model.findReport?.pageStartAfter == 1
                && model.nativeSelectionRange == NSRange(location: 7, length: 2), "later expression page used literal transport or coordinates")
            model.moveHit(backwards: true)
            wait("previous expression page not selected") { !model.busy && model.findReport?.generation == 3 && model.selectedHitIndex == 0 }
            model.moveHit(backwards: true)
            wait("known expression total did not wrap to final page") { !model.busy && model.findReport?.generation == 4 && model.selectedHitIndex == 0 }
            check(model.findReport?.pageStartAfter == 1 && model.source != nil,
                  "expression wrap reused the first retained table")
            if literalPages {
                let retained = model.source
                model.findOptions.mode = .literal
                check(model.findReport == nil && model.matchHex == nil && model.nativeSelectionRange == nil
                    && !model.canMoveHit && model.source === retained, "mode edit did not revoke expression actions synchronously")
                model.fileQuery = "one"; model.find()
                wait("literal search after expression mode did not recover") { !model.busy && model.findReport?.generation == 5 && model.selectedHitIndex == 0 }
                check(model.findReport?.expression == nil && model.matchHex == "6f006e006500"
                    && driver.log.contains("literal:5:0"), "literal mode inherited expression policy")
            }
            check(driver.log.filter { $0.hasPrefix("expression:") } == [
                "expression:1:0", "expression:2:1", "expression:3:0", "expression:4:1"
            ] && !driver.log.contains("legacy"), "expression navigation bypassed its additive ABI or reused generations")
            model.close()
            wait("expression reader close") { driver.log.contains("close") }
            check(driver.log.filter { $0 == "open" }.count == 1, "expression replay reopened disk")
        }
        let driver = ExpressionTransport()
        let model = AtlasPagedReaderModel(transport: driver.source, search: driver.search())
        model.open(root: "/project", path: "unicode", accessLease: nil)
        wait("malformed expression reader open") { !model.busy && model.page != nil }
        model.findOptions = expressionOptions; model.fileQuery = expressionQuery; model.find()
        wait("malformed expression first search") { !model.busy && model.selectedHitIndex == 0 }
        let retained = model.source
        driver.reject(); model.moveHit(backwards: false)
        check(model.findReport == nil && model.matchHex == nil && model.source === retained, "new page retained old hit authority")
        wait("malformed expression page did not drain") { !model.busy }
        check(model.findReport == nil && !model.canMoveHit && !driver.log.contains("hit:2:0")
            && model.source === retained, "forged qualification activated a primary match")
        model.find()
        wait("expression did not recover after bad receipt") { !model.busy && model.findReport?.generation == 3 && model.selectedHitIndex == 0 }
        model.close()
        wait("malformed expression reader close") { driver.log.contains("close") }
    }

    private static func cancellation() {
        for changeMode in [false, true] {
            let driver = ExpressionTransport()
            let model = AtlasPagedReaderModel(transport: driver.source, search: driver.search())
            model.open(root: "/project", path: "unicode", accessLease: nil)
            wait("canceled expression reader open") { !model.busy && model.page != nil }
            model.findOptions = expressionOptions; model.fileQuery = expressionQuery; model.find()
            wait("canceled expression first search") { !model.busy && model.selectedHitIndex == 0 }
            let retained = model.source
            driver.pause(); model.moveHit(backwards: false)
            wait("expression page worker did not pause") { driver.entered.wait(timeout: .now()) == .success }
            if changeMode {
                model.findOptions.mode = .literal; model.fileQuery = "one"
            } else { model.fileQuery = "\"😀\" -missing" }
            check(!model.busy && model.findReport == nil && model.matchHex == nil && model.nativeSelectionRange == nil
                && model.source === retained, "query/mode edit retained stale expression authority or evicted source")
            model.find(); driver.gate.signal()
            wait("replacement after canceled expression did not drain") {
                !model.busy && model.findReport?.generation == 3 && model.selectedHitIndex == 0
            }
            check(!driver.log.contains("hit:2:0") && model.findReport?.pageStartAfter == 0
                && model.findReport?.options.mode == (changeMode ? .literal : .expression),
                "late expression page replaced the newer query or reused its generation")
            model.close()
            wait("canceled expression reader close") { driver.log.contains("close") }
        }
    }
}
