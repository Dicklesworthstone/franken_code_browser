import Foundation
import Dispatch

/// Exercises the production selection coordinate/receipt decoder, coordinator
/// and model. Only the foreign boundary is injected; the real retained decoder
/// and C ABI have separate Rust integration tests. No native qualification or
/// clipboard publication is claimed by this portable suite.
private func selectionHex(_ bytes: some Sequence<UInt8>) -> String {
    bytes.map { String(format: "%02x", $0) }.joined()
}
private func selectionJSON(_ value: [String: Any]) -> String {
    String(decoding: try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self)
}
private func selectionHeader(owner: UInt64 = 2, length: UInt64, encoding: String = "utf8",
                             command: String = "resolve-selection") -> [String: Any] {
    ["schema": "fcb.reader-session/1", "status": "ok", "command": command,
     "owner": String(owner), "file_id": "1", "source_revision": "1",
     "captured_bytes": String(length), "encoding": encoding,
     "path": ["encoding": "unix-bytes", "hex": selectionHex("/project/source".utf8), "display": "/project/source"],
     "additional_source_bytes_read": "0", "native_presented": false]
}
private func selectionPage(text: String, raw: [UInt8], encoding: String = "utf8",
                           start: UInt64 = 0, replacements: Bool = false,
                           boundariesAdjusted: Bool = false) -> AtlasReaderPage {
    let identity = AtlasReaderIdentity(owner: 2, file: 1, revision: 1,
        capturedBytes: start + UInt64(raw.count), pathBytes: Array("/project/source".utf8),
        encoding: encoding, displayPath: "/project/source")
    return AtlasReaderPage(identity: identity, start: start, end: start + UInt64(raw.count),
        text: text, originalHex: selectionHex(raw), firstPhysicalLine: nil,
        rangeLimited: false, boundariesAdjusted: boundariesAdjusted, hasReplacements: replacements,
        highlight: nil, selection: nil, symbolSelection: nil)
}
private func selectionReceipt(_ request: AtlasReaderTextSelectionRequest,
                              originalStart: UInt64, originalEnd: UInt64,
                              originalHex: String, replacements: Bool = false) -> [String: Any] {
    let page = request.page
    var value = selectionHeader(owner: page.identity.owner, length: page.identity.capturedBytes,
        encoding: page.identity.encoding)
    value["selection_namespace"] = "native-text"
    value["window_original_range"] = ["start": String(page.start), "end": String(page.end)]
    value["window_utf8_range"] = ["start": String(request.utf8Start), "end": String(request.utf8End)]
    value["original_range"] = ["start": String(originalStart), "end": String(originalEnd)]
    value["text"] = request.text
    value["original_hex"] = originalHex
    value["contains_replacements"] = replacements
    return value
}

private final class TextSelectionFixture: @unchecked Sendable {
    private let lock = NSLock()
    private var next: UInt64 = 2
    private var block = false
    private var selected = 0
    private var opened = 0
    private var blocked = 0
    private var corrupt = false
    private var completed = 0
    private var searched: [String] = []
    let gate = DispatchSemaphore(value: 0)
    let raw = Array("alpha beta gamma\r\n".utf8)

    private func locked<Value>(_ body: () -> Value) -> Value { lock.lock(); defer { lock.unlock() }; return body() }
    func blockNext() { locked { block = true } }
    func corruptNext() { locked { corrupt = true } }
    var calls: (selected: Int, opened: Int, blocked: Int, completed: Int) { locked { (selected, opened, blocked, completed) } }
    var searchCalls: [String] { locked { searched } }

    /// The foreign search is deliberately refused. The test observes the real
    /// coordinator's worker handoff and failure/rebinding lifecycle, without
    /// pretending this fixture implements source matching.
    var refusingSearch: AtlasReaderSearchTransport {
        AtlasReaderSearchTransport(find: { [self] _, _, text, _, _ in
            precondition(!Thread.isMainThread)
            locked { searched.append(text) }
            return nil
        }, hit: { _, _, _, _ in nil })
    }

    var transport: AtlasReaderTransport {
        AtlasReaderTransport(create: { [self] in locked { let id = next; next += 1; return id } },
            open: { [self] owner, _, _ in
                precondition(!Thread.isMainThread)
                locked { opened += 1 }
                var value = selectionHeader(owner: owner, length: UInt64(raw.count), command: "info")
                value["capture_origin"] = "regular-file-observation-not-atomic"
                value["capture_sha256"] = String(repeating: "a", count: 64)
                return selectionJSON(value)
            }, window: { [self] owner, offset, limit in
                precondition(!Thread.isMainThread)
                let end = min(UInt64(raw.count), offset + limit)
                let bytes = raw[Int(offset)..<Int(end)]
                var value = selectionHeader(owner: owner, length: UInt64(raw.count), command: "window")
                value["text_kind"] = "logical-captured-text-not-shaped"
                value["requested_original_range"] = ["start": String(offset), "end": String(end)]
                value["visible_range"] = ["start": String(offset), "end": String(end)]
                value["range_limited"] = false; value["boundaries_adjusted"] = false
                value["has_replacements"] = false; value["first_physical_line"] = NSNull()
                value["selection"] = NSNull()
                value["text"] = String(decoding: bytes, as: UTF8.self); value["original_hex"] = selectionHex(bytes)
                value["next_offset"] = end < UInt64(raw.count) ? String(end) as Any : NSNull()
                return selectionJSON(value)
            }, lines: { _, _, _, _ in nil }, cancel: { _ in true }, close: { _ in true },
            retirementFailed: {}, fork: { _, _ in nil },
            textSelection: AtlasReaderTextSelectionTransport(resolve: { [self] owner, start, end, first, last in
                precondition(!Thread.isMainThread)
                let pause = locked {
                    selected += 1
                    let pause = block; block = false
                    if pause { blocked += 1 }
                    return pause
                }
                if pause { precondition(gate.wait(timeout: .now() + .seconds(5)) == .success) }
                let bytes = raw[Int(start + first)..<Int(start + last)]
                var value = selectionHeader(owner: owner, length: UInt64(raw.count))
                value["selection_namespace"] = "native-text"
                value["window_original_range"] = ["start": String(start), "end": String(end)]
                value["window_utf8_range"] = ["start": String(first), "end": String(last)]
                value["original_range"] = ["start": String(start + first), "end": String(start + last)]
                value["text"] = String(decoding: bytes, as: UTF8.self)
                value["original_hex"] = selectionHex(bytes); value["contains_replacements"] = false
                if locked({ let current = corrupt; corrupt = false; return current }) { value["owner"] = String(owner + 1) }
                locked { completed += 1 }
                return selectionJSON(value)
            }))
    }
}

@MainActor @main struct AtlasReaderTextSelectionTests {
    private static var checks = 0
    private static func check(_ value: @autoclosure () -> Bool, _ message: String) {
        checks += 1; precondition(value(), message)
    }
    private static func rejects(_ message: String, _ body: () throws -> Void) {
        do { try body(); fatalError(message) } catch { checks += 1 }
    }
    private static func wait(_ message: String, _ condition: () -> Bool) async {
        let deadline = Date().addingTimeInterval(5)
        while !condition(), Date() < deadline {
            try? await Task.sleep(for: .milliseconds(2))
        }
        check(condition(), message)
    }
    private static func opened(_ fixture: TextSelectionFixture) async -> AtlasPagedReaderModel {
        let model = AtlasPagedReaderModel(transport: fixture.transport)
        model.open(root: "/project", path: "source", accessLease: nil)
        await wait("reader did not open") { !model.busy && model.page != nil }
        return model
    }
    private static func select(_ model: AtlasPagedReaderModel, _ range: NSRange?) {
        model.updateTextSelection(source: model.source!, navigation: model.navigation, range: range)
    }

    static func main() async throws {
        try unicodeCoordinatesAndProvenance()
        try utf16AndReplacementWitnesses()
        try malformedCoordinatesAndReceipts()
        try boundaryAdjustedPages()
        try exactSelectionSearchSeeds()
        await latestSelectionCoalesces()
        await stalePagesAndClosureRevoke()
        await failureCannotReuseSelection()
        await failedNavigationRebindsVisibleSelection()
        await selectionSearchRejectsRetiredMenus()
        try suppliedTransportPreservesSelection()
        print("AtlasReaderTextSelection: \(checks) production coordinate, receipt, coalescing and lifetime checks passed")
    }

    private static func unicodeCoordinatesAndProvenance() throws {
        let text = "a😀e\u{301}\r\n\u{202e}z\0"
        let page = selectionPage(text: text, raw: Array(text.utf8), start: 91)
        let range = (text as NSString).range(of: "😀e\u{301}\r\n")
        let request = try AtlasReaderTextSelectionRequest(page: page, nativeRange: range)
        check(request.utf8Start == 1 && request.utf8End == 10, "UTF-16 offsets were cast to byte offsets")
        let raw = Array(text.utf8)[1..<10]
        let selection = try AtlasReaderTextSelection.decode(selectionJSON(selectionReceipt(request,
            originalStart: 92, originalEnd: 101, originalHex: selectionHex(raw))),
            request: request, captureSHA256: String(repeating: "b", count: 64))
        check(selection.text.utf8.elementsEqual("😀e\u{301}\r\n".utf8), "source order or CRLF changed")
        check(selection.originalStart == 92 && selection.originalEnd == 101, "page offset lost")
        check(selection.withLocationText.contains("\"capture_sha256\":\"" + String(repeating: "b", count: 64)),
              "copy provenance omitted retained capture witness")
        check(selection.withLocationText.hasSuffix("\n\n😀e\u{301}\r\n"), "location copy normalized text")
        let bidiRange = (text as NSString).range(of: "\u{202e}z\0")
        let bidi = try AtlasReaderTextSelectionRequest(page: page, nativeRange: bidiRange)
        check(bidi.text.utf8.elementsEqual("\u{202e}z\0".utf8), "bidi or NUL was reordered or removed")
    }

    private static func utf16AndReplacementWitnesses() throws {
        for (encoding, bytes) in [
            ("utf16le", [UInt8(255), 254, 61, 216, 0, 222, 13, 0, 10, 0]),
            ("utf16be", [UInt8(254), 255, 216, 61, 222, 0, 0, 13, 0, 10])
        ] {
            let page = selectionPage(text: "😀\r\n", raw: Array(bytes.dropFirst(2)), encoding: encoding,
                start: 2, boundariesAdjusted: true)
            let request = try AtlasReaderTextSelectionRequest(page: page, nativeRange: NSRange(location: 0, length: 2))
            let selection = try AtlasReaderTextSelection.decode(selectionJSON(selectionReceipt(request,
                originalStart: 2, originalEnd: 6, originalHex: selectionHex(bytes[2..<6]))), request: request)
            check(selection.text == "😀" && selection.originalStart == 2 && selection.originalEnd == 6,
                  "UTF-16 BOM or surrogate-pair original range lost")
        }
        let malformed = selectionPage(text: "a�b", raw: [97, 255, 98], replacements: true)
        let request = try AtlasReaderTextSelectionRequest(page: malformed, nativeRange: NSRange(location: 1, length: 1))
        let selection = try AtlasReaderTextSelection.decode(selectionJSON(selectionReceipt(request,
            originalStart: 1, originalEnd: 2, originalHex: "ff", replacements: true)), request: request)
        check(selection.text == "�" && selection.originalHex == "ff" && selection.containsReplacements,
              "replacement was relabeled as original Unicode bytes")
        check(selection.searchText(maximumUTF8Bytes: 1024) == nil,
              "malformed source became an apparently exact text query")
        let literal = selectionPage(text: "�", raw: [239, 191, 189])
        let literalRequest = try AtlasReaderTextSelectionRequest(page: literal, nativeRange: NSRange(location: 0, length: 1))
        let exact = try AtlasReaderTextSelection.decode(selectionJSON(selectionReceipt(literalRequest,
            originalStart: 0, originalEnd: 3, originalHex: "efbfbd")), request: literalRequest)
        check(!exact.containsReplacements, "genuine replacement-character scalar labeled malformed")
        check(exact.searchText(maximumUTF8Bytes: 1024) == "�",
              "genuine U+FFFD was refused as a decoding replacement")
        var hiddenReplacement = selectionReceipt(request, originalStart: 1, originalEnd: 2, originalHex: "ff")
        rejects("malformed byte advertised as lossless Unicode") {
            _ = try AtlasReaderTextSelection.decode(selectionJSON(hiddenReplacement), request: request)
        }
        let mixed = selectionPage(text: "� �", raw: [255, 32, 239, 191, 189], replacements: true)
        let mixedRequest = try AtlasReaderTextSelectionRequest(page: mixed, nativeRange: NSRange(location: 2, length: 1))
        hiddenReplacement = selectionReceipt(mixedRequest, originalStart: 2, originalEnd: 5,
            originalHex: "efbfbd", replacements: true)
        rejects("genuine U+FFFD advertised as a decoding replacement") {
            _ = try AtlasReaderTextSelection.decode(selectionJSON(hiddenReplacement), request: mixedRequest)
        }
    }

    private static func malformedCoordinatesAndReceipts() throws {
        let text = "a😀e\u{301}\r\nz", page = selectionPage(text: "a😀e\u{301}\r\nz", raw: Array("a😀e\u{301}\r\nz".utf8))
        for range in [NSRange(location: NSNotFound, length: 1), NSRange(location: -1, length: 1),
                      NSRange(location: 0, length: 0), NSRange(location: 0, length: Int.max),
                      NSRange(location: 2, length: 1), NSRange(location: 4, length: 1),
                      NSRange(location: 5, length: 1)] {
            rejects("invalid or partial native range admitted") { _ = try AtlasReaderTextSelectionRequest(page: page, nativeRange: range) }
        }
        let request = try AtlasReaderTextSelectionRequest(page: page, nativeRange: (text as NSString).range(of: "😀"))
        let receipt = selectionReceipt(request, originalStart: 1, originalEnd: 5, originalHex: "f09f9880")
        for (key, value) in [("owner", "3" as Any), ("source_revision", "2" as Any),
                             ("selection_namespace", "query" as Any), ("command", "window" as Any),
                             ("original_hex", "f09f9881" as Any), ("text", "😇" as Any),
                             ("additional_source_bytes_read", "1" as Any), ("native_presented", true as Any),
                             ("contains_replacements", true as Any),
                             ("window_original_range", ["start": "1", "end": String(page.end)] as Any),
                             ("window_utf8_range", ["start": "0", "end": "4"] as Any),
                             ("original_range", ["start": "1", "end": String(page.end + 1)] as Any)] {
            var bad = receipt; bad[key] = value
            rejects("corrupt selection receipt accepted: " + key) {
                _ = try AtlasReaderTextSelection.decode(selectionJSON(bad), request: request)
            }
        }
        let duplicatePage = selectionPage(text: "same same", raw: Array("same same".utf8))
        let first = try AtlasReaderTextSelectionRequest(page: duplicatePage, nativeRange: NSRange(location: 0, length: 4))
        let swapped = selectionReceipt(first, originalStart: 5, originalEnd: 9, originalHex: selectionHex("same".utf8))
        rejects("equal token at different original offsets substituted") {
            _ = try AtlasReaderTextSelection.decode(selectionJSON(swapped), request: first)
        }
        let decomposed = try AtlasReaderTextSelectionRequest(page: page, nativeRange: (text as NSString).range(of: "e\u{301}"))
        var canonical = selectionReceipt(decomposed, originalStart: 5, originalEnd: 8, originalHex: "65cc81")
        canonical["text"] = "é"
        rejects("canonical-equivalent source spelling substituted") {
            _ = try AtlasReaderTextSelection.decode(selectionJSON(canonical), request: decomposed)
        }
    }

    private static func boundaryAdjustedPages() throws {
        // A window requested at byte 3, inside the leading four-byte scalar,
        // with the maximum byte budget expands backward to byte 0.
        let bytes = Array("😀".utf8) + [UInt8](repeating: 97, count: Int(AtlasReaderLimits.maximumPageBytes - 1))
        let text = String(decoding: bytes, as: UTF8.self)
        let page = selectionPage(text: text, raw: bytes, boundariesAdjusted: true)
        let request = try AtlasReaderTextSelectionRequest(page: page,
            nativeRange: NSRange(location: text.utf16.count - 1, length: 1))
        let value = try AtlasReaderTextSelection.decode(selectionJSON(selectionReceipt(request,
            originalStart: page.end - 1, originalEnd: page.end, originalHex: "61")), request: request)
        check(value.originalEnd == page.end, "valid adjusted page edge refused")
        let excess = [UInt8](repeating: 97, count: Int(AtlasReaderLimits.maximumPageBytes + 17))
        let oversized = selectionPage(text: String(decoding: excess, as: UTF8.self), raw: excess)
        rejects("oversized page entered selection worker") {
            _ = try AtlasReaderTextSelectionRequest(page: oversized, nativeRange: NSRange(location: 0, length: 1))
        }
    }

    private static func exactSelectionSearchSeeds() throws {
        func selected(_ text: String) throws -> AtlasReaderTextSelection {
            let raw = Array(text.utf8), page = selectionPage(text: text, raw: Array(text.utf8), start: 17)
            let request = try AtlasReaderTextSelectionRequest(page: page,
                nativeRange: NSRange(location: 0, length: (text as NSString).length))
            return try AtlasReaderTextSelection.decode(selectionJSON(selectionReceipt(request,
                originalStart: 17, originalEnd: 17 + UInt64(raw.count), originalHex: selectionHex(raw))), request: request)
        }
        for text in ["café", "cafe\u{301}", "  alpha\r\nbeta\t", "path:src/ -unsafe", "😀"] {
            let selection = try selected(text)
            for mode in [AtlasSearchMode.text, .symbols, .occurrences] {
                let query = try AtlasReaderSelectionQuery(selection: selection, mode: mode)
                check(query.text.utf8.elementsEqual(text.utf8), "selection search trimmed, normalized or parsed the query")
                check(query.selectionID == selection.id && query.identity.matches(selection.identity)
                      && query.originalStart == 17 && query.originalEnd == 17 + UInt64(text.utf8.count),
                      "selection search changed the originating source witness")
                check(query.mode == mode, "selected literal silently became an expression")
            }
        }
        let simple = try selected("alpha")
        for unsupported in [AtlasSearchMode.expression, .headings] {
            rejects("selection silently opted into unrelated query semantics") {
                _ = try AtlasReaderSelectionQuery(selection: simple, mode: unsupported)
            }
        }
        let atProjectLimit = try selected(String(repeating: "é", count: 512))
        let atLimitQuery = try AtlasReaderSelectionQuery(selection: atProjectLimit, mode: .text)
        check(atLimitQuery.text.utf8.count == 1024,
              "project selection limit measured characters instead of UTF-8 bytes")
        let overProjectLimit = try selected(String(repeating: "é", count: 513))
        rejects("oversized project selection silently shortened") {
            _ = try AtlasReaderSelectionQuery(selection: overProjectLimit, mode: .text)
        }
        check(overProjectLimit.searchText(maximumUTF8Bytes: AtlasReaderFindReport.maximumNeedleBytes) != nil,
              "project query limit disabled the larger retained-file Find route")
        let overFileLimit = try selected(String(repeating: "a", count: AtlasReaderFindReport.maximumNeedleBytes + 1))
        check(overFileLimit.searchText(maximumUTF8Bytes: AtlasReaderFindReport.maximumNeedleBytes) == nil,
              "oversized file selection silently shortened")
        let nul = try selected("alpha\0beta")
        check(nul.searchText(maximumUTF8Bytes: 4096) == nil, "selection query could be truncated at the C boundary")
    }

    private static func latestSelectionCoalesces() async {
        let fixture = TextSelectionFixture()
        let active = await opened(fixture)
        defer { active.close() }
        fixture.blockNext()
        select(active, NSRange(location: 0, length: 5))
        await wait("selection did not enter worker") { fixture.calls.blocked == 1 }
        for i in 0..<1000 { select(active, NSRange(location: i % 10, length: 1)) }
        select(active, NSRange(location: 6, length: 4))
        check(active.hasTextSelectionIntent && active.textSelection == nil && !active.canKeep,
              "pending selection reused old bytes or allowed page fallback")
        check(fixture.calls.selected == 1, "drag generated unbounded foreign requests")
        fixture.gate.signal()
        await wait("newest selection did not resolve") { !active.resolvingTextSelection && active.textSelection != nil }
        check(active.textSelection?.text == "beta" && active.textSelection?.originalStart == 6,
              "older selection replaced newest intent")
        check(fixture.calls.selected == 2 && active.canKeep, "coalesced selection not usable")
        let calls = fixture.calls.selected
        select(active, NSRange(location: NSNotFound, length: 1))
        await wait("invalid range did not refuse") { !active.resolvingTextSelection }
        check(active.hasTextSelectionIntent && active.textSelection == nil && !active.canKeep,
              "refused selection fell back to page Keep")
        check(fixture.calls.selected == calls, "invalid coordinates crossed the foreign boundary")
        select(active, nil)
        check(!active.hasTextSelectionIntent && active.canKeep, "clearing text selection did not restore page actions")
    }

    private static func stalePagesAndClosureRevoke() async {
        let fixture = TextSelectionFixture(), model = await opened(fixture)
        let oldSource = model.source!, oldNavigation = model.navigation
        fixture.blockNext()
        select(model, NSRange(location: 0, length: 5))
        await wait("blocked selection did not start") { fixture.calls.blocked == 1 }
        model.offsetInput = "6"; model.goToByte()
        check(model.textSelection == nil && model.hasTextSelectionIntent, "pending navigation lost visible selection intent")
        fixture.gate.signal()
        await wait("new page did not install") { !model.busy && model.page?.start == 6 }
        model.updateTextSelection(source: oldSource, navigation: oldNavigation, range: NSRange(location: 0, length: 5))
        check(!model.hasTextSelectionIntent && model.textSelection == nil, "stale source callback selected replacement page")
        select(model, NSRange(location: 0, length: 4))
        await wait("new page selection did not resolve") { model.textSelection != nil }
        check(model.textSelection?.originalStart == 6, "new page native offset lacked original base")
        let priorOwner = model.page!.identity.owner
        model.refreshFromDisk()
        check(model.textSelection == nil, "refresh retained old text-selection authority")
        await wait("refresh did not install") { !model.busy && model.page?.identity.owner != priorOwner }
        fixture.blockNext()
        select(model, NSRange(location: 0, length: 5))
        await wait("close-case selection did not start") { fixture.calls.blocked == 2 }
        model.close()
        fixture.gate.signal()
        await wait("closed selection did not drain") { fixture.calls.completed == 3 }
        try? await Task.sleep(for: .milliseconds(10))
        check(model.page == nil && model.textSelection == nil && !model.hasTextSelectionIntent,
              "close retained selected source")
    }

    private static func failureCannotReuseSelection() async {
        let fixture = TextSelectionFixture(), model = await opened(fixture)
        defer { model.close() }
        select(model, NSRange(location: 0, length: 5))
        await wait("initial selection did not resolve") { model.textSelection != nil }
        fixture.corruptNext()
        select(model, NSRange(location: 6, length: 4))
        await wait("invalid receipt was not handled") { !model.resolvingTextSelection }
        check(model.textSelection == nil && model.hasTextSelectionIntent && !model.canKeep,
              "invalid receipt reused preceding selected range")
        check(model.source?.text == "alpha beta gamma\r\n" && !model.textSelectionNotice.isEmpty,
              "selection failure replaced source or disappeared silently")
        model.retryTextSelection()
        await wait("explicit retry did not resolve retained descriptor") { model.textSelection != nil }
        check(model.textSelection?.text == "beta", "retry changed selection or reopened source")
        let opens = fixture.calls.opened
        model.cancel()
        check(model.hasTextSelectionIntent && model.textSelection == nil, "cancel converted visible selection into page fallback")
        await wait("canceled query did not rebind visible selection") { model.textSelection != nil }
        check(model.textSelection?.text == "beta" && fixture.calls.opened == opens, "cancel retry changed retained source")
    }

    private static func failedNavigationRebindsVisibleSelection() async {
        let fixture = TextSelectionFixture(), model = await opened(fixture)
        defer { model.close() }
        select(model, NSRange(location: 6, length: 4))
        await wait("selection before failed navigation did not resolve") { model.textSelection != nil }
        let source = model.source!, oldReceipt = model.textSelection!.id
        model.goToLine() // The injected foreign boundary refuses this operation.
        check(model.hasTextSelectionIntent && model.textSelection == nil && !model.canKeep,
              "navigation intent was promoted into a page-wide Keep")
        await wait("failed navigation did not restore exact visible selection") {
            !model.busy && model.textSelection != nil
        }
        check(model.source === source && model.textSelection?.id != oldReceipt
              && model.textSelection?.text == "beta" && model.canKeep,
              "failed navigation lost the displayed selection or reused its revoked receipt")
    }

    private static func selectionSearchRejectsRetiredMenus() async {
        let fixture = TextSelectionFixture()
        let model = AtlasPagedReaderModel(transport: fixture.transport, search: fixture.refusingSearch)
        model.open(root: "/project", path: "source", accessLease: nil)
        await wait("selection-search reader did not open") { !model.busy && model.page != nil }
        defer { model.close() }
        model.fileQuery = "unchanged"
        select(model, NSRange(location: 0, length: 5))
        await wait("first menu selection did not resolve") { model.textSelection != nil }
        let first = model.textSelection!.id
        check(model.selectionQuery(.symbols, selectionID: first)?.text == "alpha",
              "accepted source selection did not seed declaration lookup")
        fixture.blockNext()
        select(model, NSRange(location: 6, length: 4))
        await wait("replacement selection did not start") { fixture.calls.blocked == 1 }
        check(model.selectionQuery(.text, selectionID: first) == nil
              && !model.findSelectedText(selectionID: first) && model.fileQuery == "unchanged",
              "retained menu activated a superseded selection while its replacement was pending")
        fixture.gate.signal()
        await wait("replacement selection did not resolve") { model.textSelection != nil }
        let second = model.textSelection!.id
        check(model.selectionQuery(.occurrences, selectionID: second)?.originalStart == 6
              && model.selectionQuery(.occurrences, selectionID: first) == nil,
              "an old menu borrowed the new selection receipt")
        check(model.selectionQuery(.expression, selectionID: second) == nil,
              "selected text opted into expression parsing")
        let source = model.source!, navigation = model.navigation, opens = fixture.calls.opened
        check(model.findSelectedText(selectionID: second), "current selected text did not start retained-file Find")
        check(model.fileQuery == "beta" && model.findOptions.caseSensitive
              && model.selectionQuery(.text, selectionID: second) == nil,
              "Find changed the spelling or reused a selection during reader work")
        await wait("refused search did not drain and rebind the displayed selection") {
            !model.busy && fixture.searchCalls.count == 1 && model.textSelection != nil
        }
        check(fixture.searchCalls == ["beta"] && fixture.calls.opened == opens
              && model.source === source && model.navigation == navigation,
              "selection Find reread disk or replaced the retained source/navigation")
        check(model.textSelection?.id != second && model.selectionQuery(.text, selectionID: second) == nil,
              "failed search revived a retired menu receipt")
        let rebound = model.textSelection!.id
        model.offsetInput = "0"; model.goToByte()
        check(model.selectionQuery(.symbols, selectionID: rebound) == nil,
              "navigation accepted a previous selection despite identical page text")
        await wait("same-text navigation did not complete") { !model.busy }
        check(model.selectionQuery(.symbols, selectionID: rebound) == nil,
              "same-text page replacement revived an old selection")
        model.close()
        check(!model.findSelectedText(selectionID: rebound) && fixture.calls.opened == opens,
              "closed selection could start a query or reopen a pathname")
    }

    private static func suppliedTransportPreservesSelection() throws {
        let fixture = TextSelectionFixture()
        let source = try AtlasSuppliedReaderSource(root: "/project", path: "source", text: "alpha")
        check(fixture.transport.importing(source).textSelection != nil, "supplied-source wrapper dropped selection capability")
    }
}
