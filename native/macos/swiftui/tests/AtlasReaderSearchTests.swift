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
private func configuredPacket(_ generation: UInt64 = 1, needle: String = "😀",
                              options: AtlasReaderFindOptions, retainedLimit: UInt64 = 4096) -> [String: Any] {
    if options == .init() { return findPacket(generation, needle: needle) }
    // Fixture-only original spelling; the production host owns matching.
    let original = !options.caseSensitive && needle == "ONE" ? "one" : needle
    var out = findPacket(generation, needle: original)
    var rows = Array((out["hits"] as! [[String: Any]]).prefix(Int(retainedLimit)))
    out["needle"] = needle
    out["mode"] = options.caseSensitive ? "exact-decoded-literal" : "unicode-lowercase-literal"
    out["case_sensitive"] = options.caseSensitive; out["count_all_matches"] = options.countAllMatches
    out["retained_limit"] = String(retainedLimit); out["retained_hits"] = String(rows.count)
    out["rows_truncated"] = ranges(original).count > rows.count; out["match_count_complete"] = true
    if !options.caseSensitive {
        out["literal_original_hex"] = NSNull()
        for index in rows.indices {
            let hit = ranges(original)[index]
            rows[index]["matched_text"] = original
            rows[index]["original_hex"] = hex(sourceBytes[Int(hit.0)..<Int(hit.1)])
        }
    } else if rows.isEmpty { out["literal_original_hex"] = NSNull() }
    out["hits"] = rows
    return out
}

/// Long query witnesses can reduce the host's retained row allowance. This
/// one-row fixture makes the same page boundary visible in a tiny source.
/// The source stays fixed; only retained occurrence rows and their local indexes
/// change. No matching logic is substituted for the production search engine.
private func pagedFindPacket(_ generation: UInt64 = 1, needle: String = "😀",
                             startAfter: UInt64 = 0, options: AtlasReaderFindOptions = .init(),
                             partial: Bool = false) -> [String: Any] {
    let original = !options.caseSensitive && needle == "ONE" ? "one" : needle
    let all = ranges(original)
    let available = partial ? Array(all.prefix(1)) : all
    let start = Int(min(startAfter, UInt64(available.count)))
    let selected = Array(available.dropFirst(start).prefix(1))
    let later = available.count > start + selected.count
    let seen = options.countAllMatches ? available.count : start + selected.count + (later ? 1 : 0)
    let skipped = min(startAfter, UInt64(seen))
    let complete = !partial && (options.countAllMatches || !later)
    var out = header("find")
    out["query_generation"] = String(generation); out["needle"] = needle
    out["mode"] = options.caseSensitive ? "exact-decoded-literal" : "unicode-lowercase-literal"
    out["case_sensitive"] = options.caseSensitive; out["count_all_matches"] = options.countAllMatches
    out["retained_limit"] = "1"; out["page_start_after"] = String(startAfter)
    out["skipped_matches"] = String(skipped); out["has_more_matches"] = UInt64(seen) - skipped > UInt64(selected.count)
    out["state"] = partial ? "byte-limit" : (complete ? "complete-observed-input" : "match-limit")
    out["search_complete"] = complete; out["match_count_complete"] = complete
    out["scanned_bytes"] = partial ? String(available.last?.1 ?? 0) : String(sourceBytes.count)
    out["matches_seen"] = String(seen); out["retained_hits"] = String(selected.count)
    out["rows_truncated"] = seen > selected.count; out["unsupported_at"] = NSNull()
    out["literal_original_hex"] = options.caseSensitive
        ? (selected.first.map { hex(sourceBytes[Int($0.0)..<Int($0.1)]) as Any } ?? NSNull()) : NSNull()
    out["hits"] = selected.enumerated().map { index, hit in
        var row: [String: Any] = ["hit_index": String(index), "occurrence_id": String(start + index + 1),
            "original_range": ["start": String(hit.0), "end": String(hit.1)]]
        if !options.caseSensitive {
            row["matched_text"] = original
            row["original_hex"] = hex(sourceBytes[Int(hit.0)..<Int(hit.1)])
        }
        return row
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
    private var pageStart: UInt64 = 0
    private var malformedPage = false
    private var partialPage = false
    let entered = DispatchSemaphore(value: 0), gate = DispatchSemaphore(value: 0)
    var log: [String] { lock.lock(); defer { lock.unlock() }; return calls }
    func pauseFind() { lock.lock(); paused = true; lock.unlock() }
    func rejectNextPage() { lock.lock(); malformedPage = true; lock.unlock() }
    func limitNextPage() { lock.lock(); partialPage = true; lock.unlock() }
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
    var configurableSearch: AtlasReaderSearchTransport {
        let legacy = search
        return AtlasReaderSearchTransport(find: legacy.find, hit: legacy.hit,
            findWithOptions: { [self] _, generation, value, count, bytes, flags in
                precondition(!Thread.isMainThread && count == 4096 && bytes == UInt64(sourceBytes.count))
                precondition(flags <= 3)
                let options = AtlasReaderFindOptions(caseSensitive: flags & 1 == 0, countAllMatches: flags & 2 != 0)
                lock.lock(); needle = !options.caseSensitive && value == "ONE" ? "one" : value
                let block = paused; paused = false
                calls.append("find:\(generation)"); calls.append("flags:\(flags)"); lock.unlock()
                if block { entered.signal(); precondition(gate.wait(timeout: .now() + .seconds(5)) == .success) }
                return json(configuredPacket(generation, needle: value, options: options))
            })
    }

    var pagingSearch: AtlasReaderSearchTransport {
        let legacy = search
        return AtlasReaderSearchTransport(find: legacy.find, hit: { [self] _, generation, index, context in
            precondition(!Thread.isMainThread && context == AtlasReaderHitTarget.contextBytes)
            lock.lock(); let value = needle, originalIndex = pageStart + index
            calls.append("hit:\(generation):\(index)")
            calls.append("global-hit:\(generation):\(originalIndex)")
            lock.unlock()
            return json(pagePacket(command: "hit", generation: generation, needle: value, hit: Int(originalIndex)))
        }, findPage: { [self] _, generation, value, startAfter, count, bytes, flags in
            precondition(!Thread.isMainThread && count == AtlasReaderFindReport.maxHits && bytes == UInt64(sourceBytes.count))
            precondition(flags <= 3)
            let options = AtlasReaderFindOptions(caseSensitive: flags & 1 == 0, countAllMatches: flags & 2 != 0)
            lock.lock()
            needle = !options.caseSensitive && value == "ONE" ? "one" : value; pageStart = startAfter
            let block = paused, malformed = malformedPage, partial = partialPage
            paused = false; malformedPage = false; partialPage = false
            calls.append("page:\(generation):\(startAfter)"); calls.append("page-flags:\(flags)")
            lock.unlock()
            if block { entered.signal(); precondition(gate.wait(timeout: .now() + .seconds(5)) == .success) }
            var packet = pagedFindPacket(generation, needle: value, startAfter: startAfter, options: options, partial: partial)
            if malformed { packet["page_start_after"] = String(startAfter + 1) }
            record("page-return:\(generation)")
            return json(packet)
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

    @MainActor private static func pagingReceipts(info: AtlasReaderInfo) throws {
        let first = try AtlasReaderFindReport.decode(json(pagedFindPacket()), info: info,
            generation: 1, needle: "😀", startAfter: 0)
        check(first.pageStartAfter == 0 && first.skippedMatches == 0 && first.retainedLimit == 1,
              "first page lost its declared cursor or effective retained allowance")
        check(!first.complete && first.hasMoreMatches && first.matchesSeen == 2 && first.hits.count == 1,
              "page truncation became complete coverage or lost its known later occurrence")
        check(first.hits[0].occurrence == 1 && first.hits[0].index == 0
            && first.summary.contains("Matches 1–1"), "first global occurrence mislabeled")
        let secondPacket = pagedFindPacket(2, startAfter: 1)
        let second = try AtlasReaderFindReport.decode(json(secondPacket), info: info,
            generation: 2, needle: "😀", startAfter: 1)
        check(second.complete && !second.hasMoreMatches && second.skippedMatches == 1
            && second.matchesSeen == 2 && second.hits[0].occurrence == 2
            && second.hits[0].index == 0, "second page confused local and global occurrence indexes")
        let target = second.target(at: 0)!
        let page = try AtlasReaderPage.decode(json(pagePacket(command: "hit", generation: 2, hit: 1)),
            info: info, request: .hit(target))
        let source = AtlasSource(path: "test", text: page.text)
        check(target.start == 16 && target.end == 20 && target.originalHex == "3dd800de"
            && page.selection?.utf8Start == 9 && page.selection?.utf8End == 13
            && source.utf16Range(byteStart: 9, byteEnd: 13) == NSRange(location: 7, length: 2),
            "later-page source selection/copy borrowed first-page coordinates")
        let counting = AtlasReaderFindOptions(caseSensitive: true, countAllMatches: true)
        let counted = try AtlasReaderFindReport.decode(json(pagedFindPacket(options: counting)), info: info,
            generation: 1, needle: "😀", options: counting, startAfter: 0)
        check(counted.complete && counted.matchesSeen == 2 && counted.hasMoreMatches && counted.hits.count == 1,
              "complete count suppressed navigation to omitted rows")
        let lowercase = AtlasReaderFindOptions(caseSensitive: false, countAllMatches: false)
        let lower = try AtlasReaderFindReport.decode(json(pagedFindPacket(needle: "ONE", options: lowercase)),
            info: info, generation: 1, needle: "ONE", options: lowercase, startAfter: 0)
        check(lower.target(at: 0)?.needle == "one" && lower.target(at: 0)?.originalHex == "6f006e006500",
              "paged lowercase query replaced original source spelling")
        let empty = try AtlasReaderFindReport.decode(json(pagedFindPacket(startAfter: 2)), info: info,
            generation: 1, needle: "😀", startAfter: 2)
        check(empty.complete && empty.hits.isEmpty && empty.skippedMatches == 2
            && !empty.hasMoreMatches, "earlier page omissions became phantom future matches")
        let partial = try AtlasReaderFindReport.decode(json(pagedFindPacket(partial: true)), info: info,
            generation: 1, needle: "😀", startAfter: 0)
        check(!partial.complete && !partial.hasMoreMatches && partial.matchesSeen == 1
            && partial.summary.hasPrefix("Partial"), "unknown remainder became a complete result boundary")

        let invalid: [(String, Any)] = [
            ("page_start_after", "1"), ("page_start_after", "00"), ("skipped_matches", "1"),
            ("has_more_matches", false), ("retained_limit", "0"), ("retained_hits", "2"),
            ("matches_seen", "1"), ("rows_truncated", false), ("match_count_complete", true),
            ("case_sensitive", false), ("count_all_matches", true), ("search_complete", true)
        ]
        for (key, value) in invalid {
            var bad = pagedFindPacket(); bad[key] = value
            rejects("inconsistent paging receipt \(key)") {
                _ = try AtlasReaderFindReport.decode(json(bad), info: info, generation: 1, needle: "😀", startAfter: 0)
            }
        }
        for key in ["page_start_after", "skipped_matches", "has_more_matches", "retained_limit",
                    "case_sensitive", "count_all_matches", "match_count_complete", "rows_truncated"] {
            var bad = pagedFindPacket(); bad.removeValue(forKey: key)
            rejects("missing required paging receipt \(key)") {
                _ = try AtlasReaderFindReport.decode(json(bad), info: info, generation: 1, needle: "😀", startAfter: 0)
            }
        }
        for (key, value) in [("occurrence_id", "1"), ("occurrence_id", "3"), ("hit_index", "1")] {
            var bad = secondPacket; var rows = bad["hits"] as! [[String: Any]]
            rows[0][key] = value; bad["hits"] = rows
            rejects("foreign or noncontiguous page row \(key)") {
                _ = try AtlasReaderFindReport.decode(json(bad), info: info, generation: 2, needle: "😀", startAfter: 1)
            }
        }
        rejects("wrong requested page cursor accepted") {
            _ = try AtlasReaderFindReport.decode(json(secondPacket), info: info, generation: 2, needle: "😀", startAfter: 0)
        }
        rejects("legacy packet granted paging authority") {
            _ = try AtlasReaderFindReport.decode(json(findPacket()), info: info, generation: 1, needle: "😀", startAfter: 0)
        }
        rejects("unsolicited paging receipt accepted by a legacy host") {
            _ = try AtlasReaderFindReport.decode(json(pagedFindPacket()), info: info, generation: 1, needle: "😀")
        }
        let legacy = try AtlasReaderFindReport.decode(json(findPacket()), info: info, generation: 1, needle: "😀")
        check(legacy.pageStartAfter == nil && legacy.skippedMatches == 0 && !legacy.hasMoreMatches,
              "legacy complete reports changed meaning")
        let driver = Transport()
        rejects("legacy transport silently restarted a later page at zero") {
            _ = try driver.search.performFind(7, 1, "😀", 4096, UInt64(sourceBytes.count),
                options: .init(), startAfter: 1)
        }
        check(driver.log.isEmpty, "unsupported paging reached the legacy ABI")
    }

    @MainActor private static func pagingNavigation() {
        do {
            let driver = Transport()
            let model = AtlasPagedReaderModel(transport: driver.source, search: driver.pagingSearch)
            model.open(root: "/project", path: "unicode", accessLease: nil)
            wait("paged model open") { !model.busy && model.page != nil }
            check(model.canConfigureFind, "paging-only transport lost Find options")
            model.fileQuery = "😀"; model.find()
            wait("first occurrence page") { !model.busy && model.findReport?.generation == 1 && model.selectedHitIndex == 0 }
            check(model.findPositionLabel == "Match 1 of at least 2" && model.matchHex == "3dd800de"
                && model.nativeSelectionRange == NSRange(location: 0, length: 2), "first paged selection or global label")
            let beforeStart = driver.log
            model.moveHit(backwards: true)
            check(!model.busy && driver.log == beforeStart && model.findReport?.generation == 1,
                  "first-page Previous wrapped before the total was known")
            model.moveHit(backwards: false)
            wait("next page did not reach second global occurrence") {
                !model.busy && model.findReport?.generation == 2 && model.selectedHitIndex == 0
            }
            check(model.findReport?.pageStartAfter == 1 && model.findPositionLabel == "Match 2 of 2"
                && model.nativeSelectionRange == NSRange(location: 7, length: 2) && model.matchHex == "3dd800de",
                "later-page native selection/copy or complete count")
            model.moveHit(backwards: true)
            wait("previous page did not select its final occurrence") {
                !model.busy && model.findReport?.generation == 3 && model.selectedHitIndex == 0
            }
            check(model.findReport?.pageStartAfter == 0 && model.findPositionLabel == "Match 1 of 2"
                && model.nativeSelectionRange == NSRange(location: 0, length: 2),
                "previous-page replay lost exact coordinates or the known immutable total")
            model.moveHit(backwards: true)
            wait("known-total Previous did not wrap to final page") {
                !model.busy && model.findReport?.generation == 4 && model.selectedHitIndex == 0
            }
            check(model.findReport?.pageStartAfter == 1 && model.findPositionLabel == "Match 2 of 2",
                  "known total wrapped within first retained table")
            model.moveHit(backwards: false)
            wait("final-page Next did not wrap to first page") {
                !model.busy && model.findReport?.generation == 5 && model.selectedHitIndex == 0
            }
            check(model.findReport?.pageStartAfter == 0 && model.findPositionLabel == "Match 1 of 2",
                  "end wrap lost the global occurrence count")
            check(driver.log.filter { $0.hasPrefix("page:") } == ["page:1:0", "page:2:1", "page:3:0", "page:4:1", "page:5:0"],
                  "page navigation reused a generation or wrong skip ordinal")
            check(driver.log.contains("global-hit:2:1") && driver.log.contains("global-hit:4:1")
                && !driver.log.contains(where: { $0.hasPrefix("find:") }), "paging bypassed the additive ABI or selected a local index globally")
            model.close()
            wait("paged model close") { driver.log.contains("close") }
            check(driver.log.filter { $0 == "open" }.count == 1, "paging reopened current disk bytes")
        }
        do {
            let driver = Transport()
            let model = AtlasPagedReaderModel(transport: driver.source, search: driver.pagingSearch)
            model.open(root: "/project", path: "unicode", accessLease: nil)
            wait("partial page model open") { !model.busy && model.page != nil }
            driver.limitNextPage(); model.fileQuery = "😀"; model.find()
            wait("partial page first occurrence") { !model.busy && model.selectedHitIndex == 0 }
            let calls = driver.log
            model.moveHit(backwards: false)
            check(!model.busy && driver.log == calls && model.findReport?.complete == false
                && model.findPositionLabel == "Match 1 of at least 1" && model.notice.hasPrefix("Partial"),
                "partial coverage wrapped or manufactured a later page without evidence")
            model.close()
            wait("partial page model close") { driver.log.contains("close") }
        }
        do {
            let driver = Transport()
            let model = AtlasPagedReaderModel(transport: driver.source, search: driver.pagingSearch)
            model.open(root: "/project", path: "unicode", accessLease: nil)
            wait("rejected page model open") { !model.busy && model.page != nil }
            model.fileQuery = "😀"; model.find()
            wait("rejected page initial find") { !model.busy && model.selectedHitIndex == 0 }
            let retained = model.source
            driver.rejectNextPage(); model.moveHit(backwards: false)
            check(model.findReport == nil && model.matchHex == nil && model.nativeSelectionRange == nil
                && model.source === retained, "page attempt kept old hit authority or evicted readable source")
            wait("bad page receipt did not fail") { !model.busy }
            check(model.findReport == nil && !model.canMoveHit && model.source === retained
                && !driver.log.contains("hit:2:0"), "rejected page activated old or unvalidated rows")
            model.find()
            wait("Find did not recover after rejected page") {
                !model.busy && model.findReport?.generation == 3 && model.selectedHitIndex == 0
            }
            check(model.matchHex == "3dd800de", "recovery lost exact copy witness")
            model.close()
            wait("rejected page model close") { driver.log.contains("close") }
        }
        for edit in [false, true] {
            let driver = Transport()
            let model = AtlasPagedReaderModel(transport: driver.source, search: driver.pagingSearch)
            model.open(root: "/project", path: "unicode", accessLease: nil)
            wait("canceled page model open") { !model.busy && model.page != nil }
            model.fileQuery = "😀"; model.find()
            wait("canceled page initial find") { !model.busy && model.selectedHitIndex == 0 }
            let retained = model.source
            driver.pauseFind(); model.moveHit(backwards: false)
            wait("page worker did not pause") { driver.entered.wait(timeout: .now()) == .success }
            if edit {
                model.findOptions = .init(caseSensitive: false, countAllMatches: false)
                model.fileQuery = "ONE"
            } else { model.cancel() }
            check(!model.busy && model.findReport == nil && model.matchHex == nil
                && model.nativeSelectionRange == nil && model.source === retained,
                "edited or canceled page retained authority or replaced the source")
            model.find(); driver.gate.signal()
            wait("replacement query after canceled page did not drain") {
                !model.busy && model.findReport?.generation == 3 && model.selectedHitIndex == 0
            }
            check(!driver.log.contains("hit:2:0") && model.findReport?.pageStartAfter == 0,
                  "late page published or reused a canceled generation")
            if edit {
                check(model.findReport?.needle == "ONE" && driver.log.contains("page-flags:1")
                    && model.matchHex == "6f006e006500" && model.nativeSelectionRange == NSRange(location: 3, length: 3),
                    "replacement lowercase page lost options or original UTF-16 spelling")
            } else {
                check(model.matchHex == "3dd800de" && model.nativeSelectionRange == NSRange(location: 0, length: 2),
                      "canceled page replacement selected the wrong occurrence")
            }
            model.close()
            wait("canceled page model close") { driver.log.contains("close") }
        }
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
        let lowercase = AtlasReaderFindOptions(caseSensitive: false, countAllMatches: false)
        let lowerPacket = configuredPacket(needle: "ONE", options: lowercase)
        let lower = try AtlasReaderFindReport.decode(json(lowerPacket), info: info, generation: 1,
            needle: "ONE", options: lowercase)
        check(lower.complete && lower.hits.count == 1 && lower.options == lowercase,
              "declared lowercase request lost its mode")
        let lowerTarget = lower.target(at: 0)!
        check(lowerTarget.needle == "one" && lowerTarget.originalHex == "6f006e006500",
              "query spelling substituted for original UTF-16 witness")
        let lowerPage = try AtlasReaderPage.decode(json(pagePacket(command: "hit", needle: "one")),
            info: info, request: .hit(lowerTarget))
        check(lowerPage.selection?.target.start == 8 && lowerPage.selection?.utf8Start == 5,
              "case-insensitive hit did not retain exact capture coordinates")
        rejects("lowercase packet accepted as exact") {
            _ = try AtlasReaderFindReport.decode(json(lowerPacket), info: info, generation: 1, needle: "ONE")
        }
        let counting = AtlasReaderFindOptions(caseSensitive: true, countAllMatches: true)
        let countPacket = configuredPacket(options: counting, retainedLimit: 1)
        let counted = try AtlasReaderFindReport.decode(json(countPacket), info: info, generation: 1,
            needle: "😀", options: counting)
        check(counted.complete && counted.matchesSeen == 2 && counted.hits.count == 1 && counted.retainedLimit == 1,
              "full count became truncated coverage or invented retained rows")
        check(counted.summary.contains("2 exact matches counted") && counted.summary.contains("first 1"),
              "summary hid counted occurrences outside navigation rows")
        let zeroCount = try AtlasReaderFindReport.decode(json(configuredPacket(options: counting, retainedLimit: 0)),
            info: info, generation: 1, needle: "😀", options: counting)
        check(zeroCount.complete && zeroCount.matchesSeen == 2 && zeroCount.target(at: 0) == nil,
              "zero-row count lost its complete count or authorized a phantom target")
        let invalidCountFields: [(String, Any)] = [("case_sensitive", false), ("count_all_matches", false),
            ("match_count_complete", false), ("rows_truncated", false), ("retained_limit", "0"),
            ("matches_seen", "18446744073709551615"), ("matches_seen", String(sourceBytes.count + 1))]
        for (key, value) in invalidCountFields {
            var bad = countPacket; bad[key] = value
            rejects("inconsistent counted \(key)") {
                _ = try AtlasReaderFindReport.decode(json(bad), info: info, generation: 1, needle: "😀", options: counting)
            }
        }
        for key in ["case_sensitive", "count_all_matches", "match_count_complete", "rows_truncated", "retained_limit"] {
            var bad = lowerPacket; bad.removeValue(forKey: key)
            rejects("missing declared option \(key)") {
                _ = try AtlasReaderFindReport.decode(json(bad), info: info, generation: 1, needle: "ONE", options: lowercase)
            }
        }
        var incompleteCount = countPacket
        incompleteCount["search_complete"] = false; incompleteCount["match_count_complete"] = false
        incompleteCount["state"] = "byte-limit"; incompleteCount["scanned_bytes"] = "18"
        let observedCount = try AtlasReaderFindReport.decode(json(incompleteCount), info: info, generation: 1,
            needle: "😀", options: counting)
        check(!observedCount.complete && observedCount.summary.contains("More matches may exist"),
              "byte-limited count became an exhaustive answer")
        for (key, value) in [("matched_text", ""), ("original_hex", "6f"), ("original_hex", "6F006E006500")] {
            var bad = lowerPacket; var rows = bad["hits"] as! [[String: Any]]
            rows[0][key] = value; bad["hits"] = rows
            rejects("invalid lowercase row witness \(key)") {
                _ = try AtlasReaderFindReport.decode(json(bad), info: info, generation: 1, needle: "ONE", options: lowercase)
            }
        }
        for invalid in ["", "a\0b", String(repeating: "a", count: 4097)] {
            rejects("invalid find input") { try AtlasReaderFindReport.validateNeedle(invalid) }
        }
        let wideNeedle = String(repeating: "K", count: 4096)
        let wideHex = String(repeating: "4b00", count: 4096)
        var wideInfoPacket = header("info"); wideInfoPacket["captured_bytes"] = "8194"
        let wideInfo = try AtlasReaderInfo.decode(json(wideInfoPacket), handle: 7, path: "/project/unicode")
        var wideFind = findPacket(needle: wideNeedle)
        wideFind["captured_bytes"] = "8194"; wideFind["scanned_bytes"] = "8194"
        wideFind["matches_seen"] = "1"; wideFind["retained_hits"] = "1"
        wideFind["literal_original_hex"] = wideHex
        wideFind["hits"] = [["hit_index": "0", "occurrence_id": "1", "original_range": ["start": "2", "end": "8194"]]]
        let wideReport = try AtlasReaderFindReport.decode(json(wideFind), info: wideInfo, generation: 1, needle: wideNeedle)
        let wideTarget = wideReport.target(at: 0)!
        try AtlasReaderRequest.hit(wideTarget).validate(capturedBytes: 8194)
        var widePagePacket = pagePacket()
        widePagePacket["command"] = "hit"; widePagePacket["captured_bytes"] = "8194"
        widePagePacket["requested_original_range"] = ["start": "0", "end": "8194"]
        widePagePacket["visible_range"] = ["start": "0", "end": "8194"]
        widePagePacket["text"] = wideNeedle; widePagePacket["original_hex"] = "fffe" + wideHex
        widePagePacket["selection"] = ["query_generation": "1", "original_range": ["start": "2", "end": "8194"],
            "window_utf8_range": ["start": "0", "end": "4096"], "original_hex": wideHex]
        let widePage = try AtlasReaderPage.decode(json(widePagePacket), info: wideInfo, request: .hit(wideTarget))
        check(widePage.selection?.target.originalHex == wideHex && widePage.selection?.utf8End == 4096,
            "Expanded source spelling fits exact Find, original UTF-16 selection and copy at the maximum witness bound")
        widePagePacket["text"] = String(repeating: "k", count: 4096)
        rejects("case-changed reader spelling borrowed an original witness") {
            _ = try AtlasReaderPage.decode(json(widePagePacket), info: wideInfo, request: .hit(wideTarget))
        }
        let expandedNeedle = String(repeating: "k", count: 4096)
        let expandedOriginal = String(repeating: "K", count: 4096)
        let expandedHex = String(repeating: "2a21", count: 4096)
        var expanded = wideFind
        expanded["needle"] = expandedNeedle; expanded["mode"] = "unicode-lowercase-literal"
        expanded["case_sensitive"] = false; expanded["count_all_matches"] = false
        expanded["retained_limit"] = "15"; expanded["match_count_complete"] = true; expanded["rows_truncated"] = false
        expanded["literal_original_hex"] = NSNull()
        expanded["hits"] = [["hit_index": "0", "occurrence_id": "1", "original_range": ["start": "2", "end": "8194"],
            "matched_text": expandedOriginal, "original_hex": expandedHex]]
        let expandedReport = try AtlasReaderFindReport.decode(json(expanded), info: wideInfo, generation: 1,
            needle: expandedNeedle, options: lowercase)
        let expandedTarget = expandedReport.target(at: 0)!
        try AtlasReaderRequest.hit(expandedTarget).validate(capturedBytes: 8194)
        widePagePacket["text"] = expandedOriginal; widePagePacket["original_hex"] = "fffe" + expandedHex
        widePagePacket["selection"] = ["query_generation": "1", "original_range": ["start": "2", "end": "8194"],
            "window_utf8_range": ["start": "0", "end": "12288"], "original_hex": expandedHex]
        let expandedPage = try AtlasReaderPage.decode(json(widePagePacket), info: wideInfo, request: .hit(expandedTarget))
        check(expandedPage.selection?.target.needle.utf8.count == 12288,
              "longer original lowercase spelling was incorrectly admitted as another query or refused as a hit")
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
        do {
            let driver = Transport()
            let model = AtlasPagedReaderModel(transport: driver.source, search: driver.configurableSearch)
            model.open(root: "/project", path: "unicode", accessLease: nil)
            wait("configurable find model open") { !model.busy && model.page != nil }
            check(model.canConfigureFind, "optional Find capability hidden")
            model.findOptions = lowercase; model.fileQuery = "ONE"; model.find()
            wait("case-insensitive source activation") { !model.busy && model.selectedHitIndex == 0 }
            check(model.findIsCurrent && model.matchHex == "6f006e006500"
                && model.nativeSelectionRange == NSRange(location: 3, length: 3), "original lowercase source selection/copy")
            let retained = model.source
            model.findOptions.countAllMatches = true
            check(model.findReport == nil && model.matchHex == nil && model.nativeSelectionRange == nil
                && model.source === retained, "option edit did not synchronously revoke old query authority")
            driver.pauseFind(); model.find()
            wait("counting query entered worker") { driver.entered.wait(timeout: .now()) == .success }
            model.findOptions.caseSensitive = true
            check(!model.busy && model.findReport == nil, "case edit did not cancel pending count")
            model.fileQuery = "one"; model.find(); driver.gate.signal()
            wait("replacement option query drained") {
                !model.busy && model.findReport?.generation == 3 && model.selectedHitIndex == 0
            }
            check(model.findReport?.options == counting && driver.log.contains("flags:1")
                && driver.log.contains("flags:3") && driver.log.contains("flags:2"), "requested option bits lost across worker")
            check(!driver.log.contains("hit:2:0") && model.matchHex == "6f006e006500",
                  "canceled option query published or borrowed replacement authority")
            model.close()
            wait("configurable find reader did not retire") { driver.log.contains("close") }
        }
        try pagingReceipts(info: info)
        pagingNavigation()
        print("PASS: \(checks) retained-file find, UTF-16 hit, generation, copy and cancellation checks")
    }
}
