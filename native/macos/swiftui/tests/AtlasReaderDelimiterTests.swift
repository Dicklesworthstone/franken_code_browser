// Production bracket receipt, coordinator and navigation regressions. The
// injected foreign boundary supplies fixed witnesses; it does not implement a
// second lexer. Run with the selection suite through strict RCH. These tests
// do not claim Rust execution or physical-Mac presentation qualification.
import Foundation
import Dispatch

private func bracketHex(_ bytes: some Sequence<UInt8>) -> String {
    bytes.map { String(format: "%02x", $0) }.joined()
}
private func bracketJSON(_ object: [String: Any]) -> String {
    String(decoding: try! JSONSerialization.data(withJSONObject: object), as: UTF8.self)
}
private func bracketHeader(owner: UInt64 = 2, length: UInt64, encoding: String = "utf8",
                           command: String) -> [String: Any] {
    ["schema": "fcb.reader-session/1", "status": "ok", "command": command,
     "owner": String(owner), "file_id": "1", "source_revision": "1",
     "captured_bytes": String(length), "encoding": encoding,
     "path": ["encoding": "unix-bytes", "hex": bracketHex("/project/source".utf8), "display": "/project/source"],
     "additional_source_bytes_read": "0", "native_presented": false]
}
private func bracketReceipt(owner: UInt64 = 2, length: UInt64, encoding: String = "utf8",
                            start: UInt64, end: UInt64, mate: (UInt64, UInt64)?,
                            state: String = "matched") -> [String: Any] {
    var wire = bracketHeader(owner: owner, length: length, encoding: encoding, command: "matching-delimiter")
    wire["delimiter_state"] = state
    wire["selected_range"] = ["start": String(start), "end": String(end)]
    if let mate { wire["matching_range"] = ["start": String(mate.0), "end": String(mate.1)] }
    else { wire["matching_range"] = NSNull() }
    wire["evidence_level"] = "lexical-delimiter-candidate"
    wire["source_context"] = "complete-capture"
    wire["semantic_resolution"] = false
    wire["language"] = "rust"
    return wire
}
private func bracketSelection(page: AtlasReaderPage, range: NSRange,
                              start: UInt64, end: UInt64) throws -> AtlasReaderTextSelection {
    let request = try AtlasReaderTextSelectionRequest(page: page, nativeRange: range)
    var wire = bracketHeader(owner: page.identity.owner, length: page.identity.capturedBytes,
        encoding: page.identity.encoding, command: "resolve-selection")
    wire["selection_namespace"] = "native-text"
    wire["window_original_range"] = ["start": String(page.start), "end": String(page.end)]
    wire["window_utf8_range"] = ["start": String(request.utf8Start), "end": String(request.utf8End)]
    wire["original_range"] = ["start": String(start), "end": String(end)]
    wire["text"] = request.text
    let bytes = Array(page.originalHex.utf8)
    wire["original_hex"] = String(decoding: bytes[Int(start - page.start) * 2..<Int(end - page.start) * 2], as: UTF8.self)
    wire["contains_replacements"] = false
    return try AtlasReaderTextSelection.decode(bracketJSON(wire), request: request)
}

private final class BracketFixture: @unchecked Sendable {
    enum Mode: Equatable { case matched, unmatched, mismatched, notCode, wrongIdentity, wrongMate, sourceLimit }
    private let lock = NSLock()
    private var next: UInt64 = 2
    private var mode = Mode.matched
    private var pauseMatch = false
    private var pauseWindow = false
    private var refuseWindow = false
    private var opened = 0
    private var matches = 0
    private var matchReturns = 0
    private var windows = 0
    private var windowReturns = 0
    private var blocked = 0
    let gate = DispatchSemaphore(value: 0)
    let raw: [UInt8]

    init(padding: Int = 70_000) {
        raw = Array(("(" + String(repeating: "x", count: padding) + ")").utf8)
    }

    private func locked<Value>(_ body: () -> Value) -> Value { lock.lock(); defer { lock.unlock() }; return body() }
    func setMode(_ value: Mode) { locked { mode = value } }
    func blockMatch() { locked { pauseMatch = true } }
    func blockWindow() { locked { pauseWindow = true } }
    func failWindow() { locked { refuseWindow = true } }
    var calls: (opened: Int, matches: Int, matchReturns: Int, windows: Int, windowReturns: Int, blocked: Int) {
        locked { (opened, matches, matchReturns, windows, windowReturns, blocked) }
    }
    private func pause(_ blocked: Bool) {
        if blocked { precondition(gate.wait(timeout: .now() + .seconds(5)) == .success) }
    }

    var transport: AtlasReaderTransport {
        AtlasReaderTransport(create: { [self] in locked { let handle = next; next += 1; return handle } },
            open: { [self] owner, _, _ in
                precondition(!Thread.isMainThread)
                locked { opened += 1 }
                var wire = bracketHeader(owner: owner, length: UInt64(raw.count), command: "info")
                wire["capture_origin"] = "regular-file-observation-not-atomic"
                return bracketJSON(wire)
            }, window: { [self] owner, offset, limit in
                precondition(!Thread.isMainThread)
                let behavior = locked {
                    windows += 1
                    let behavior = (pauseWindow, refuseWindow)
                    pauseWindow = false; refuseWindow = false
                    if behavior.0 { blocked += 1 }
                    return behavior
                }
                pause(behavior.0)
                defer { locked { windowReturns += 1 } }
                guard !behavior.1 else { return nil }
                let end = min(UInt64(raw.count), offset + limit)
                let bytes = raw[Int(offset)..<Int(end)]
                var wire = bracketHeader(owner: owner, length: UInt64(raw.count), command: "window")
                wire["text_kind"] = "logical-captured-text-not-shaped"
                wire["requested_original_range"] = ["start": String(offset), "end": String(end)]
                wire["visible_range"] = ["start": String(offset), "end": String(end)]
                wire["range_limited"] = end - offset < min(UInt64(raw.count) - offset, limit)
                wire["boundaries_adjusted"] = false; wire["has_replacements"] = false
                wire["first_physical_line"] = NSNull(); wire["selection"] = NSNull()
                wire["text"] = String(decoding: bytes, as: UTF8.self); wire["original_hex"] = bracketHex(bytes)
                wire["next_offset"] = end < UInt64(raw.count) ? String(end) as Any : NSNull()
                return bracketJSON(wire)
            }, lines: { _, _, _, _ in nil }, cancel: { _ in true }, close: { _ in true },
            retirementFailed: {}, textSelection: AtlasReaderTextSelectionTransport(resolve: { [self] owner, start, end, first, last in
                precondition(!Thread.isMainThread)
                var wire = bracketHeader(owner: owner, length: UInt64(raw.count), command: "resolve-selection")
                let bytes = raw[Int(start + first)..<Int(start + last)]
                wire["selection_namespace"] = "native-text"
                wire["window_original_range"] = ["start": String(start), "end": String(end)]
                wire["window_utf8_range"] = ["start": String(first), "end": String(last)]
                wire["original_range"] = ["start": String(start + first), "end": String(start + last)]
                wire["text"] = String(decoding: bytes, as: UTF8.self)
                wire["original_hex"] = bracketHex(bytes); wire["contains_replacements"] = false
                return bracketJSON(wire)
            }), matchingDelimiter: { [self] owner, start, end in
                precondition(!Thread.isMainThread)
                let behavior = locked {
                    matches += 1
                    let behavior = (pauseMatch, mode)
                    pauseMatch = false
                    if behavior.0 { blocked += 1 }
                    return behavior
                }
                pause(behavior.0)
                defer { locked { matchReturns += 1 } }
                if behavior.1 == .sourceLimit {
                    return bracketJSON(["schema": "fcb.reader-session/1", "status": "error",
                        "owner": String(owner), "error": ["code": "DELIMITER_SOURCE_LIMIT"]])
                }
                let state: String
                switch behavior.1 {
                case .unmatched: state = "unmatched"
                case .mismatched: state = "mismatched"
                case .notCode: state = "not-code"
                default: state = "matched"
                }
                let mate = behavior.1 == .wrongMate ? UInt64(1) : (start == 0 ? UInt64(raw.count - 1) : 0)
                return bracketJSON(bracketReceipt(owner: behavior.1 == .wrongIdentity ? owner + 1 : owner,
                    length: UInt64(raw.count), start: start, end: end,
                    mate: state == "matched" ? (mate, mate + 1) : nil, state: state))
            })
    }
}

@MainActor @main struct AtlasReaderDelimiterTests {
    private static var checks = 0
    private static func check(_ value: @autoclosure () -> Bool, _ message: String) {
        checks += 1; precondition(value(), message)
    }
    private static func rejects(_ message: String, _ body: () throws -> Void) {
        do { try body(); fatalError(message) } catch { checks += 1 }
    }
    private static func wait(_ message: String, _ condition: () -> Bool) async {
        let deadline = Date().addingTimeInterval(5)
        while !condition(), Date() < deadline { try? await Task.sleep(for: .milliseconds(2)) }
        check(condition(), message)
    }
    private static func open(_ fixture: BracketFixture) async -> AtlasPagedReaderModel {
        let model = AtlasPagedReaderModel(transport: fixture.transport)
        model.open(root: "/project", path: "source", accessLease: nil)
        await wait("reader did not open") { !model.busy && model.page != nil }
        return model
    }
    private static func select(_ model: AtlasPagedReaderModel, _ range: NSRange) async -> UUID {
        model.updateTextSelection(source: model.source!, navigation: model.navigation, range: range)
        await wait("selection did not resolve") { model.textSelection != nil && !model.resolvingTextSelection }
        return model.textSelection!.id
    }

    static func main() async throws {
        try exactOriginalCoordinatesAndReceipts()
        try sourceLimitAdmission()
        await navigatesAcrossPagesAndBack()
        await navigatesBeyondFormerLimitAndPreservesAcceptedSelection()
        await refusalsPreserveSelectionAndHistory()
        await cancellationAndStaleDelivery()
        try importedCapabilitySurvives()
        print("AtlasReaderDelimiter: \(checks) production receipt, navigation and stale-delivery checks passed")
    }

    private static func exactOriginalCoordinatesAndReceipts() throws {
        for encoding in ["utf8", "utf16le", "utf16be"] {
            try exactOriginalCoordinatesAndReceipts(encoding: encoding, offset: 0)
            try exactOriginalCoordinatesAndReceipts(encoding: encoding, offset: 600_000)
        }
    }

    private static func exactOriginalCoordinatesAndReceipts(encoding: String, offset: UInt64) throws {
        let text = "(🦀)"
        let start: UInt64 = offset + (encoding == "utf8" ? 0 : 2)
        let raw = Array(encoding == "utf8" ? Data(text.utf8)
            : text.data(using: encoding == "utf16le" ? .utf16LittleEndian : .utf16BigEndian)!)
        let width: UInt64 = encoding == "utf8" ? 1 : 2
        let identity = AtlasReaderIdentity(owner: 2, file: 1, revision: 1,
            capturedBytes: start + UInt64(raw.count), pathBytes: Array("/project/source".utf8),
            encoding: encoding, displayPath: "/project/source")
        let page = AtlasReaderPage(identity: identity, start: start, end: identity.capturedBytes,
            text: text, originalHex: bracketHex(raw), firstPhysicalLine: nil,
            rangeLimited: false, boundariesAdjusted: start != 0, hasReplacements: false,
            highlight: nil, selection: nil, symbolSelection: nil)
        let selected = try bracketSelection(page: page, range: NSRange(location: 0, length: 1), start: start, end: start + width)
        let mate = page.end - width
        let receipt = bracketReceipt(length: page.end, encoding: encoding, start: start, end: start + width,
            mate: (mate, page.end))
        let result = try AtlasReaderDelimiterMatch.decode(bracketJSON(receipt), selection: selected)
        let destination = try result.selection(in: page)
        check(result.matchingRange?.start == mate && destination.nativeRange == NSRange(location: 3, length: 1),
              "original encoding width or supplementary scalar changed bracket destination")
        check(selected.matchingBracket == ")", "exact selected bracket was not eligible")
        let closing = try bracketSelection(page: page, range: NSRange(location: 3, length: 1), start: mate, end: page.end)
        let backward = try AtlasReaderDelimiterMatch.decode(bracketJSON(bracketReceipt(length: page.end,
            encoding: encoding, start: mate, end: page.end, mate: (start, start + width))), selection: closing)
        let backwardDestination = try backward.selection(in: page)
        check(backwardDestination.nativeRange == NSRange(location: 0, length: 1), "closing bracket could not navigate backward")

        for (key, value) in [
            ("owner", "3" as Any), ("source_revision", "2" as Any), ("command", "window" as Any),
            ("selected_range", ["start": String(mate), "end": String(page.end)] as Any),
            ("source_context", "visible-page" as Any), ("semantic_resolution", true as Any),
            ("evidence_level", "compiler-resolved" as Any), ("language", "unknown" as Any),
            ("matching_range", NSNull() as Any), ("delimiter_state", "unmatched" as Any),
            ("matching_range", ["start": String(mate), "end": String(page.end + 1)] as Any),
            ("matching_range", ["start": "0" + String(mate), "end": String(page.end)] as Any)
        ] {
            var corrupt = receipt; corrupt[key] = value
            rejects("unbound or inflated bracket receipt accepted: \(key)") {
                _ = try AtlasReaderDelimiterMatch.decode(bracketJSON(corrupt), selection: selected)
            }
        }
        var missing = receipt; missing.removeValue(forKey: "matching_range")
        rejects("missing nullable range accepted") { _ = try AtlasReaderDelimiterMatch.decode(bracketJSON(missing), selection: selected) }
        var reversed = receipt; reversed["matching_range"] = ["start": String(start), "end": String(start + width)]
        rejects("selected bracket substituted for its mate") { _ = try AtlasReaderDelimiterMatch.decode(bracketJSON(reversed), selection: selected) }
        let multiple = try bracketSelection(page: page, range: NSRange(location: 0, length: 4), start: start, end: page.end)
        check(multiple.matchingBracket == nil, "multiple selected scalars became one bracket")
    }

    private static func sourceLimitAdmission() throws {
        for capturedBytes in [UInt64(1_048_576), UInt64(1_048_577)] {
            let start = capturedBytes - 2
            let identity = AtlasReaderIdentity(owner: 2, file: 1, revision: 1,
                capturedBytes: capturedBytes, pathBytes: Array("/project/source".utf8),
                encoding: "utf8", displayPath: "/project/source")
            let page = AtlasReaderPage(identity: identity, start: start, end: capturedBytes,
                text: "()", originalHex: "2829", firstPhysicalLine: nil,
                rangeLimited: false, boundariesAdjusted: false, hasReplacements: false,
                highlight: nil, selection: nil, symbolSelection: nil)
            let selected = try bracketSelection(page: page, range: NSRange(location: 0, length: 1),
                start: start, end: start + 1)
            let receipt = bracketJSON(bracketReceipt(length: capturedBytes, start: start, end: start + 1,
                mate: (start + 1, capturedBytes)))
            if capturedBytes == 1_048_576 {
                let result = try AtlasReaderDelimiterMatch.decode(receipt, selection: selected)
                let destination = try result.selection(in: page)
                check(destination.nativeRange == NSRange(location: 1, length: 1),
                      "complete one-MiB capture was refused or changed its exact mate")
            } else {
                rejects("capture above one MiB admitted a complete bracket match") {
                    _ = try AtlasReaderDelimiterMatch.decode(receipt, selection: selected)
                }
            }
        }
    }

    private static func navigatesAcrossPagesAndBack() async {
        let fixture = BracketFixture(), model = await open(fixture)
        defer { model.close() }
        let owner = model.page!.identity.owner
        let selected = await select(model, NSRange(location: 0, length: 1))
        check(model.canGoToMatchingBracket && model.goToMatchingBracket(selectionID: selected), "resolved bracket action refused")
        await wait("far mate did not install") { !model.busy && model.textSelection?.originalStart == 70_001 }
        check(model.page?.start == 69_745 && model.nativeSelectionRange == NSRange(location: 256, length: 1)
              && model.canGoBack && model.notice.contains("lexical"), "mate did not navigate, select or retain history")
        check(model.page?.identity.owner == owner && fixture.calls.opened == 1, "bracket navigation reopened the live source")
        let closing = model.textSelection!.id
        check(!model.goToMatchingBracket(selectionID: selected), "retired menu borrowed the current bracket selection")
        check(model.goToMatchingBracket(selectionID: closing), "closing bracket action refused")
        await wait("opening bracket did not install") { !model.busy && model.textSelection?.originalStart == 0 }
        model.back()
        await wait("bracket history did not restore far source page") { !model.busy && model.page?.start == 69_745 }
        check(fixture.calls.opened == 1, "back navigation reread disk")
    }

    private static func navigatesBeyondFormerLimitAndPreservesAcceptedSelection() async {
        let fixture = BracketFixture(padding: 600_000), model = await open(fixture)
        defer { model.close() }
        let opening = await select(model, NSRange(location: 0, length: 1))
        check(model.goToMatchingBracket(selectionID: opening), "large-source bracket action refused")
        await wait("mate beyond the former source limit did not install") {
            !model.busy && model.textSelection?.originalStart == 600_001
        }
        check(model.nativeSelectionRange == NSRange(location: 256, length: 1)
              && model.textSelection?.originalEnd == 600_002 && model.textSelection?.originalHex == "29",
              "late mate lost its exact original bytes or native selection")
        let source = model.source!, navigation = model.navigation, closing = model.textSelection!.id
        let pageStart = model.page!.start
        check(model.canGoBack && fixture.calls.opened == 1, "large-source navigation lost retained history")

        for mode in [BracketFixture.Mode.wrongMate, .sourceLimit] {
            fixture.setMode(mode)
            check(model.goToMatchingBracket(selectionID: closing), "late selection could not request a result")
            await wait("late bracket refusal did not finish") { !model.busy }
            check(model.source === source && model.navigation == navigation && model.page?.start == pageStart
                  && model.textSelection?.id == closing && model.canGoBack,
                  "late refusal changed the accepted page, exact selection or history")
        }
        fixture.setMode(.matched); fixture.failWindow()
        check(model.goToMatchingBracket(selectionID: closing), "late destination-window refusal did not start")
        await wait("late destination-window refusal did not finish") { !model.busy }
        check(model.source === source && model.navigation == navigation && model.textSelection?.id == closing
              && model.page?.start == pageStart && model.canGoBack,
              "failed large-source destination committed partial navigation")

        fixture.blockMatch()
        let returned = fixture.calls.matchReturns
        check(model.goToMatchingBracket(selectionID: closing), "late canceled matching did not start")
        await wait("late matching did not block") { fixture.calls.blocked == 1 }
        model.cancel(); fixture.gate.signal()
        await wait("late canceled matching did not drain") { fixture.calls.matchReturns == returned + 1 }
        check(model.source === source && model.navigation == navigation && model.textSelection?.id == closing
              && model.page?.start == pageStart && model.canGoBack,
              "cancellation revoked the accepted late selection")

        check(model.goToMatchingBracket(selectionID: closing), "late closing bracket could not navigate backward")
        await wait("opening bracket did not return after a large-source refusal") {
            !model.busy && model.textSelection?.originalStart == 0
        }
        model.back()
        await wait("large-source history did not restore the late page") { !model.busy && model.page?.start == pageStart }
        check(fixture.calls.opened == 1, "late bracket navigation reopened the source")
    }

    private static func refusalsPreserveSelectionAndHistory() async {
        let fixture = BracketFixture(), model = await open(fixture)
        defer { model.close() }
        let selected = await select(model, NSRange(location: 0, length: 1))
        let source = model.source!, navigation = model.navigation
        for mode in [BracketFixture.Mode.unmatched, .mismatched, .notCode, .wrongIdentity, .wrongMate, .sourceLimit] {
            fixture.setMode(mode)
            check(model.goToMatchingBracket(selectionID: selected), "current bracket could not request a result")
            await wait("refused bracket navigation did not finish") { !model.busy }
            check(model.source === source && model.navigation == navigation && model.textSelection?.id == selected
                  && !model.canGoBack && !model.notice.isEmpty, "refusal changed source, selection or history")
        }
        check(model.notice.contains("1 MiB"), "bounded source refusal had no useful explanation")
        fixture.setMode(.matched); fixture.failWindow()
        check(model.goToMatchingBracket(selectionID: selected), "mate window refusal did not start")
        await wait("mate window refusal did not finish") { !model.busy }
        check(model.source === source && model.textSelection?.id == selected && !model.canGoBack,
              "failed destination read committed a partial bracket jump")
        _ = await select(model, NSRange(location: 1, length: 1))
        check(!model.canGoToMatchingBracket && !model.goToMatchingBracket(selectionID: selected), "non-bracket text admitted matching")
    }

    private static func cancellationAndStaleDelivery() async {
        let fixture = BracketFixture(), model = await open(fixture)
        defer { model.close() }
        let selected = await select(model, NSRange(location: 0, length: 1))
        let source = model.source!, navigation = model.navigation
        fixture.blockMatch()
        check(model.goToMatchingBracket(selectionID: selected), "blocked matching did not start")
        await wait("foreign matching did not block") { fixture.calls.blocked == 1 }
        model.cancel()
        check(model.source === source && model.textSelection?.id == selected && !model.canGoBack,
              "cancel revoked the unchanged exact selection")
        fixture.gate.signal()
        await wait("canceled matching did not drain") { fixture.calls.matchReturns == 1 }
        check(model.navigation == navigation && fixture.calls.windows == 1, "canceled match read or installed its destination")

        fixture.blockWindow()
        check(model.goToMatchingBracket(selectionID: selected), "blocked destination did not start")
        await wait("foreign destination did not block") { fixture.calls.blocked == 2 }
        model.updateTextSelection(source: source, navigation: navigation, range: NSRange(location: 1, length: 1))
        fixture.gate.signal()
        await wait("new selection did not supersede the destination") { model.textSelection?.text == "x" && !model.resolvingTextSelection }
        check(model.source === source && model.navigation == navigation && !model.canGoBack,
              "late destination replaced a newer selection")

        let latest = await select(model, NSRange(location: 0, length: 1))
        fixture.blockMatch()
        check(model.goToMatchingBracket(selectionID: latest), "close-case matching did not start")
        await wait("close-case matching did not block") { fixture.calls.blocked == 3 }
        model.close(); fixture.gate.signal()
        await wait("closed matching did not drain") { fixture.calls.matchReturns == 3 }
        check(model.page == nil && model.textSelection == nil && !model.canGoBack, "late matching revived a closed reader")
    }

    private static func importedCapabilitySurvives() throws {
        let fixture = BracketFixture()
        let supplied = try AtlasSuppliedReaderSource(root: "/project", path: "source", text: "()")
        check(fixture.transport.importing(supplied).matchingDelimiter != nil,
              "supplied-source adapter dropped retained bracket navigation")
    }
}
