// Production retained-pair coordinator/model tests. Only the C transport is
// injected. These metadata fixtures exercise identity, bounded decoding,
// cancellation, navigation and lifetime; the Rust suites own the diff algorithm
// and real capture hashes, and the Mac bridge fixture owns C marshaling.
import Foundation
import Dispatch

private func hex(_ bytes: some Sequence<UInt8>) -> String {
    bytes.map { String(format: "%02x", $0) }.joined()
}
private func json(_ value: [String: Any]) -> String {
    String(data: try! JSONSerialization.data(withJSONObject: value), encoding: .utf8)!
}
private struct Capture: Sendable {
    let path: String
    let bytes: [UInt8]
    let digest: String
    func header(_ owner: UInt64, _ command: String) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": command,
         "owner": String(owner), "file_id": "1", "source_revision": "1",
         "path": ["encoding": "unix-bytes", "hex": hex(path.utf8), "display": path],
         "captured_bytes": String(bytes.count), "encoding": "utf8",
         "capture_origin": "regular-file-observation-not-atomic", "capture_sha256": digest,
         "additional_source_bytes_read": "0", "native_presented": false]
    }
    func window(_ owner: UInt64, offset: UInt64, limit: UInt64) -> [String: Any] {
        let end = min(UInt64(bytes.count), offset + limit)
        let raw = bytes[Int(offset)..<Int(end)]
        var out = header(owner, "window")
        out["text_kind"] = "logical-captured-text-not-shaped"
        out["requested_original_range"] = ["start": String(offset), "end": String(end)]
        out["visible_range"] = ["start": String(offset), "end": String(end)]
        out["range_limited"] = false; out["boundaries_adjusted"] = false
        out["has_replacements"] = false
        out["text"] = String(decoding: raw, as: UTF8.self); out["original_hex"] = hex(raw)
        out["first_physical_line"] = NSNull(); out["selection"] = NSNull()
        out["next_offset"] = end < UInt64(bytes.count) ? String(end) as Any : NSNull()
        return out
    }
}

private final class Lease {}
private final class Driver: @unchecked Sendable {
    private let lock = NSLock()
    private var next: UInt64 = 1
    private var captures: [UInt64: Capture] = [:]
    private var calls: [String] = []
    private var blocked = false
    private var limited = false
    private var corruption: String?
    private var blockedWindow: UInt64?
    private var failedWindow: UInt64?
    private let sourceBytes: [UInt8]?
    let domain = UUID()
    let entered = DispatchSemaphore(value: 0)
    let gate = DispatchSemaphore(value: 0)

    init(bytes: [UInt8]? = nil) { sourceBytes = bytes }
    var log: [String] { lock.lock(); defer { lock.unlock() }; return calls }
    func blockCompare() { lock.lock(); blocked = true; lock.unlock() }
    func limitCompare() { lock.lock(); limited = true; lock.unlock() }
    func corrupt(_ kind: String) { lock.lock(); corruption = kind; lock.unlock() }
    func blockWindow(_ owner: UInt64) { lock.lock(); blockedWindow = owner; lock.unlock() }
    func failWindow(_ owner: UInt64) { lock.lock(); failedWindow = owner; lock.unlock() }

    private func create() -> UInt64 {
        lock.lock(); defer { lock.unlock() }
        let id = next; next += 1; calls.append("create:\(id)"); return id
    }
    private func open(_ owner: UInt64, path: String) -> String {
        precondition(!Thread.isMainThread)
        lock.lock(); defer { lock.unlock() }
        let capture = Capture(path: path,
            bytes: sourceBytes ?? Array((path.hasSuffix("/after") ? "new after" : "old before").utf8),
            digest: String(repeating: owner % 2 == 1 ? "a" : "b", count: 64))
        captures[owner] = capture; calls.append("open:\(owner):\(path)")
        return json(capture.header(owner, "info"))
    }
    private func window(_ owner: UInt64, offset: UInt64, bytes: UInt64) -> String? {
        precondition(!Thread.isMainThread)
        lock.lock()
        let capture = captures[owner], pause = blockedWindow == owner, fail = failedWindow == owner
        if pause { blockedWindow = nil }
        if fail { failedWindow = nil }
        calls.append("window:\(owner):\(offset)")
        lock.unlock()
        if pause {
            entered.signal()
            precondition(gate.wait(timeout: .now() + .seconds(5)) == .success)
        }
        guard !fail, let capture, offset <= UInt64(capture.bytes.count) else { return nil }
        return json(capture.window(owner, offset: offset, limit: bytes))
    }
    private func cancel(_ owner: UInt64) -> Bool {
        lock.lock(); calls.append("cancel:\(owner)"); lock.unlock(); return true
    }
    private func close(_ owner: UInt64) -> Bool {
        precondition(!Thread.isMainThread)
        lock.lock(); calls.append("close:\(owner)"); captures[owner] = nil; lock.unlock(); return true
    }
    private func compare(_ before: UInt64, _ after: UInt64, _ generation: UInt64,
                         _ edits: UInt64, _ work: UInt64) -> String? {
        precondition(!Thread.isMainThread)
        lock.lock()
        let old = captures[before], new = captures[after]
        let pause = blocked, partial = limited, corrupt = corruption
        blocked = false; limited = false; corruption = nil
        calls.append("compare:\(before):\(after):\(generation)")
        lock.unlock()
        if pause {
            entered.signal()
            precondition(gate.wait(timeout: .now() + .seconds(5)) == .success)
        }
        guard let old, let new else { return nil }
        let incomplete = partial || work == 0 || edits < 13
        func span(_ id: Int, _ kind: String, _ a: Int, _ b: Int, _ c: Int, _ d: Int) -> [String: Any] {
            ["span": String(id), "kind": kind,
             "before_range": ["start": String(a), "end": String(b)],
             "after_range": ["start": String(c), "end": String(d)]]
        }
        var spans = incomplete
            ? [span(0, "unresolved", 0, old.bytes.count, 0, new.bytes.count)]
            : [span(0, "changed", 0, 3, 0, 3), span(1, "equal", 3, 4, 3, 4),
               span(2, "changed", 4, 6, 4, 5), span(3, "equal", 6, 7, 5, 6),
               span(4, "changed", 7, 8, 6, 8), span(5, "equal", 8, 9, 8, 9),
               span(6, "changed", 9, 10, 9, 9)]
        var out = old.header(before, "compare")
        out["comparison_owner"] = "1000"; out["comparison_generation"] = String(generation)
        out["comparison_quality"] = incomplete ? (edits < 13 && work != 0 ? "edit-limit" : "work-limit") : "exact"
        out["comparison_complete"] = !incomplete
        out["relation"] = incomplete ? "undetermined" : "different"
        out["source_reopened"] = false
        out["coordinate_domain"] = "original-bytes-may-split-text"
        out["alignment_semantics"] = "deterministic-not-identity-proof"
        out["max_source_bytes"] = String(AtlasReaderLimits.comparisonBytes)
        out["max_edit_distance"] = String(edits); out["max_work"] = String(work)
        out["work_units"] = String(incomplete ? work : min(32, work))
        out["edit_distance"] = incomplete ? NSNull() as Any : "13"
        out["before_source"] = old.header(before, "info")
        out["after_source"] = new.header(after, "info")
        switch corrupt {
        case "owner": out["owner"] = "999"
        case "generation": out["comparison_generation"] = String(generation + 1)
        case "analysisdomain": out["comparison_owner"] = String(before)
        case "swap":
            out["before_source"] = new.header(after, "info")
            out["after_source"] = old.header(before, "info")
        case "digest":
            var source = new.header(after, "info"); source["capture_sha256"] = old.digest
            out["after_source"] = source
        case "path":
            var source = new.header(after, "info")
            source["path"] = ["encoding": "unix-bytes", "hex": hex("/elsewhere".utf8), "display": new.path]
            out["after_source"] = source
        case "hole": spans[0]["before_range"] = ["start": "1", "end": "3"]
        case "overflow": spans[spans.count - 1]["before_range"] = ["start": "9", "end": String(UInt64.max)]
        case "equal": spans[spans.count - 1]["kind"] = "equal"
        case "unresolved": spans[0]["kind"] = "unresolved"
        case "work": out["work_units"] = String(work + 1)
        case "limits": out["max_edit_distance"] = String(edits + 1)
        case "reopened": out["source_reopened"] = true
        default: break
        }
        out["total_spans"] = String(spans.count + (corrupt == "count" ? 1 : 0))
        out["spans"] = spans
        return json(out)
    }
    func transport(comparisonDomain: UUID? = nil, supportsComparison: Bool = true) -> AtlasReaderTransport {
        let comparison: AtlasReaderComparisonTransport?
        if supportsComparison {
            comparison = AtlasReaderComparisonTransport(domain: comparisonDomain ?? domain,
                compare: { [self] in compare($0, $1, $2, $3, $4) })
        } else { comparison = nil }
        return AtlasReaderTransport(create: { [self] in create() },
            open: { [self] owner, path, limit in
                precondition(limit == AtlasReaderLimits.captureBytes)
                return open(owner, path: path)
            }, window: { [self] in window($0, offset: $1, bytes: $2) },
            lines: { _, _, _, _ in nil }, cancel: { [self] in cancel($0) },
            close: { [self] in close($0) }, retirementFailed: { fatalError("failed retirement") },
            comparison: comparison)
    }
}

@main @MainActor private enum AtlasReaderComparisonTests {
    private static var checks = 0
    private static func check(_ value: @autoclosure () -> Bool, _ message: String) {
        precondition(value(), message); checks += 1
    }
    private static func wait(_ message: String, _ ready: () -> Bool) {
        let deadline = Date().addingTimeInterval(5)
        var done = ready()
        while !done, Date() < deadline {
            _ = RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.005))
            done = ready()
        }
        check(done, message)
    }
    private static func opened(_ driver: Driver, path: String, lease: AnyObject? = nil,
                               domain: UUID? = nil, supported: Bool = true) -> AtlasPagedReaderModel {
        let model = AtlasPagedReaderModel(transport: driver.transport(
            comparisonDomain: domain, supportsComparison: supported))
        model.open(root: "/project", path: path, accessLease: lease)
        wait("reader opening did not finish") { !model.busy }
        check(model.page != nil, "reader opening failed")
        return model
    }
    private static func compared(_ before: AtlasPagedReaderModel, _ after: AtlasPagedReaderModel,
                                 limits: AtlasReaderComparisonLimits = .init()) -> AtlasReaderComparisonReport {
        var report: AtlasReaderComparisonReport?
        Task {
            do { report = try await before.compare(with: after, limits: limits) }
            catch { fatalError("comparison failed: \(error)") }
        }
        wait("comparison did not finish") { report != nil }
        return report!
    }
    private static func refused(_ before: AtlasPagedReaderModel, _ after: AtlasPagedReaderModel,
                                limits: AtlasReaderComparisonLimits = .init()) {
        var failed = false
        Task {
            do { _ = try await before.compare(with: after, limits: limits); fatalError("invalid comparison published") }
            catch { failed = true }
        }
        wait("comparison did not refuse") { failed }
    }

    static func main() {
        for count in [AtlasReaderLimits.comparisonBytes, AtlasReaderLimits.comparisonBytes + 1] {
            let driver = Driver(bytes: Array(repeating: 120, count: Int(count)))
            let before = opened(driver, path: "before"), after = opened(driver, path: "after")
            check(before.canCompare == (count <= AtlasReaderLimits.comparisonBytes),
                  "comparison eligibility followed the larger live capture limit")
            if count > AtlasReaderLimits.comparisonBytes {
                let calls = driver.log
                refused(before, after)
                check(driver.log == calls, "oversized comparison reached the foreign operation")
                check(before.page?.identity.capturedBytes == count && after.page?.identity.capturedBytes == count,
                      "comparison refusal discarded a readable source")
            }
            before.close(); after.close()
        }
        do {
            let driver = Driver()
            let inert = AtlasPagedReaderModel(transport: driver.transport())
            check(!inert.canCompare && driver.log.isEmpty, "capability discovery opened a source")
            let before = opened(driver, path: "before")
            let after = opened(driver, path: "after")
            let beforePage = before.page!, afterPage = after.page!
            let beforeNavigation = before.navigation, afterNavigation = after.navigation
            let opens = driver.log.filter { $0.hasPrefix("open:") }.count
            let report = compared(before, after)
            check(report.quality == "exact" && report.changes.count == 4, "exact correspondence was lost")
            check(report.matches(before: beforePage.identity, after: afterPage.identity), "report borrowed source identity")
            check(report.beforeSHA256 == String(repeating: "a", count: 64)
                && report.afterSHA256 == String(repeating: "b", count: 64), "report lost capture witnesses")
            check(before.navigation == beforeNavigation && after.navigation == afterNavigation,
                  "comparison changed reader navigation")
            check(before.page?.originalHex == beforePage.originalHex && after.page?.originalHex == afterPage.originalHex,
                  "comparison changed retained bytes")
            check(driver.log.filter { $0.hasPrefix("open:") }.count == opens, "comparison reopened source paths")
            check(before.goToComparedRange(report.spans[2].before, identity: report.before),
                  "accepted before anchor was refused")
            wait("comparison anchor did not navigate") { !before.busy }
            check(before.currentComparisonSelection?.range == report.spans[2].before
                && before.nativeSelectionRange == NSRange(location: 4, length: 2),
                  "comparison navigation lost the range end or exact native selection")
            let selected = before.currentComparisonSelection!
            check(before.comparisonCopy(.shownHex, selectionID: selected.id) == hex("be".utf8)
                && before.comparisonCopy(.shownText, selectionID: selected.id) == "be",
                  "compared copy included page context or lost captured bytes")
            check(after.navigation == afterNavigation, "before anchor navigated after reader")
            let unchanged = before.navigation
            check(!before.goToComparedRange(report.spans[2].after, identity: report.after),
                  "foreign reader identity authorized navigation")
            check(before.navigation == unchanged, "foreign anchor changed navigation")
            driver.limitCompare()
            let limited = compared(before, after)
            check(limited.quality == "work-limit" && limited.changes.first?.kind == .unresolved,
                  "bounded comparison promoted unresolved bytes")
            check(limited.generation > report.generation && limited.summary.contains("unresolved"),
                  "limited correspondence lost generation or coverage")
            let refined = compared(before, after, limits: .init(editDistance: 512, work: 64 * 1024 * 1024))
            check(refined.quality == "exact" && refined.generation > limited.generation,
                  "refinement recycled a request or lost exact coverage")
            before.close(); after.close()
        }

        do {
            let driver = Driver()
            let before = opened(driver, path: "before"), after = opened(driver, path: "after")
            let beforeIdentity = before.page!.identity, afterIdentity = after.page!.identity
            let beforeNavigation = before.navigation, afterNavigation = after.navigation
            for kind in ["owner", "generation", "analysisdomain", "swap", "digest", "path", "hole",
                         "overflow", "equal", "unresolved", "work", "limits", "reopened", "count"] {
                driver.corrupt(kind); refused(before, after)
                check(before.page?.identity.matches(beforeIdentity) == true
                    && after.page?.identity.matches(afterIdentity) == true,
                    "malformed \(kind) replaced a source")
                check(before.navigation == beforeNavigation && after.navigation == afterNavigation,
                      "malformed \(kind) changed navigation")
            }
            let calls = driver.log
            refused(before, after, limits: .init(editDistance: 513, work: 1))
            refused(before, after, limits: .init(editDistance: 1, work: 64 * 1024 * 1024 + 1))
            refused(before, before)
            check(driver.log == calls, "invalid pair or budget reached the native boundary")
            before.close(); after.close()
        }

        do {
            let driver = Driver()
            let before = opened(driver, path: "before")
            let foreign = opened(driver, path: "after", domain: UUID())
            let legacy = opened(driver, path: "after", supported: false)
            check(!legacy.canCompare, "legacy transport acquired comparison authority")
            let calls = driver.log
            refused(before, foreign); refused(before, legacy)
            check(driver.log == calls, "foreign registry or missing capability crossed native boundary")
            before.close(); foreign.close(); legacy.close()
        }

        // Rejection must wait for the blocking foreign call to drain while BOTH
        // original grants remain retained. Neither source may be revived later.
        for action in ["cancel", "close-before", "close-after", "replace-after"] {
            let driver = Driver()
            var oldLease: Lease? = Lease()
            var newLease: Lease? = Lease()
            weak var weakOldLease = oldLease
            weak var weakNewLease = newLease
            let before = opened(driver, path: "before", lease: oldLease)
            let after = opened(driver, path: "after", lease: newLease)
            let originalAfter = after.page!.identity
            oldLease = nil; newLease = nil
            driver.blockCompare()
            var failed = false
            let task = Task {
                do { _ = try await before.compare(with: after); fatalError("obsolete \(action) comparison published") }
                catch { failed = true }
            }
            wait("comparison did not enter the worker") { driver.entered.wait(timeout: .now()) == .success }
            switch action {
            case "cancel": task.cancel()
            case "close-before": before.close()
            case "close-after": after.close()
            default: after.open(root: "/project", path: "after", accessLease: nil)
            }
            if action == "replace-after" {
                wait("after replacement did not finish") { !after.busy }
                check(after.page?.identity.matches(originalAfter) == false, "replacement reused original identity")
            }
            check(weakOldLease != nil && weakNewLease != nil, "source grant released during comparison drain")
            check(!failed, "comparison abandoned native work before drain")
            driver.gate.signal()
            wait("obsolete comparison did not terminate") { failed }
            if action == "cancel" {
                check(before.page != nil && after.page != nil, "caller cancellation closed retained sources")
                check(driver.log.contains("cancel:1") && driver.log.contains("cancel:2"),
                      "caller cancellation did not signal both native sources")
            }
            if action == "close-before" { check(before.page == nil, "comparison revived closed before reader") }
            if action == "close-after" { check(after.page == nil, "comparison revived closed after reader") }
            if action == "replace-after" {
                check(after.page?.identity.matches(originalAfter) == false, "comparison undid source replacement")
            }
            before.close(); after.close()
            wait("comparison leaked an original grant") { weakOldLease == nil && weakNewLease == nil }
        }

        do {
            let driver = Driver()
            let before = opened(driver, path: "before"), after = opened(driver, path: "after")
            let model = AtlasReaderComparisonModel()
            model.compare(before: before, after: after)
            wait("comparison presentation did not finish") { !model.busy }
            check(model.report != nil, "comparison model dropped accepted report")
            after.close()
            model.validate(before: before.page?.identity, after: after.page?.identity)
            check(model.report == nil && !model.busy && model.notice.contains("changed or closed"),
                  "comparison model kept a closed-source report")
            model.close(); before.close()
        }
        testPairedNavigation()
        testLargeRange()
        testAtomicNavigation()
        testRangePresentation()
        print("AtlasReaderComparisonTests: \(checks) assertions passed")
    }

    private static func readyComparison(_ before: AtlasPagedReaderModel,
                                        _ after: AtlasPagedReaderModel) -> AtlasReaderComparisonModel {
        let model = AtlasReaderComparisonModel()
        model.compare(before: before, after: after)
        wait("comparison presentation did not finish") { !model.busy }
        check(model.report != nil, "comparison presentation lost its report")
        return model
    }

    private static func testPairedNavigation() {
        let driver = Driver()
        let before = opened(driver, path: "before"), after = opened(driver, path: "after")
        let model = readyComparison(before, after)
        let opens = driver.log.filter { $0.hasPrefix("open:") }.count
        model.moveChange(backwards: false, before: before, after: after)
        wait("first paired change did not open") { !model.busy }
        check(model.selectedSpanID == 0 && model.changePosition == "Change 1 of 4",
              "first change lost its accepted ordinal")
        check(before.nativeSelectionRange == NSRange(location: 0, length: 3)
            && after.nativeSelectionRange == NSRange(location: 0, length: 3),
              "paired change did not select both exact ranges")
        check(!model.canMoveChange(backwards: true) && model.canMoveChange(backwards: false),
              "changed-range navigation wrapped or skipped its first boundary")
        let oldSelection = before.currentComparisonSelection!.id
        model.moveChange(backwards: false, before: before, after: after)
        wait("next paired change did not open") { !model.busy }
        check(model.selectedSpanID == 2 && before.nativeSelectionRange == NSRange(location: 4, length: 2)
            && after.nativeSelectionRange == NSRange(location: 4, length: 1),
              "Next did not skip equal bytes or preserve different range lengths")
        check(before.comparisonCopy(.shownHex, selectionID: oldSelection) == nil,
              "an old compared-range copy action remained authorized")
        model.open(spanID: 3, before: before, after: after)
        wait("equal range did not open") { !model.busy }
        check(model.changePosition == "Equal range selected", "equal range claimed a change ordinal")
        model.moveChange(backwards: true, before: before, after: after)
        wait("previous change from equal range did not open") { !model.busy }
        check(model.selectedSpanID == 2, "Previous from an equal range chose the wrong change")
        model.open(spanID: 6, before: before, after: after)
        wait("deletion point did not open") { !model.busy }
        check(before.currentComparisonSelection?.range == .init(start: 9, end: 10)
            && after.currentComparisonSelection?.range == .init(start: 9, end: 9)
            && after.nativeSelectionRange == NSRange(location: 9, length: 0),
              "deletion point was widened into nonexistent after bytes")
        check(after.currentComparisonSelection?.shownHex == ""
            && !model.canMoveChange(backwards: false) && model.changePosition == "Change 4 of 4",
              "point copy or last change boundary was misrepresented")
        check(driver.log.filter { $0.hasPrefix("open:") }.count == opens,
              "changed-range navigation reopened a live source")
        let navigation = before.navigation
        check(!before.goToComparedRange(.init(start: 8, end: 7), identity: before.page!.identity)
            && !before.goToComparedRange(.init(start: 0, end: 11), identity: before.page!.identity)
            && before.navigation == navigation, "invalid compared bounds changed the reader")
        let previous = before.currentComparisonSelection!.id
        before.updateTextSelection(source: before.source!, navigation: before.navigation, range: nil)
        check(before.currentComparisonSelection == nil
            && before.comparisonCopy(.location, selectionID: previous) == nil,
              "a changed native selection kept an older compared-range action")
        model.close(); before.close(); after.close()
    }

    private static func testLargeRange() {
        let count = Int(AtlasReaderLimits.pageBytes * 3 + 37)
        let driver = Driver(bytes: Array(repeating: 120, count: count))
        let before = opened(driver, path: "before"), after = opened(driver, path: "after")
        driver.limitCompare()
        let model = readyComparison(before, after)
        model.moveChange(backwards: false, before: before, after: after)
        wait("large unresolved range did not open") { !model.busy }
        let selected = before.currentComparisonSelection!
        check(selected.range == .init(start: 0, end: UInt64(count)) && !selected.showsWholeRange,
              "large comparison collapsed its full target to the viewport")
        check(selected.shownRange == .init(start: 0, end: AtlasReaderLimits.pageBytes)
            && selected.shownHex?.utf8.count == Int(AtlasReaderLimits.pageBytes * 2)
            && selected.summary.contains("only"),
              "clipped comparison copied context or claimed complete coverage")
        check(before.nativeSelectionRange == NSRange(location: 0, length: Int(AtlasReaderLimits.pageBytes)),
              "large comparison did not select its bounded visible intersection")
        check(before.comparisonCopy(.location, selectionID: selected.id)?.contains("0..<\(count)") == true,
              "copied location lost the complete compared range")
        check(before.goToComparedRange(selected.range, identity: selected.identity, atEnd: true),
              "large range end was unavailable")
        wait("large range end did not open") { !before.busy }
        check(before.currentComparisonSelection?.range == selected.range
            && before.currentComparisonSelection?.shownRange?.end == UInt64(count)
            && before.page!.start > 0 && before.canGoBack,
              "range end navigation truncated its target or lost source history")
        before.back()
        wait("Back after range end did not finish") { !before.busy }
        check(before.page?.start == 0 && before.currentComparisonSelection == nil,
              "plain source history resurrected a comparison selection")
        model.close(); before.close(); after.close()
    }

    private static func testAtomicNavigation() {
        for action in ["fail-after", "cancel", "navigate-before", "replace-after"] {
            let driver = Driver()
            let before = opened(driver, path: "before"), after = opened(driver, path: "after")
            let model = readyComparison(before, after)
            model.open(spanID: 0, before: before, after: after)
            wait("initial paired selection did not open") { !model.busy }
            let oldBefore = before.navigation, oldAfter = after.navigation
            let oldSelection = before.currentComparisonSelection!.id
            if action == "fail-after" { driver.failWindow(after.page!.identity.owner) }
            else { driver.blockWindow(after.page!.identity.owner) }
            model.open(spanID: 6, before: before, after: after)
            if action == "fail-after" {
                wait("failed paired navigation did not finish") { !model.busy }
                check(before.navigation == oldBefore && after.navigation == oldAfter
                    && before.currentComparisonSelection?.id == oldSelection && model.selectedSpanID == 0,
                      "second-side failure published a partial pair or changed its ordinal")
            } else {
                wait("paired navigation did not block on its second side") { driver.entered.wait(timeout: .now()) == .success }
                check(before.navigation == oldBefore && after.navigation == oldAfter,
                      "first side published before the second side was ready")
                switch action {
                case "cancel":
                    model.cancel()
                    check(!before.busy && !after.busy && before.navigation == oldBefore
                        && after.navigation == oldAfter && model.selectedSpanID == 0,
                          "canceled pair changed accepted source state")
                    // A new request queues behind the draining old window.
                    // Its completion also proves late cancellation cannot
                    // revoke a newer pair's reservations.
                    model.moveChange(backwards: false, before: before, after: after)
                    driver.gate.signal()
                    wait("pair did not recover after cancellation") { !model.busy }
                    check(model.selectedSpanID == 2
                        && before.currentComparisonSelection?.range == .init(start: 4, end: 6),
                          "late canceled navigation displaced the new change")
                case "navigate-before":
                    before.offsetInput = "2"; before.goToByte()
                    wait("independent navigation did not finish") { !before.busy }
                    let independent = before.navigation
                    driver.gate.signal()
                    wait("obsolete paired navigation did not finish") { !model.busy }
                    check(before.navigation == independent && before.page?.start == 2
                        && after.navigation == oldAfter && model.selectedSpanID == 0,
                          "obsolete pair replaced independent navigation or published its other side")
                default:
                    after.open(root: "/project", path: "after", accessLease: nil)
                    model.validate(before: before.page?.identity, after: after.page?.identity)
                    driver.gate.signal()
                    wait("replacement source did not finish") { !after.busy }
                    check(model.report == nil && before.navigation == oldBefore
                        && after.currentComparisonSelection == nil,
                          "old comparison revived a selection after source replacement")
                }
            }
            model.close(); before.close(); after.close()
        }
    }

    private static func testRangePresentation() {
        func selection(_ text: String, bytes: [UInt8], encoding: String = "utf8", start: UInt64 = 0,
                       target: AtlasReaderComparisonReport.Range, replacements: Bool = false) -> AtlasReaderComparisonSelection {
            let identity = AtlasReaderIdentity(owner: 1, file: 1, revision: 1,
                capturedBytes: start + UInt64(bytes.count), pathBytes: Array("/source".utf8),
                encoding: encoding, displayPath: "/source")
            let page = AtlasReaderPage(identity: identity, start: start, end: identity.capturedBytes,
                text: text, originalHex: hex(bytes), firstPhysicalLine: nil, rangeLimited: false,
                boundariesAdjusted: false, hasReplacements: replacements, highlight: nil,
                selection: nil, symbolSelection: nil)
            return try! AtlasReaderComparisonSelection.make(range: target, identity: identity, page: page)
        }
        let text = "a😀e\u{301}\r\nz"
        let bytes = Array(text.utf8)
        let emoji = selection(text, bytes: bytes, target: .init(start: 1, end: 5))
        check(emoji.nativeRange == NSRange(location: 1, length: 2)
            && emoji.shownText == "😀" && emoji.shownHex == hex("😀".utf8),
              "exact multibyte comparison lost native or original-byte selection")
        for range in [AtlasReaderComparisonReport.Range(start: 2, end: 4),
                      .init(start: 5, end: 6), .init(start: 8, end: 9)] {
            let split = selection(text, bytes: bytes, target: range)
            check(split.nativeRange == nil && split.shownText == nil
                && split.shownHex == hex(bytes[Int(range.start)..<Int(range.end)]),
                  "scalar, composed character or CRLF split rounded a comparison target")
        }
        for encoding in ["utf16le", "utf16be"] {
            let raw = Array(text.data(using: encoding == "utf16le" ? .utf16LittleEndian : .utf16BigEndian)!)
            let exact = selection(text, bytes: raw, encoding: encoding, target: .init(start: 2, end: 6))
            check(exact.nativeRange == NSRange(location: 1, length: 2) && exact.shownText == "😀",
                  "UTF-16 comparison offsets were treated as decoded UTF-8")
            for range in [AtlasReaderComparisonReport.Range(start: 3, end: 6), .init(start: 2, end: 4)] {
                let split = selection(text, bytes: raw, encoding: encoding, target: range)
                check(split.nativeRange == nil && split.shownHex == hex(raw[Int(range.start)..<Int(range.end)]),
                      "odd UTF-16 byte offset or split surrogate was rounded or lost its original bytes")
            }
        }
        let malformed = selection("a�b", bytes: [97, 255, 98], target: .init(start: 1, end: 2), replacements: true)
        check(malformed.nativeRange == nil && malformed.shownHex == "ff",
              "replacement display acquired an unproved exact comparison highlight")
        let bom = selection("abc", bytes: Array("abc".utf8), start: 3, target: .init(start: 0, end: 3))
        check(bom.shownRange == nil && bom.shownHex == nil && !bom.showsWholeRange,
              "undisplayed BOM bytes were substituted with neighboring text")
        let partialBOM = selection("abc", bytes: Array("abc".utf8), start: 3, target: .init(start: 0, end: 5))
        check(partialBOM.shownRange == .init(start: 3, end: 5) && partialBOM.shownText == "ab"
            && partialBOM.shownHex == "6162" && !partialBOM.showsWholeRange,
              "a partially displayed range concealed its omitted BOM bytes")
        let mismatched = selection("xxx", bytes: Array("abc".utf8), target: .init(start: 0, end: 2))
        check(mismatched.nativeRange == nil && mismatched.shownHex == "6162",
              "unverified decoded text acquired exact original-byte selection authority")
        let insertion = selection("abc", bytes: Array("abc".utf8), target: .init(start: 0, end: 0))
        let eof = selection("abc", bytes: Array("abc".utf8), target: .init(start: 3, end: 3))
        check(insertion.nativeRange == NSRange(location: 0, length: 0)
            && eof.nativeRange == NSRange(location: 3, length: 0)
            && insertion.shownHex == "" && eof.showsWholeRange,
              "insertion/deletion point anchors became nonempty text selections")
    }
}
