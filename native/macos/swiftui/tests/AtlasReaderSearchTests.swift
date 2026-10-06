// Production retained-file search decoding, generation and UI-state tests.
// Link AtlasReader*, AtlasPagedReaderModel, AtlasSource, AtlasProjectIO and
// the existing search cancellation primitive; the C transport alone is injected.
import Foundation
import Dispatch

private let sourceText = "😀 one 😀"
private let sourceBytes = [UInt8](arrayLiteral: 255, 254) + sourceText.utf16.flatMap { [UInt8($0 & 255), UInt8($0 >> 8)] }
private func hex(_ bytes: some Sequence<UInt8>) -> String { bytes.map { String(format: "%02x", $0) }.joined() }
private func json(_ value: [String: Any]) -> String {
    String(data: try! JSONSerialization.data(withJSONObject: value), encoding: .utf8)!
}
private func header(_ command: String) -> [String: Any] {
    ["schema": "fcb.reader-session/1", "status": "ok", "command": command, "owner": "7",
     "file_id": "1", "source_revision": "1", "captured_bytes": String(sourceBytes.count),
     "encoding": "utf16le", "additional_source_bytes_read": "0", "native_presented": false,
     "path": ["encoding": "unix-bytes", "hex": hex("/project/unicode".utf8), "display": "/project/unicode"],
     "capture_origin": "regular-file-observation-not-atomic"]
}
private func ranges(_ needle: String) -> [(UInt64, UInt64, UInt64, UInt64)] {
    switch needle { case "😀": return [(2, 6, 0, 4), (16, 20, 9, 13)]; case "one": return [(8, 14, 5, 8)]; default: return [] }
}
private func findPacket(_ generation: UInt64 = 1, needle: String = "😀") -> [String: Any] {
    var out = header("find")
    let hits = ranges(needle)
    out["query_generation"] = String(generation); out["needle"] = needle
    out["mode"] = "exact-decoded-literal"; out["state"] = "complete-observed-input"
    out["search_complete"] = true; out["scanned_bytes"] = String(sourceBytes.count)
    out["matches_seen"] = String(hits.count); out["retained_hits"] = String(hits.count)
    out["literal_original_hex"] = hits.first.map { hex(sourceBytes[Int($0.0)..<Int($0.1)]) as Any } ?? NSNull()
    out["unsupported_at"] = NSNull()
    out["hits"] = hits.enumerated().map { index, hit in
        ["hit_index": String(index), "occurrence_id": String(index + 1),
         "original_range": ["start": String(hit.0), "end": String(hit.1)]] as [String: Any]
    }
    return out
}
private func pagePacket(command: String = "window", generation: UInt64 = 1, needle: String = "😀", hit index: Int = 0,
                        offset: UInt64 = 0, bytes: UInt64 = AtlasReaderLimits.pageBytes) -> [String: Any] {
    var out = header(command)
    let end = min(UInt64(sourceBytes.count), offset + bytes)
    out["text_kind"] = "logical-captured-text-not-shaped"
    out["requested_original_range"] = ["start": String(offset), "end": String(end)]
    out["visible_range"] = ["start": String(offset), "end": String(end)]
    out["text"] = sourceText; out["original_hex"] = hex(sourceBytes[Int(offset)..<Int(end)])
    out["range_limited"] = false; out["boundaries_adjusted"] = false; out["has_replacements"] = false
    out["first_physical_line"] = NSNull(); out["next_offset"] = NSNull(); out["selection"] = NSNull()
    if command == "hit" {
        let hit = ranges(needle)[index]
        out["selection"] = ["query_generation": String(generation),
            "original_range": ["start": String(hit.0), "end": String(hit.1)],
            "window_utf8_range": ["start": String(hit.2), "end": String(hit.3)],
            "original_hex": hex(sourceBytes[Int(hit.0)..<Int(hit.1)])]
    }
    return out
}

private final class Transport: @unchecked Sendable {
    private let lock = NSLock()
    private var calls: [String] = []
    private var needle = "😀"
    private var paused = false
    let entered = DispatchSemaphore(value: 0), gate = DispatchSemaphore(value: 0)
    var log: [String] { lock.lock(); defer { lock.unlock() }; return calls }
    func pauseFind() { lock.lock(); paused = true; lock.unlock() }
    private func record(_ text: String) { lock.lock(); calls.append(text); lock.unlock() }
    var source: AtlasReaderTransport {
        AtlasReaderTransport(create: { 7 }, open: { [self] _, _, _ in record("open"); return json(header("info")) },
            window: { [self] _, offset, bytes in record("window"); return json(pagePacket(offset: offset, bytes: bytes)) },
            lines: { _, _, _, _ in nil }, cancel: { [self] _ in record("cancel"); return true },
            close: { [self] _ in precondition(!Thread.isMainThread); record("close"); return true },
            retirementFailed: { fatalError("unexpected retirement failure") })
    }
    var search: AtlasReaderSearchTransport {
        AtlasReaderSearchTransport(find: { [self] _, generation, value, count, bytes in
            precondition(!Thread.isMainThread && count == 4096 && bytes == UInt64(sourceBytes.count))
            lock.lock(); needle = value; let block = paused; paused = false; calls.append("find:\(generation)"); lock.unlock()
            if block { entered.signal(); precondition(gate.wait(timeout: .now() + .seconds(5)) == .success) }
            return json(findPacket(generation, needle: value))
        }, hit: { [self] _, generation, index, context in
            precondition(!Thread.isMainThread && context == AtlasReaderHitTarget.contextBytes)
            lock.lock(); let value = needle; calls.append("hit:\(generation):\(index)"); lock.unlock()
            return json(pagePacket(command: "hit", generation: generation, needle: value, hit: Int(index)))
        })
    }
}

@main struct AtlasReaderSearchTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ value: @autoclosure () -> Bool, _ message: String) {
        checks += 1; precondition(value(), message)
    }
    @MainActor private static func rejects(_ message: String, _ body: () throws -> Void) {
        do { try body(); fatalError(message) } catch { checks += 1 }
    }
    @MainActor private static func wait(_ message: String, _ predicate: () -> Bool) {
        let deadline = Date().addingTimeInterval(5)
        var result = predicate()
        while !result, Date() < deadline {
            _ = RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.005)); result = predicate()
        }
        check(result, message)
    }
    @MainActor static func main() throws {
        let info = try AtlasReaderInfo.decode(json(header("info")), handle: 7, path: "/project/unicode")
        let report = try AtlasReaderFindReport.decode(json(findPacket()), info: info, generation: 1, needle: "😀")
        check(report.complete && report.hits.count == 2 && report.originalHex == "3dd800de", "UTF-16 find evidence")
        check(report.target(at: -1) == nil && report.target(at: 2) == nil, "out-of-range occurrence")
        let target = report.target(at: 1)!
        let page = try AtlasReaderPage.decode(json(pagePacket(command: "hit", hit: 1)), info: info, request: .hit(target))
        check(page.selection?.utf8Start == 9 && page.selection?.utf8End == 13 && page.selection?.target.start == 16,
              "original bytes and decoded offsets conflated")
        let source = AtlasSource(path: "test", text: page.text)
        check(source.utf16Range(byteStart: 9, byteEnd: 13) == NSRange(location: 7, length: 2), "native surrogate-pair coordinates")
        for (key, value) in [("owner", "8"), ("source_revision", "2"), ("query_generation", "01"),
                             ("needle", "one"), ("state", "pending"), ("retained_hits", "1"), ("matches_seen", "1"),
                             ("literal_original_hex", "ff"), ("scanned_bytes", "18446744073709551615")] {
            var bad = findPacket(); bad[key] = value
            rejects("invalid find \(key)") { _ = try AtlasReaderFindReport.decode(json(bad), info: info, generation: 1, needle: "😀") }
        }
        var duplicate = findPacket(); var duplicatedHits = duplicate["hits"] as! [[String: Any]]
        duplicatedHits[1]["occurrence_id"] = "1"; duplicate["hits"] = duplicatedHits
        rejects("duplicate occurrence accepted") { _ = try AtlasReaderFindReport.decode(json(duplicate), info: info, generation: 1, needle: "😀") }
        var oversized = findPacket(); oversized["hits"] = Array(repeating: (findPacket()["hits"] as! [[String: Any]])[0], count: 4097)
        rejects("oversized hit array admitted") { _ = try AtlasReaderFindReport.decode(json(oversized), info: info, generation: 1, needle: "😀") }
        var partial = findPacket(); partial["search_complete"] = false; partial["state"] = "match-limit"; partial["matches_seen"] = "3"
        let incomplete = try AtlasReaderFindReport.decode(json(partial), info: info, generation: 1, needle: "😀")
        check(!incomplete.complete && incomplete.summary.hasPrefix("Partial") && incomplete.hits.count == 2, "partial matches hidden or promoted")
        var noHits = findPacket(needle: "absent"); noHits["state"] = "unsupported-text"; noHits["search_complete"] = false; noHits["unsupported_at"] = "0"
        let unsupported = try AtlasReaderFindReport.decode(json(noHits), info: info, generation: 1, needle: "absent")
        check(!unsupported.complete && unsupported.hits.isEmpty, "unsupported source became exhaustive no-match")
        for invalid in ["", "a\0b", String(repeating: "a", count: 1025)] {
            rejects("invalid find input") { try AtlasReaderFindReport.validateNeedle(invalid) }
        }
        var unicode = findPacket(needle: "é")
        unicode["needle"] = "e\u{301}"
        rejects("query canonical alias accepted") { _ = try AtlasReaderFindReport.decode(json(unicode), info: info, generation: 1, needle: "é") }
        let a = AtlasReaderHitTarget(generation: 1, index: 0, start: 0, end: 2, needle: "é", originalHex: "c3a9")
        let b = AtlasReaderHitTarget(generation: 1, index: 0, start: 0, end: 2, needle: "e\u{301}", originalHex: "c3a9")
        check(a != b, "target equality normalized query bytes")
        for key in ["query_generation", "original_hex", "selection_namespace"] {
            var bad = pagePacket(command: "hit", hit: 1); var selected = bad["selection"] as! [String: Any]
            selected[key] = key == "query_generation" ? "2" : (key == "original_hex" ? "ff" : "outline")
            bad["selection"] = selected
            rejects("foreign hit selection accepted") { _ = try AtlasReaderPage.decode(json(bad), info: info, request: .hit(target)) }
        }
        var otherText = pagePacket(command: "hit", hit: 1); otherText["text"] = "😀 one 😁"
        rejects("changed decoded witness") { _ = try AtlasReaderPage.decode(json(otherText), info: info, request: .hit(target)) }
        var otherBytes = pagePacket(command: "hit", hit: 1); otherBytes["original_hex"] = String(repeating: "00", count: sourceBytes.count)
        rejects("changed original witness") { _ = try AtlasReaderPage.decode(json(otherBytes), info: info, request: .hit(target)) }
        rejects("search result used as plain page") { _ = try AtlasReaderPage.decode(json(pagePacket(command: "hit", hit: 1)), info: info, request: .firstPage) }
        var backwards = pagePacket(offset: 5, bytes: 4)
        backwards["visible_range"] = ["start": "0", "end": "4"]
        backwards["original_hex"] = hex(sourceBytes[0..<4]); backwards["text"] = "x"; backwards["boundaries_adjusted"] = true
        rejects("non-progressing page admitted") { _ = try AtlasReaderPage.decode(json(backwards), info: info, request: .window(offset: 5, bytes: 4)) }

        do {
            let driver = Transport()
            let coordinator = AtlasReaderCoordinator(transport: driver.source, search: driver.search)
            var done = false
            Task {
                do {
                    _ = try await coordinator.open(input: AtlasReaderInput(root: "/project", path: "unicode"))
                    let found = try await coordinator.find("😀")
                    let hit = try await coordinator.read(.hit(found.target(at: 1)!))
                    check(hit.selection?.target.start == 16, "coordinator lost selected occurrence")
                    do { _ = try await coordinator.find(""); fatalError("empty find admitted") } catch AtlasReaderError.invalidInput { }
                    _ = try await coordinator.read(.hit(found.target(at: 0)!))
                    let next = try await coordinator.find("one")
                    check(next.generation == 2, "query identity did not advance")
                    do { _ = try await coordinator.read(.hit(found.target(at: 0)!)); fatalError("old query activated") } catch AtlasReaderError.invalidInput { }
                    coordinator.clearFind()
                    do { _ = try await coordinator.read(.hit(next.target(at: 0)!)); fatalError("cleared query activated") } catch AtlasReaderError.invalidInput { }
                    _ = try await coordinator.read(.firstPage)
                    coordinator.close(); done = true
                } catch { fatalError("coordinator find failed: \(error)") }
            }
            wait("coordinator work did not finish") { done }
            wait("coordinator did not retire") { driver.log.contains("close") }
            check(driver.log.filter { $0 == "open" }.count == 1, "file-local search reopened source")
            check(driver.log.filter { $0.hasPrefix("hit:") }.count == 2, "stale target reached C ABI")
        }
        do {
            let driver = Transport()
            let model = AtlasPagedReaderModel(transport: driver.source, search: driver.search)
            model.open(root: "/project", path: "unicode", accessLease: nil)
            wait("find model open") { !model.busy && model.page != nil }
            model.fileQuery = "😀"; model.find()
            wait("model first occurrence") { !model.busy && model.selectedHitIndex == 0 }
            check(model.nativeSelectionRange == NSRange(location: 0, length: 2) && model.matchHex == "3dd800de", "first native selection/copy")
            model.moveHit(backwards: true)
            wait("model wraparound") { !model.busy && model.selectedHitIndex == 1 }
            check(model.nativeSelectionRange == NSRange(location: 7, length: 2), "wrapped occurrence native range")
            let retained = model.source
            model.fileQuery = "one"
            check(model.nativeSelectionRange == nil && model.matchHex == nil, "query edit borrowed old authority before onChange")
            check(model.findReport == nil && !model.busy, "query edit did not synchronously revoke find context")
            model.clearFileSearch()
            check(model.source === retained && model.findReport == nil, "clearing file query evicted source")
            model.find()
            wait("model replacement query") { !model.busy && model.findReport?.generation == 2 && model.selectedHitIndex == 0 }
            check(model.nativeSelectionRange == NSRange(location: 3, length: 3) && model.matchHex == "6f006e006500", "replacement query witness")
            model.back()
            wait("history after query replacement") { !model.busy && model.nativeSelectionRange == nil }
            check(model.page != nil && model.matchHex == nil, "Back reactivated obsolete query")
            model.close()
            wait("model find close") { driver.log.contains("close") }
        }
        do {
            let driver = Transport()
            let model = AtlasPagedReaderModel(transport: driver.source, search: driver.search)
            model.open(root: "/project", path: "unicode", accessLease: nil)
            wait("cancellation model open") { !model.busy && model.page != nil }
            driver.pauseFind(); model.fileQuery = "😀"; model.find()
            wait("find entered worker") { driver.entered.wait(timeout: .now()) == .success }
            model.fileQuery = "one"; model.clearFileSearch()
            check(!model.busy && model.findReport == nil && model.matchHex == nil, "edited query did not cancel publication")
            model.find(); driver.gate.signal()
            wait("replacement find did not drain") { !model.busy && model.findReport?.needle == "one" && model.selectedHitIndex == 0 }
            check(model.findReport?.generation == 2 && !driver.log.contains("hit:1:0"), "canceled query published or reused generation")
            model.close()
            wait("canceled query capture did not retire") { driver.log.contains("close") }
        }
        print("PASS: \(checks) retained-file find, UTF-16 hit, generation, copy and cancellation checks")
    }
}
