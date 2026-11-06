// Production position decoder/coordinator/model regressions. The transport
// supplies fixed source/coordinate witnesses, not a second column resolver.
// Invoke through strict RCH at the reviewed revision. Portable tests do not
// qualify Rust or native layout; the AppKit cases additionally require a Mac.
import Foundation
import Dispatch
#if canImport(AppKit)
import AppKit
#endif

private let sample = "x\r\nA🦀e\u{301}\tZ\r\nlast"
private func hex(_ bytes: some Sequence<UInt8>) -> String {
    bytes.map { String(format: "%02x", $0) }.joined()
}
private func json(_ object: [String: Any]) -> String {
    String(data: try! JSONSerialization.data(withJSONObject: object), encoding: .utf8)!
}
private func header(owner: UInt64 = 2, length: UInt64, encoding: String = "utf8",
                    origin: String = "regular-file-observation-not-atomic") -> [String: Any] {
    ["schema": "fcb.reader-session/1", "status": "ok", "command": "info",
     "owner": String(owner), "file_id": "1", "source_revision": "1",
     "path": ["encoding": "unix-bytes", "hex": hex("/project/file".utf8), "display": "/project/file"],
     "captured_bytes": String(length), "encoding": encoding, "capture_origin": origin,
     "additional_source_bytes_read": "0", "native_presented": false]
}
private func wholePage(owner: UInt64 = 2, raw: [UInt8], text: String, encoding: String = "utf8",
                       bomBytes: Int = 0, replacements: Bool = false,
                       origin: String = "regular-file-observation-not-atomic") -> [String: Any] {
    var wire = header(owner: owner, length: UInt64(raw.count), encoding: encoding, origin: origin)
    wire["command"] = "window"
    wire["text_kind"] = "logical-captured-text-not-shaped"
    wire["requested_original_range"] = ["start": "0", "end": String(raw.count)]
    wire["visible_range"] = ["start": String(bomBytes), "end": String(raw.count)]
    wire["range_limited"] = false; wire["boundaries_adjusted"] = bomBytes != 0
    wire["has_replacements"] = replacements
    wire["text"] = text; wire["original_hex"] = hex(raw.dropFirst(bomBytes))
    wire["first_physical_line"] = NSNull(); wire["next_offset"] = NSNull(); wire["selection"] = NSNull()
    return wire
}
private func positioned(_ page: [String: Any], line: UInt64 = 2, column: UInt64 = 3,
                        kind: AtlasReaderColumnKind = .unicodeScalar,
                        original: UInt64 = 8, utf8: UInt64 = 8) -> [String: Any] {
    var wire = page
    wire["command"] = "position"
    wire["position"] = ["line": String(line), "column": String(column), "column_kind": kind.wire,
                        "original_offset": String(original), "window_utf8_offset": String(utf8)]
    return wire
}
private func utf16(_ text: String, littleEndian: Bool) -> [UInt8] {
    var bytes: [UInt8] = littleEndian ? [255, 254] : [254, 255]
    for unit in text.utf16 {
        let low = UInt8(truncatingIfNeeded: unit), high = UInt8(truncatingIfNeeded: unit >> 8)
        bytes += littleEndian ? [low, high] : [high, low]
    }
    return bytes
}

private final class PositionDriver: @unchecked Sendable {
    private let lock = NSLock()
    private var next: UInt64 = 1
    private var live = sample
    private var captures: [UInt64: String] = [:]
    private var forked: Set<UInt64> = []
    private var calls: [String] = []
    private var blockPosition = false
    private var corruptPosition = false
    private var completed = 0
    let entered = DispatchSemaphore(value: 0)
    let gate = DispatchSemaphore(value: 0)

    var log: [String] { lock.lock(); defer { lock.unlock() }; return calls }
    var returns: Int { lock.lock(); defer { lock.unlock() }; return completed }
    func block() { lock.lock(); blockPosition = true; lock.unlock() }
    func corrupt() { lock.lock(); corruptPosition = true; lock.unlock() }
    func changeLive() { lock.lock(); live = "replacement\n"; lock.unlock() }
    private func create() -> UInt64 {
        lock.lock(); defer { lock.unlock() }
        let handle = next; next += 1; calls.append("create:\(handle)"); return handle
    }
    private func open(_ owner: UInt64) -> String {
        precondition(!Thread.isMainThread)
        lock.lock(); defer { lock.unlock() }
        captures[owner] = live; calls.append("open:\(owner)")
        return json(header(owner: owner, length: UInt64(live.utf8.count)))
    }
    private func window(_ owner: UInt64, offset: UInt64) -> String? {
        precondition(!Thread.isMainThread)
        lock.lock(); defer { lock.unlock() }
        guard let text = captures[owner], offset == 0 else { return nil }
        calls.append("window:\(owner)")
        return json(wholePage(owner: owner, raw: Array(text.utf8), text: text,
            origin: forked.contains(owner) ? "host-supplied" : "regular-file-observation-not-atomic"))
    }
    private func position(_ owner: UInt64, line: UInt64, column: UInt64, kind: UInt64, bytes: UInt64) -> String? {
        precondition(!Thread.isMainThread && bytes == AtlasReaderLimits.pageBytes)
        lock.lock()
        let text = captures[owner], paused = blockPosition, corrupted = corruptPosition
        let origin = forked.contains(owner) ? "host-supplied" : "regular-file-observation-not-atomic"
        blockPosition = false; corruptPosition = false
        calls.append("position:\(owner):\(line):\(column):\(kind)")
        lock.unlock()
        if paused {
            entered.signal()
            precondition(gate.wait(timeout: .now() + .seconds(5)) == .success)
        }
        defer { lock.lock(); completed += 1; lock.unlock() }
        guard let text else { return nil }
        // Fixed independent witnesses: scalar 3 / UTF-16 4 / UTF-8 6 all name
        // the 'e'; scalar 4 is its combining mark; scalar 7 is before CRLF.
        let offset: UInt64?
        if text == sample && line == 2 {
            if (kind == 1 && column == 3) || (kind == 2 && column == 4) || (kind == 3 && column == 6) { offset = 8 }
            else if kind == 1 && column == 4 { offset = 9 }
            else if kind == 1 && column == 7 { offset = 13 }
            else { offset = nil }
        } else { offset = nil }
        guard let offset, let unit = AtlasReaderColumnKind.allCases.first(where: { $0.abi == kind }) else {
            return json(["schema": "fcb.reader-session/1", "status": "error", "owner": String(owner),
                         "error": ["code": "READER_SESSION_MISSING_COLUMN"]])
        }
        return json(positioned(wholePage(owner: owner, raw: Array(text.utf8), text: text, origin: origin),
            line: line, column: column, kind: unit, original: offset, utf8: corrupted ? 0 : offset))
    }
    private func fork(_ source: UInt64, _ destination: UInt64) -> String? {
        precondition(!Thread.isMainThread)
        lock.lock(); defer { lock.unlock() }
        guard let text = captures[source] else { return nil }
        captures[destination] = text; forked.insert(destination); calls.append("fork:\(source):\(destination)")
        return json(header(owner: destination, length: UInt64(text.utf8.count), origin: "host-supplied"))
    }
    func transport(positions: Bool = true) -> AtlasReaderTransport {
        let resolve: (@Sendable (UInt64, UInt64, UInt64, UInt64, UInt64) -> String?)?
        if positions { resolve = { [self] in position($0, line: $1, column: $2, kind: $3, bytes: $4) } }
        else { resolve = nil }
        return AtlasReaderTransport(create: { [self] in create() },
            open: { [self] owner, _, _ in open(owner) },
            window: { [self] owner, offset, _ in window(owner, offset: offset) },
            lines: { _, _, _, _ in nil }, position: resolve,
            cancel: { [self] owner in lock.lock(); calls.append("cancel:\(owner)"); lock.unlock(); return true },
            close: { [self] owner in
                lock.lock(); captures[owner] = nil; calls.append("close:\(owner)"); lock.unlock(); return true
            }, retirementFailed: { fatalError("reader retirement failed") },
            fork: { [self] in fork($0, $1) })
    }
}

@main @MainActor private enum AtlasReaderPositionTests {
    private static var checks = 0
    private static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        precondition(condition(), message); checks += 1
    }
    private static func rejects(_ message: String, _ operation: () throws -> Void) {
        do { try operation(); preconditionFailure(message) } catch { checks += 1 }
    }
    private static func pump() {
        _ = RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.005))
    }
    private static func wait(_ message: String, _ condition: () -> Bool) {
        let deadline = Date().addingTimeInterval(5)
        var satisfied = condition()
        while !satisfied, Date() < deadline { pump(); satisfied = condition() }
        check(satisfied, message)
    }
    private static func info(raw: [UInt8], encoding: String = "utf8") throws -> AtlasReaderInfo {
        try AtlasReaderInfo.decode(json(header(length: UInt64(raw.count), encoding: encoding)),
            handle: 2, path: "/project/file")
    }
    private static func request(_ column: UInt64 = 3, kind: AtlasReaderColumnKind = .unicodeScalar) -> AtlasReaderRequest {
        .position(line: 2, column: column, kind: kind, bytes: AtlasReaderLimits.pageBytes)
    }
    private static func opened(_ driver: PositionDriver, positions: Bool = true) -> AtlasPagedReaderModel {
        let model = AtlasPagedReaderModel(transport: driver.transport(positions: positions))
        model.open(root: "/project", path: "file", accessLease: nil)
        wait("reader did not open") { !model.busy && model.page != nil }
        return model
    }
    private static func go(_ model: AtlasPagedReaderModel, column: String = "3") {
        model.lineInput = "2"; model.columnInput = column; model.columnKind = .unicodeScalar
        model.goToPosition()
    }
    private static func pin(_ model: AtlasPagedReaderModel) -> AtlasPagedReaderModel {
        var result: AtlasPagedReaderModel?
        Task {
            do { result = try await model.fork() }
            catch { preconditionFailure("position fork failed: \(error)") }
        }
        wait("position fork did not finish") { result != nil }
        return result!
    }

    static func main() throws {
        try unicodeAndProtocol()
        try replacementProofsAndBoundaries()
        modelNavigationAndFork()
        staleDelivery()
        try importedCapabilities()
#if canImport(AppKit)
        try nativeCaret()
#endif
        print("AtlasReaderPositionTests: \(checks) position, protocol, source-proof and navigation assertions passed")
    }

    private static func unicodeAndProtocol() throws {
        let raw = Array(sample.utf8), metadata = try info(raw: raw)
        let wire = positioned(wholePage(raw: raw, text: sample))
        let page = try AtlasReaderPage.decode(json(wire), info: metadata, request: request())
        check(page.position?.originalOffset == 8 && page.position?.windowUtf8Offset == 8
            && page.position?.windowUTF16Offset == 6, "UTF-8 scalar caret lost its source coordinates")
        check(page.position?.nativeCaretRange == NSRange(location: 6, length: 0),
              "a point was expanded into a text selection")
        for little in [true, false] {
            let encoded = utf16(sample, littleEndian: little), encoding = little ? "utf16le" : "utf16be"
            let metadata = try info(raw: encoded, encoding: encoding)
            let base = wholePage(raw: encoded, text: sample, encoding: encoding, bomBytes: 2)
            for (kind, column) in [(AtlasReaderColumnKind.unicodeScalar, UInt64(3)), (.utf16CodeUnit, 4), (.decodedUtf8Byte, 6)] {
                let locationRequest = request(column, kind: kind)
                let wire = positioned(base, column: column, kind: kind, original: 14, utf8: 8)
                let page = try AtlasReaderPage.decode(json(wire), info: metadata, request: locationRequest)
                check(page.position?.nativeCaretRange == NSRange(location: 6, length: 0)
                    && page.position?.originalOffset == 14 && page.start == 2,
                    "UTF-16 units, original BOM bytes and decoded offsets were interchanged")
                var forged = wire
                var location = forged["position"] as! [String: String]
                location["original_offset"] = "16"; forged["position"] = location
                rejects("independently in-bounds UTF-16 source and text offsets were trusted") {
                    _ = try AtlasReaderPage.decode(json(forged), info: metadata, request: locationRequest)
                }
            }
        }
        let combining = try AtlasReaderPage.decode(json(positioned(wholePage(raw: raw, text: sample),
            column: 4, original: 9, utf8: 9)), info: metadata, request: request(4))
        check(combining.position?.originalOffset == 9 && combining.position?.windowUTF16Offset == 7
            && combining.position?.nativeCaretRange == nil,
            "a logical combining-mark position was rounded to a different native character")
        if case .composedCharacter? = combining.position?.caretLimitation { checks += 1 }
        else { preconditionFailure("missing composed-character limitation") }
        let afterTab = try AtlasReaderPage.decode(json(positioned(wholePage(raw: raw, text: sample),
            column: 6, original: 12, utf8: 12)), info: metadata, request: request(6))
        check(afterTab.position?.nativeCaretRange == NSRange(location: 9, length: 0),
              "logical navigation expanded a tab into visual columns")
        let bidi = "AאבZ", bidiRaw = Array(bidi.utf8)
        let bidiPage = try AtlasReaderPage.decode(json(positioned(wholePage(raw: bidiRaw, text: bidi),
            line: 1, column: 3, original: 3, utf8: 3)), info: info(raw: bidiRaw),
            request: .position(line: 1, column: 3, kind: .unicodeScalar, bytes: AtlasReaderLimits.pageBytes))
        check(bidiPage.position?.nativeCaretRange == NSRange(location: 2, length: 0),
              "logical bidi coordinates were replaced by guessed visual order")

        for field in ["line", "column", "column_kind", "original_offset", "window_utf8_offset"] {
            var bad = wire
            var value = bad["position"] as! [String: String]
            value[field] = field == "column_kind" ? "visual_column" : "18446744073709551615"
            bad["position"] = value
            rejects("mismatched or unbounded position metadata accepted") {
                _ = try AtlasReaderPage.decode(json(bad), info: metadata, request: request())
            }
        }
        for field in ["owner", "file_id", "source_revision"] {
            var bad = wire; bad[field] = "999"
            rejects("foreign position identity accepted") {
                _ = try AtlasReaderPage.decode(json(bad), info: metadata, request: request())
            }
        }
        for field in ["position", "selection"] {
            var bad = wire; bad.removeValue(forKey: field)
            rejects("position response omitted an explicit required field") {
                _ = try AtlasReaderPage.decode(json(bad), info: metadata, request: request())
            }
        }
        for offset in ["5", "0", "20", "08"] {
            var bad = wire
            var value = bad["position"] as! [String: String]
            value["window_utf8_offset"] = offset; bad["position"] = value
            rejects("interior-scalar, mismatched, wide or noncanonical caret accepted") {
                _ = try AtlasReaderPage.decode(json(bad), info: metadata, request: request())
            }
        }
        var shifted = wire
        shifted["requested_original_range"] = ["start": "2", "end": String(raw.count)]
        shifted["boundaries_adjusted"] = true
        rejects("incorrect position window centering accepted") {
            _ = try AtlasReaderPage.decode(json(shifted), info: metadata, request: request())
        }
        var wrongText = wire; wrongText["text"] = sample.replacingOccurrences(of: "last", with: "lost")
        rejects("exact position page did not round-trip original bytes") {
            _ = try AtlasReaderPage.decode(json(wrongText), info: metadata, request: request())
        }
        var ordinary = wire; ordinary["command"] = "window"
        rejects("a plain page borrowed position authority") {
            _ = try AtlasReaderPage.decode(json(ordinary), info: metadata, request: .firstPage)
        }
        for target in [
            AtlasReaderRequest.position(line: 0, column: 1, kind: .unicodeScalar, bytes: 4),
            .position(line: 1, column: 0, kind: .unicodeScalar, bytes: 4),
            .position(line: 1, column: 1, kind: .unicodeScalar, bytes: 3),
            .position(line: 1, column: 1, kind: .unicodeScalar, bytes: AtlasReaderLimits.maximumPageBytes + 1)
        ] { rejects("invalid position request admitted") { try target.validate(capturedBytes: 30) } }
    }

    private static func replacementProofsAndBoundaries() throws {
        for (raw, text, original, offset, available) in [
            ([UInt8(255), 97], "�a", UInt64(1), UInt64(3), true),
            ([UInt8(97), 255], "a�", UInt64(1), UInt64(1), true),
            ([UInt8(255), 97, 255], "�a�", UInt64(1), UInt64(3), false)
        ] {
            let wire = positioned(wholePage(raw: raw, text: text, replacements: true),
                line: 1, column: 2, original: original, utf8: offset)
            let page = try AtlasReaderPage.decode(json(wire), info: info(raw: raw),
                request: .position(line: 1, column: 2, kind: .unicodeScalar, bytes: AtlasReaderLimits.pageBytes))
            check((page.position?.nativeCaretRange != nil) == available && page.position?.originalOffset == original,
                  "replacement context caused a false caret or discarded the exact logical position")
            if !available {
                if case .unverifiedDecodedOffset? = page.position?.caretLimitation { checks += 1 }
                else { preconditionFailure("replacement uncertainty mislabeled as a grapheme problem") }
            }
        }
        let malformed16: [UInt8] = [255, 254, 0, 216, 97, 0]
        let replacement16 = try AtlasReaderPage.decode(json(positioned(wholePage(raw: malformed16, text: "�a",
            encoding: "utf16le", bomBytes: 2, replacements: true), line: 1, column: 2, original: 4, utf8: 3)),
            info: info(raw: malformed16, encoding: "utf16le"),
            request: .position(line: 1, column: 2, kind: .unicodeScalar, bytes: AtlasReaderLimits.pageBytes))
        check(replacement16.position?.nativeCaretRange == NSRange(location: 1, length: 0),
              "an exact UTF-16 suffix did not prove the caret after malformed source")
        // EOL precedes the complete CRLF. EOF is a legitimate zero-width point.
        let raw = Array(sample.utf8), metadata = try info(raw: raw)
        let eol = try AtlasReaderPage.decode(json(positioned(wholePage(raw: raw, text: sample),
            column: 7, original: 13, utf8: 13)), info: metadata, request: request(7))
        check(eol.position?.nativeCaretRange == NSRange(location: 10, length: 0), "EOL moved into CRLF")
        let eof = try AtlasReaderPage.decode(json(positioned(wholePage(raw: raw, text: sample),
            line: 3, column: 5, original: UInt64(raw.count), utf8: UInt64(raw.count))), info: metadata,
            request: .position(line: 3, column: 5, kind: .unicodeScalar, bytes: AtlasReaderLimits.pageBytes))
        check(eof.position?.nativeCaretRange == NSRange(location: sample.utf16.count, length: 0),
              "EOF was refused or widened")
        for (raw, encoding, bom) in [([UInt8(239), 187, 191], "utf8", 3),
                                      ([UInt8(255), 254], "utf16le", 2)] {
            var wire = positioned(wholePage(raw: raw, text: "", encoding: encoding, bomBytes: bom),
                line: 1, column: 1, original: UInt64(bom), utf8: 0)
            wire["requested_original_range"] = ["start": String(bom), "end": String(bom)]
            wire["boundaries_adjusted"] = false
            let page = try AtlasReaderPage.decode(json(wire), info: info(raw: raw, encoding: encoding),
                request: .position(line: 1, column: 1, kind: .unicodeScalar, bytes: 4))
            check(page.position?.nativeCaretRange == NSRange(location: 0, length: 0),
                  "BOM-only logical empty page lost its admitted physical-line point")
        }
        let contentBOM = "\u{feff}z", contentRaw = Array("\u{feff}\u{feff}z".utf8)
        let content = try AtlasReaderPage.decode(json(positioned(wholePage(raw: contentRaw, text: contentBOM, bomBytes: 3),
            line: 1, column: 2, original: 6, utf8: 3)), info: info(raw: contentRaw),
            request: .position(line: 1, column: 2, kind: .unicodeScalar, bytes: AtlasReaderLimits.pageBytes))
        check(content.position?.nativeCaretRange == NSRange(location: 1, length: 0),
              "a delivered content U+FEFF was stripped a second time")
        let length = AtlasReaderLimits.captureBytes, start: UInt64 = 8 * 1024 * 1024
        let text = String(repeating: "x", count: 64)
        var far = positioned(wholePage(raw: Array(text.utf8), text: text),
            line: 900_001, column: 120_002, original: start + 30, utf8: 30)
        far["captured_bytes"] = String(length)
        far["requested_original_range"] = ["start": String(start), "end": String(start + 64)]
        far["visible_range"] = ["start": String(start), "end": String(start + 64)]
        far["next_offset"] = String(start + 64)
        let farInfo = try AtlasReaderInfo.decode(json(header(length: length)), handle: 2, path: "/project/file")
        let page = try AtlasReaderPage.decode(json(far), info: farInfo,
            request: .position(line: 900_001, column: 120_002, kind: .unicodeScalar, bytes: 64))
        check(page.start == start && page.position?.originalOffset == start + 30
            && page.position?.nativeCaretRange == NSRange(location: 30, length: 0),
            "a far original location was narrowed into page-local coordinates")
    }

    private static func modelNavigationAndFork() {
        let driver = PositionDriver(), model = opened(driver)
        check(model.canGoToPosition, "native position capability missing")
        go(model)
        wait("position did not publish") { !model.busy && model.page?.position != nil }
        let source = model.source, navigation = model.navigation
        check(model.nativeSelectionRange == NSRange(location: 6, length: 0) && model.canGoBack,
              "accepted logical point was not installed as a native caret")
        model.columnInput = "0"; model.goToPosition()
        check(!model.busy && model.source === source && model.navigation == navigation,
              "invalid input replaced the current page")
        model.columnInput = "999"; model.goToPosition()
        wait("missing column did not terminate") { !model.busy }
        check(model.source === source && model.page?.position?.originalOffset == 8
            && model.notice.contains("READER_SESSION_MISSING_COLUMN"), "refused position changed page/history")
        driver.corrupt(); go(model)
        wait("forged position did not terminate") { !model.busy }
        check(model.source === source && model.navigation == navigation, "forged receipt replaced current source")
        model.offsetInput = "0"; model.goToByte()
        wait("plain navigation did not finish") { !model.busy && model.page?.position == nil }
        model.back()
        wait("Back did not restore exact typed position") { !model.busy && model.page?.position != nil }
        check(model.lineInput == "2" && model.columnInput == "3"
            && model.nativeSelectionRange == NSRange(location: 6, length: 0), "history lost the exact logical target")
        model.forward()
        wait("Forward did not retire position caret") { !model.busy && model.page?.position == nil }
        go(model); wait("position before pin") { !model.busy && model.page?.position != nil }
        let parentNavigation = model.navigation, opens = driver.log.filter { $0.hasPrefix("open:") }.count
        let child = pin(model)
        check(child.page?.identity.owner != model.page?.identity.owner && !child.canGoBack,
              "pin reused the parent's identity/history")
        check(child.page?.position?.originalOffset == 8
            && child.nativeSelectionRange == NSRange(location: 6, length: 0),
            "pin lost the exact logical point or borrowed parent query IDs")
        check(model.navigation == parentNavigation && driver.log.filter { $0.hasPrefix("open:") }.count == opens,
              "pin navigated the parent or reread the live file")
        driver.changeLive(); model.close()
        go(child, column: "7"); wait("independent child position") { !child.busy }
        check(child.page?.position?.originalOffset == 13 && child.source?.text == sample,
              "child location navigation reopened changed live bytes")
        child.refreshFromDisk(); wait("explicit refresh after position") { !child.busy }
        check(child.source?.text == "replacement\n" && child.page?.position == nil
            && child.nativeSelectionRange == nil && child.columnInput == "1" && !child.canGoBack,
            "refresh retained an old exact position")
        child.close()

        let unsupportedDriver = PositionDriver(), unsupported = opened(unsupportedDriver, positions: false)
        let accepted = unsupported.source
        check(!unsupported.canGoToPosition, "legacy injected transport fabricated a position capability")
        go(unsupported)
        check(unsupported.source === accepted && !unsupported.busy
            && !unsupportedDriver.log.contains(where: { $0.hasPrefix("position:") }),
            "missing position support disturbed the accepted reader")
        unsupported.close()
    }

    private static func staleDelivery() {
        for change in ["line", "column", "kind", "close", "refresh", "newer"] {
            let driver = PositionDriver(), model = opened(driver), original = model.source
            driver.block(); go(model)
            wait("position did not reach worker") { driver.entered.wait(timeout: .now()) == .success }
            switch change {
            case "line": model.lineInput = "9"
            case "column": model.columnInput = "7"
            case "kind": model.columnKind = .utf16CodeUnit
            case "close": model.close()
            case "refresh": model.cancel(); driver.changeLive(); model.refreshFromDisk()
            default: model.columnInput = "7"; model.goToPosition()
            }
            driver.gate.signal()
            wait("old position did not drain") { driver.returns >= 1 && !model.busy }
            for _ in 0..<3 { pump() }
            if change == "close" {
                check(model.page == nil && model.source == nil, "closed reader revived a position")
            } else if change == "refresh" {
                check(model.source?.text == "replacement\n" && model.page?.position == nil,
                      "old position replaced refreshed capture")
            } else if change == "newer" {
                check(model.page?.position?.column == 7 && model.page?.position?.originalOffset == 13,
                      "superseded coordinate request won publication")
                model.back(); wait("newer position polluted history") { !model.busy }
                check(model.page?.position == nil && !model.canGoBack, "canceled position entered history")
            } else {
                check(model.source === original && model.page?.position == nil && !model.canGoBack,
                      "input edit permitted a stale location delivery")
            }
            model.close()
        }
    }

    private static func importedCapabilities() throws {
        let base = AtlasReaderTransport(create: { 1 }, open: { _, _, _ in nil },
            window: { _, _, _ in nil }, lines: { _, _, _, _ in nil },
            position: { owner, line, column, kind, bytes in
                precondition(owner == 10 && line == 20 && column == 30 && kind == 2 && bytes == 64)
                return "preserved-position"
            }, cancel: { _ in true }, close: { _ in true }, retirementFailed: {})
        let supplied = try AtlasSuppliedReaderSource(root: "/project", path: "file", text: sample)
        let target = AtlasSearchCapturedHit(root: "/project", path: "file", needle: "A", start: 3, end: 4,
            capturedBytes: UInt64(sample.utf8.count), openReader: { _ in "import" })
        for transport in [base.importing(supplied), base.importing(target)] {
            check(transport.position?(10, 20, 30, 2, 64) == "preserved-position",
                  "capture import silently removed typed location navigation")
        }
    }

#if canImport(AppKit)
    private static func nativeCaret() throws {
        _ = NSApplication.shared
        let reader = AtlasSourceReaderController()
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 640, height: 260),
            styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false; window.contentView = reader.view
        defer { window.close() }
        let source = AtlasSource(path: "file", text: sample)
        let responder = window.firstResponder, raw = Array(sample.utf8)
        for (column, offset) in [(UInt64(3), UInt64(8)), (7, 13)] {
            let page = try AtlasReaderPage.decode(json(positioned(wholePage(raw: raw, text: sample),
                column: column, original: offset, utf8: offset)), info: info(raw: raw), request: request(column))
            guard let range = page.position?.nativeCaretRange else { preconditionFailure("missing native caret") }
            check(reader.update(source: source, navigation: UUID(),
                selection: AtlasReaderSelection(source: source, range: range)) {
                NSAttributedString(string: sample, attributes: [.font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular)])
            }, "AppKit refused the verified native point")
            check(reader.textView.selectedRange() == range && range.length == 0,
                  "AppKit expanded a logical caret into another source range")
            check(reader.textView.string.utf8.elementsEqual(sample.utf8) && window.firstResponder === responder,
                  "position navigation rewrote source or stole native focus")
        }
    }
#endif
}
