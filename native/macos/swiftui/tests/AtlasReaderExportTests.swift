import Foundation
import Dispatch

// Real receipt admission, target validation, retained-session lifetime and
// export orchestration; only the foreign reader/writer boundaries are injected.
// Rust tests separately exercise actual create-only filesystem effects.
private func exportHex(_ bytes: some Sequence<UInt8>) -> String {
    bytes.map { String(format: "%02x", $0) }.joined()
}
private func exportJSON(_ value: [String: Any]) -> String {
    String(decoding: try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self)
}
private func exportReceipt(_ request: AtlasReaderExportRequest,
    effect: AtlasReaderExportEffect = .parentSynced, error: String? = nil, written: UInt64? = nil) -> [String: Any] {
    let target = request.target, identity = target.identity
    let complete = effect == .written || effect == .synced || effect == .parentSynced
    return ["schema": "fcb.reader-export/1", "command": "export-original", "status": error == nil ? "ok" : "error",
        "owner": String(identity.owner), "file_id": String(identity.file), "source_revision": String(identity.revision),
        "captured_bytes": String(identity.capturedBytes), "capture_sha256": target.captureSHA256,
        "original_range": ["start": String(target.start), "end": String(target.end)],
        "export_bytes": String(target.byteCount), "bytes_written": String(written ?? (complete ? target.byteCount : 0)),
        "destination": ["encoding": "unix-bytes", "hex": exportHex(request.destination.utf8), "display": "label only"],
        "export_domain": "original-bytes", "additional_source_bytes_read": "0", "overwritten": false,
        "power_loss_qualified": false, "effect": effect.rawValue, "complete": complete,
        "error": error.map { ["code": $0, "next_action": "Inspect before an explicit retry."] as Any } ?? NSNull()]
}
private func exportReaderHeader(_ owner: UInt64, length: UInt64, command: String) -> [String: Any] {
    ["schema": "fcb.reader-session/1", "status": "ok", "command": command,
        "owner": String(owner), "file_id": "1", "source_revision": "1", "captured_bytes": String(length),
        "encoding": "utf8", "path": ["encoding": "unix-bytes", "hex": exportHex("/project/source".utf8),
            "display": "/project/source"],
        "additional_source_bytes_read": "0", "native_presented": false]
}

private final class ExportProbe: @unchecked Sendable {
    private let lock = NSLock()
    private var callCount = 0, openCount = 0, closeCount = 0
    private var releases: [String] = []
    let gate = DispatchSemaphore(value: 0)
    func entered() { lock.lock(); callCount += 1; lock.unlock() }
    func opened() { lock.lock(); openCount += 1; lock.unlock() }
    func closed() { lock.lock(); closeCount += 1; lock.unlock() }
    func released(_ name: String) { lock.lock(); releases.append(name); lock.unlock() }
    var calls: Int { lock.lock(); defer { lock.unlock() }; return callCount }
    var opens: Int { lock.lock(); defer { lock.unlock() }; return openCount }
    var closes: Int { lock.lock(); defer { lock.unlock() }; return closeCount }
    func didRelease(_ name: String) -> Bool { lock.lock(); defer { lock.unlock() }; return releases.contains(name) }
}
private final class ExportGrant {
    let probe: ExportProbe, name: String
    init(_ probe: ExportProbe, _ name: String) { self.probe = probe; self.name = name }
    deinit { probe.released(name) }
}
private final class ExportReaderFixture: @unchecked Sendable {
    let domain = UUID()
    let raw = Array("alpha beta gamma\r\n".utf8)
    let probe: ExportProbe
    init(_ probe: ExportProbe) { self.probe = probe }
    var transport: AtlasReaderTransport {
        AtlasReaderTransport(create: { 2 }, open: { [self] handle, _, _ in
            precondition(!Thread.isMainThread); probe.opened()
            var value = exportReaderHeader(handle, length: UInt64(raw.count), command: "info")
            value["capture_origin"] = "regular-file-observation-not-atomic"
            value["capture_sha256"] = String(repeating: "a", count: 64)
            return exportJSON(value)
        }, window: { [self] handle, offset, limit in
            precondition(!Thread.isMainThread)
            let end = min(UInt64(raw.count), offset + limit)
            let bytes = raw[Int(offset)..<Int(end)]
            var value = exportReaderHeader(handle, length: UInt64(raw.count), command: "window")
            value["text_kind"] = "logical-captured-text-not-shaped"
            value["requested_original_range"] = ["start": String(offset), "end": String(end)]
            value["visible_range"] = ["start": String(offset), "end": String(end)]
            value["range_limited"] = false; value["boundaries_adjusted"] = false
            value["has_replacements"] = false; value["first_physical_line"] = NSNull(); value["selection"] = NSNull()
            value["text"] = String(decoding: bytes, as: UTF8.self); value["original_hex"] = exportHex(bytes)
            value["next_offset"] = end < UInt64(raw.count) ? String(end) as Any : NSNull()
            return exportJSON(value)
        }, lines: { _, _, _, _ in nil }, cancel: { _ in true }, close: { [probe] _ in probe.closed(); return true },
        retirementFailed: { preconditionFailure("retirement failed") },
        comparison: AtlasReaderComparisonTransport(domain: domain, compare: { _, _, _, _, _ in nil }),
        textSelection: AtlasReaderTextSelectionTransport(resolve: { [self] handle, start, end, first, last in
            let bytes = raw[Int(start + first)..<Int(start + last)]
            var value = exportReaderHeader(handle, length: UInt64(raw.count), command: "resolve-selection")
            value["selection_namespace"] = "native-text"
            value["window_original_range"] = ["start": String(start), "end": String(end)]
            value["window_utf8_range"] = ["start": String(first), "end": String(last)]
            value["original_range"] = ["start": String(start + first), "end": String(start + last)]
            value["text"] = String(decoding: bytes, as: UTF8.self); value["original_hex"] = exportHex(bytes)
            value["contains_replacements"] = false
            return exportJSON(value)
        }))
    }
}

@MainActor @main private enum AtlasReaderExportTests {
    private static var checks = 0
    private static func check(_ value: Bool, _ message: String) { precondition(value, message); checks += 1 }
    private static func rejects(_ message: String, _ work: () throws -> Void) {
        do { try work(); preconditionFailure(message) } catch { checks += 1 }
    }
    private static func wait(_ message: String, _ condition: () -> Bool) async {
        for _ in 0..<2_000 {
            if condition() { return }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        preconditionFailure(message)
    }
    private static func target(length: UInt64 = 32 * 1024 * 1024, start: UInt64 = 7, end: UInt64 = 8 * 1024 * 1024)
        throws -> AtlasReaderExportTarget {
        let identity = AtlasReaderIdentity(owner: 2, file: 1, revision: 1, capturedBytes: length,
            pathBytes: Array("/project/source".utf8), encoding: "utf16le", displayPath: "/project/source")
        return try AtlasReaderExportTarget(identity: identity, captureSHA256: String(repeating: "a", count: 64),
            navigation: UUID(), origin: .byteRange, start: start, end: end)
    }
    private static func opened(_ fixture: ExportReaderFixture, access: AnyObject? = nil) async -> AtlasPagedReaderModel {
        let model = AtlasPagedReaderModel(transport: fixture.transport)
        model.open(root: "/project", path: "source", accessLease: access)
        await wait("reader did not open") { !model.busy && model.page != nil }
        return model
    }
    private static func rejectReceipt(_ value: [String: Any], _ request: AtlasReaderExportRequest) {
        rejects("contradictory export receipt accepted") { _ = try AtlasReaderExportReceipt.decode(exportJSON(value), request: request) }
    }

    static func main() async throws {
        try receiptValidation()
        try requestBoundsAndRawRanges()
        await selectionAndNavigationAuthority()
        await effectsAndLifetimeSurviveCancellation()
        await noEffectAndUncertainOutcomes()
        print("AtlasReaderExport: \(checks) receipt, range, stale-target, cancellation and lifetime checks passed")
    }

    private static func receiptValidation() throws {
        let request = try AtlasReaderExportRequest(target: target(), destination: "/out/café.bin")
        let success = try AtlasReaderExportReceipt.decode(exportJSON(exportReceipt(request)), request: request)
        check(success.succeeded && success.bytesWritten == request.target.byteCount, "large exact range refused")
        for effect in [AtlasReaderExportEffect.none, .incomplete, .written, .synced, .parentSynced] {
            let bytes = effect == .incomplete ? UInt64(123) : nil
            let value = try AtlasReaderExportReceipt.decode(exportJSON(exportReceipt(request, effect: effect,
                error: "READER_EXPORT_CANCELED", written: bytes)), request: request)
            check(value.effect == effect && !value.succeeded && value.errorCode == "READER_EXPORT_CANCELED",
                "cancellation erased an entered write effect")
        }
        for (key, value) in [
            ("schema", "fcb.reader-export/2"), ("status", "pending"), ("command", "copy-range"),
            ("owner", "3"), ("file_id", "2"), ("source_revision", "2"), ("captured_bytes", "33554433"),
            ("capture_sha256", String(repeating: "b", count: 64)), ("export_bytes", "1"),
            ("bytes_written", String(request.target.byteCount + 1)), ("additional_source_bytes_read", "1"),
            ("export_domain", "decoded-text"), ("effect", "none"), ("owner", "02")
        ] {
            var valueObject = exportReceipt(request); valueObject[key] = value; rejectReceipt(valueObject, request)
        }
        for key in ["overwritten", "power_loss_qualified"] {
            var value = exportReceipt(request); value[key] = true; rejectReceipt(value, request)
        }
        var value = exportReceipt(request); value["complete"] = false; rejectReceipt(value, request)
        value = exportReceipt(request); value["owner"] = 2; rejectReceipt(value, request)
        value = exportReceipt(request)
        value["original_range"] = ["start": "8", "end": String(request.target.end)]; rejectReceipt(value, request)
        value = exportReceipt(request)
        value["destination"] = ["encoding": "unix-bytes", "hex": exportHex("/out/cafe\u{301}.bin".utf8),
            "display": "same-looking destination"]
        rejectReceipt(value, request)
        value = exportReceipt(request, effect: .incomplete, error: "READER_EXPORT_IO")
        value["complete"] = true; rejectReceipt(value, request)
        value = exportReceipt(request, effect: .none, error: "READER_EXPORT_IO", written: 1); rejectReceipt(value, request)
        value = exportReceipt(request); value["error"] = ["code": "READER_EXPORT_CANCELED", "next_action": "Inspect."]
        rejectReceipt(value, request)
        let admission: [String: Any] = ["schema": "fcb.reader-session/1", "status": "error", "owner": "2",
            "exit_code": "130", "error": ["code": "READER_HANDLE_CANCELED"]]
        let refused = try AtlasReaderExportReceipt.decode(exportJSON(admission), request: request)
        check(refused.effect == .none && !refused.complete, "known pre-entry refusal became uncertain success")
        var mixed = admission; mixed["effect"] = "destination-created-incomplete"; rejectReceipt(mixed, request)
        mixed = admission; mixed["owner"] = "3"; rejectReceipt(mixed, request)
        for json in [nil, "", "{}", String(repeating: "x", count: AtlasReaderExportReceipt.maximumResponseBytes + 1)] {
            rejects("missing or unbounded receipt accepted") { _ = try AtlasReaderExportReceipt.decode(json, request: request) }
        }
    }

    private static func requestBoundsAndRawRanges() throws {
        let oddUTF16 = try target()
        check(oddUTF16.start == 7 && oddUTF16.byteCount > 2 * 1024 * 1024,
            "original byte ranges were decoded, aligned or limited to clipboard size")
        let full = try target(start: 0, end: 32 * 1024 * 1024)
        check(full.byteCount == AtlasReaderLimits.captureBytes, "complete admitted capture cannot export")
        let empty = try AtlasReaderExportRequest(target: target(length: 0, start: 0, end: 0), destination: "/out/empty")
        let emptyReceipt = try AtlasReaderExportReceipt.decode(exportJSON(exportReceipt(empty)), request: empty)
        check(emptyReceipt.succeeded && emptyReceipt.bytesWritten == 0, "empty retained capture cannot export")
        for (start, end) in [(UInt64(9), UInt64(8)), (0, UInt64.max)] {
            rejects("invalid range accepted") { _ = try target(start: start, end: end) }
        }
        rejects("oversized capture admitted") { _ = try target(length: AtlasReaderLimits.captureBytes + 1) }
        for path in ["", "relative", "/", "/out/../secret", "/out//file", "/out/./file", "/out/file\0tail",
                     "/" + String(repeating: "x", count: 16_384)] {
            rejects("invalid destination admitted") { _ = try AtlasReaderExportRequest(target: oddUTF16, destination: path) }
        }
    }

    private static func selectionAndNavigationAuthority() async {
        let fixture = ExportReaderFixture(ExportProbe())
        let model = await opened(fixture)
        let whole = model.originalExportTarget(.wholeCapture)!
        check(whole.start == 0 && whole.end == UInt64(fixture.raw.count), "whole-capture target clipped to page")
        check(model.retainedExportSource(matching: whole.identity, registryDomain: UUID()) == nil,
            "foreign registry accepted")
        model.updateTextSelection(source: model.source!, navigation: model.navigation, range: NSRange(location: 6, length: 4))
        await wait("selection unresolved") { !model.resolvingTextSelection && model.textSelection != nil }
        let selected = model.originalExportTarget(.textSelection(model.textSelection!.id))!
        check(selected.start == 6 && selected.end == 10 && model.acceptsOriginalExport(selected), "exact selected range lost")
        model.updateTextSelection(source: model.source!, navigation: model.navigation, range: NSRange(location: 0, length: 5))
        check(!model.acceptsOriginalExport(selected), "stale selection menu remained authorized")
        await wait("replacement selection unresolved") { !model.resolvingTextSelection && model.textSelection != nil }
        model.offsetInput = "1"; model.goToByte()
        check(!model.acceptsOriginalExport(whole), "pending navigation lent stale export authority")
        await wait("navigation did not settle") { !model.busy }
        check(!model.acceptsOriginalExport(whole), "old native page navigation survived a replacement")
        let current = model.originalExportTarget(.byteRange, start: 0, end: UInt64(fixture.raw.count))!
        check(current.byteCount == UInt64(fixture.raw.count), "explicit range was limited to visible page")
        model.close()
        check(!model.acceptsOriginalExport(current), "closed reader still authorized export")
    }

    private static func effectsAndLifetimeSurviveCancellation() async {
        let probe = ExportProbe(), fixture = ExportReaderFixture(ExportProbe())
        var sourceAccess: ExportGrant? = ExportGrant(probe, "source")
        var destinationAccess: ExportGrant? = ExportGrant(probe, "destination")
        let reader = await opened(fixture, access: sourceAccess)
        let target = reader.originalExportTarget(.wholeCapture)!
        var source = reader.retainedExportSource(matching: target.identity, registryDomain: fixture.domain)
        let model = AtlasReaderExportModel(transport: .init(domain: fixture.domain, save: { handle, request, cancellation in
            precondition(!Thread.isMainThread && handle == request.target.identity.owner)
            probe.entered()
            precondition(probe.gate.wait(timeout: .now() + .seconds(5)) == .success)
            precondition(cancellation.isCanceled)
            return exportJSON(exportReceipt(request, effect: .written, error: "READER_EXPORT_CANCELED"))
        }))
        check(model.save(source: source!, target: target, destination: "/out/retained.bin",
            destinationAccess: destinationAccess), "export was not admitted")
        check(!model.save(source: source!, target: target, destination: "/out/second.bin",
            destinationAccess: destinationAccess), "second concurrent export was queued")
        source = nil; sourceAccess = nil; destinationAccess = nil
        await wait("writer did not enter") { probe.calls == 1 }
        model.cancel(); reader.close()
        check(model.busy && model.cancellationRequested, "cancel discarded result channel")
        check(!probe.didRelease("source") && !probe.didRelease("destination"), "native grants retired before writer drained")
        check(fixture.probe.closes == 0, "retained handle closed before export returned")
        probe.gate.signal()
        await wait("writer outcome lost") { !model.busy }
        check(model.receipt?.effect == .written && model.receipt?.complete == true
            && model.receipt?.errorCode == "READER_EXPORT_CANCELED", "completed bytes relabeled as no effect")
        check(model.notice.contains("All") && model.request?.destination == "/out/retained.bin",
            "effect or destination lost after source close")
        await wait("source lease did not drain") {
            probe.didRelease("source") && probe.didRelease("destination") && fixture.probe.closes == 1
        }
        check(fixture.probe.opens == 1, "export reopened source path")
    }

    private static func noEffectAndUncertainOutcomes() async {
        let probe = ExportProbe(), fixture = ExportReaderFixture(ExportProbe())
        let reader = await opened(fixture), target = reader.originalExportTarget(.wholeCapture)!
        let source = reader.retainedExportSource(matching: target.identity, registryDomain: fixture.domain)!
        let domainMismatch = AtlasReaderExportModel(transport: .init(domain: UUID(), save: { _, _, _ in
            preconditionFailure("foreign registry entered")
        }))
        check(!domainMismatch.save(source: source, target: target, destination: "/out/file", destinationAccess: nil),
            "wrong registry reached foreign writer")
        let missing = AtlasReaderExportModel(transport: .init(domain: fixture.domain, save: { _, _, _ in
            probe.entered(); return nil
        }))
        check(missing.save(source: source, target: target, destination: "/out/uncertain", destinationAccess: nil),
            "missing-reply scenario not started")
        await wait("missing response did not settle") { !missing.busy }
        check(missing.receipt == nil && missing.notice.contains("could not be verified") && probe.calls == 1,
            "missing response became rollback or automatic retry")
        let preEntry = AtlasReaderExportModel(transport: .init(domain: fixture.domain, save: { _, _, _ in
            throw AtlasReaderExportError.notStarted
        }))
        check(preEntry.save(source: source, target: target, destination: "/out/canceled", destinationAccess: nil),
            "pre-entry scenario not admitted")
        await wait("pre-entry cancellation did not settle") { !preEntry.busy }
        check(preEntry.receipt == nil && preEntry.notice.contains("before the writer started"),
            "known pre-entry cancellation mislabeled")
        reader.close()
    }
}
