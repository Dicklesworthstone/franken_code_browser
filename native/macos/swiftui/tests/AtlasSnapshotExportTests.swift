// Production receipt admission and one-shot native orchestration. Injected
// transport is not Rust execution, filesystem publication or physical Mac proof.
import Foundation
import Dispatch

private final class SnapshotExportProbe: @unchecked Sendable {
    private let lock = NSLock()
    private var operations = 0
    private var retired: [String] = []
    func enter() { lock.lock(); operations += 1; lock.unlock() }
    func release(_ label: String) { lock.lock(); retired.append(label); lock.unlock() }
    var calls: Int { lock.lock(); defer { lock.unlock() }; return operations }
    func released(_ label: String) -> Bool { lock.lock(); defer { lock.unlock() }; return retired.contains(label) }
}
private final class SnapshotExportGrant {
    let probe: SnapshotExportProbe
    let label: String
    init(_ probe: SnapshotExportProbe, _ label: String) { self.probe = probe; self.label = label }
    deinit { probe.release(label) }
}

@main private enum AtlasSnapshotExportTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ value: Bool, _ label: String) {
        precondition(value, label); checks += 1
    }
    private static func encode(_ object: [String: Any]) throws -> String {
        String(decoding: try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]), as: UTF8.self)
    }
    private static func success(_ destination: String = "/out/project.fcbs") -> [String: Any] {
        ["schema": "fcb.cli/1", "status": "ok", "command": "snapshot-save",
         "effect": "complete-file-sync-requested",
         "destination": ["encoding": "unix-bytes", "hex": destination.utf8.map { String(format: "%02x", $0) }.joined(),
            "display": "presentation label only"],
         "snapshot_digest": String(repeating: "a", count: 64), "source_scope": "saved-observations-only",
         "live_roots_accessed": true, "policy": "product-defaults/repository-rules-v1",
         "discovery_complete": true, "known_files": "2", "captured_files": "2", "unavailable_files_count": "0",
         "captured_bytes": "8", "archive_bytes": "1024", "payload_bytes_read": "8", "read_calls": "2",
         "source_contains_plaintext": true, "power_loss_qualified": false, "rule_files_enabled": true]
    }
    private static func failure(_ effect: AtlasSnapshotExportEffect) -> [String: Any] {
        ["schema": "fcb.cli/1", "status": "error", "complete": false, "effect": effect.rawValue,
         "error": ["code": "SNAPSHOT_CANCELED", "subsystem": "snapshot", "retryable": false]]
    }
    @MainActor private static func wait(_ label: String, _ ready: () -> Bool) async {
        for _ in 0..<2_000 {
            if ready() { return }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        preconditionFailure(label)
    }
    @MainActor private static func source(_ root: String = "/repo", access: AnyObject? = nil) -> AtlasSnapshotExportSource {
        AtlasSnapshotExportSource(root: root, project: UUID(), browserOwner: 71, access: access)
    }
    @MainActor private static func rejects(_ object: [String: Any], request: AtlasSnapshotExportRequest) throws {
        do { _ = try AtlasSnapshotExportReceipt.decode(encode(object), request: request); preconditionFailure("Accepted contradictory export receipt") }
        catch AtlasSnapshotExportError.unverifiedOutcome { checks += 1 }
    }

    @MainActor static func main() async throws {
        let request = try AtlasSnapshotExportRequest(root: "/repo", destination: "/out/project.fcbs")
        let valid = try encode(success())
        let complete = try AtlasSnapshotExportReceipt.decode(valid, request: request)
        check(complete.succeeded && complete.summary?.complete == true && complete.summary?.capturedFiles == 2,
            "Synced success retains exact source coverage and digest")
        var partial = success(); partial["captured_files"] = "1"; partial["unavailable_files_count"] = "1"
        let incomplete = try AtlasSnapshotExportReceipt.decode(encode(partial), request: request)
        check(incomplete.succeeded && incomplete.summary?.complete == false && incomplete.message.contains("partial"),
            "A valid archive can contain explicit unavailable members")
        var undiscovered = success(); undiscovered["discovery_complete"] = false
        let openMembership = try AtlasSnapshotExportReceipt.decode(encode(undiscovered), request: request)
        check(openMembership.summary?.complete == false && openMembership.message.contains("additional files"),
            "Capturing every known member cannot conceal incomplete discovery")
        var empty = success()
        for key in ["known_files", "captured_files", "unavailable_files_count", "captured_bytes", "payload_bytes_read", "read_calls"] { empty[key] = "0" }
        check(try AtlasSnapshotExportReceipt.decode(encode(empty), request: request).summary?.complete == true,
            "An empty closed source universe produces a valid nonempty archive frame")
        for effect in [AtlasSnapshotExportEffect.none, .incomplete, .written, .synced] {
            let receipt = try AtlasSnapshotExportReceipt.decode(encode(failure(effect)), request: request)
            check(!receipt.succeeded && receipt.effect == effect && receipt.summary == nil,
                "Error effect survives cancellation without being relabeled as no write")
        }
        for (field, value) in [
            ("schema", "other"), ("command", "snapshot-inspect"), ("status", "unknown"),
            ("effect", "none"), ("snapshot_digest", String(repeating: "A", count: 64)),
            ("known_files", "20001"), ("captured_files", "3"), ("unavailable_files_count", "1"),
            ("captured_bytes", "67108865"), ("archive_bytes", "83886081"), ("payload_bytes_read", "7"),
            ("read_calls", "02"), ("source_scope", "live-working-tree")
        ] {
            var bad = success(); bad[field] = value; try rejects(bad, request: request)
        }
        for field in ["source_contains_plaintext", "live_roots_accessed", "rule_files_enabled"] {
            var bad = success(); bad[field] = false; try rejects(bad, request: request)
        }
        var qualified = success(); qualified["power_loss_qualified"] = true; try rejects(qualified, request: request)
        var falseComplete = partial; falseComplete["complete"] = true; try rejects(falseComplete, request: request)
        var number = success(); number["known_files"] = 2; try rejects(number, request: request)
        try rejects(success("/out/other.fcbs"), request: request)
        let unicode = try AtlasSnapshotExportRequest(root: "/repo", destination: "/out/café.fcbs")
        try rejects(success("/out/cafe\u{301}.fcbs"), request: unicode)
        for field in ["command", "complete", "snapshot_digest", "destination", "captured_bytes"] {
            var bad = failure(.incomplete)
            switch field {
            case "command": bad[field] = "snapshot-inspect"
            case "complete": bad[field] = true
            case "destination": bad[field] = success()[field]
            case "captured_bytes": bad[field] = "0"
            default: bad[field] = String(repeating: "a", count: 64)
            }
            try rejects(bad, request: request)
        }
        for text in [nil, "", "{}", String(repeating: "x", count: AtlasSnapshotExportReceipt.maximumResponseBytes + 1)] {
            do { _ = try AtlasSnapshotExportReceipt.decode(text, request: request); preconditionFailure("Unverified write outcome admitted") }
            catch AtlasSnapshotExportError.unverifiedOutcome { checks += 1 }
        }
        for root in ["", "repo", "/repo\0other", "/repo/../other", "/" + String(repeating: "x", count: 16_384)] {
            do { _ = try AtlasSnapshotExportRequest(root: root, destination: "/out/file.fcbs"); preconditionFailure("Invalid source admitted") }
            catch AtlasSnapshotExportError.invalidInput { checks += 1 }
        }
        for destination in ["", "file.fcbs", "/", "/repo", "/out/../file.fcbs", "/out/file\0.fcbs"] {
            do { _ = try AtlasSnapshotExportRequest(root: "/repo", destination: destination); preconditionFailure("Invalid destination admitted") }
            catch AtlasSnapshotExportError.invalidInput { checks += 1 }
        }
        for limits in [
            AtlasSnapshotExportLimits(files: 0, fileBytes: 1, sourceBytes: 1, respectIgnores: true),
            AtlasSnapshotExportLimits(files: 65_537, fileBytes: 1, sourceBytes: 1, respectIgnores: true),
            AtlasSnapshotExportLimits(files: 1, fileBytes: 1_048_577, sourceBytes: 1, respectIgnores: true),
            AtlasSnapshotExportLimits(files: 1, fileBytes: 1, sourceBytes: 67_108_865, respectIgnores: true)
        ] {
            do { _ = try AtlasSnapshotExportRequest(root: "/repo", destination: "/out/file.fcbs", limits: limits); preconditionFailure("Unbounded capture admitted") }
            catch AtlasSnapshotExportError.invalidInput { checks += 1 }
        }
        let small = try AtlasSnapshotExportRequest(root: "/repo", destination: "/out/project.fcbs",
            limits: AtlasSnapshotExportLimits(files: 2, fileBytes: 2, sourceBytes: 4, respectIgnores: true))
        try rejects(success(), request: small)
        try await modelLifecycle(valid)
        try await effectAndUnknownOutcomes()
        print("AtlasSnapshotExportTests: \(checks) checks passed")
    }

    @MainActor private static func modelLifecycle(_ valid: String) async throws {
        let probe = SnapshotExportProbe(), release = DispatchSemaphore(value: 0)
        var model: AtlasSnapshotExportModel? = AtlasSnapshotExportModel(transport: SelfTransport(valid, probe: probe, release: release))
        var origin: AtlasSnapshotExportSource? = source(access: SnapshotExportGrant(probe, "root"))
        var destination: SnapshotExportGrant? = SnapshotExportGrant(probe, "destination")
        weak var rootLease = origin?.access as? SnapshotExportGrant
        weak var destinationLease = destination
        check(model!.save(source: origin!, destination: "/out/project.fcbs", destinationAccess: destination), "One explicit save is admitted")
        origin = nil; destination = nil
        await wait("writer did not enter") { probe.calls == 1 }
        check(rootLease != nil && destinationLease != nil, "Source and destination grants survive entered worker work")
        check(!model!.save(source: source("/other"), destination: "/out/other.fcbs", destinationAccess: nil),
            "A busy writer refuses a second request instead of queuing it")
        model!.cancel()
        check(model!.busy && model!.cancellationRequested && model!.receipt == nil,
            "Cancel remains pending until the actual writer outcome arrives")
        release.signal()
        await wait("late success did not drain") { model?.busy == false }
        check(model!.receipt?.succeeded == true && model!.canOpenSavedArchive,
            "Cancellation cannot discard an already completed save receipt")
        await wait("source grant did not drain") { rootLease == nil }
        var openedGrant: AnyObject?
        model!.openSavedArchive { path, digest, grant in
            check(path == "/out/project.fcbs" && digest == String(repeating: "a", count: 64)
                && grant === destinationLease, "Explicit open carries exact destination, saved digest and the same grant")
            openedGrant = grant
        }
        model!.cancel()
        check(model!.receipt?.succeeded == true && probe.calls == 1, "Repeated cancellation never retries or erases a completed effect")
        model = nil
        check(destinationLease != nil && openedGrant != nil, "The receiving archive opener can retain access independently")
        openedGrant = nil
        await wait("destination grant did not drain after both consumers") { destinationLease == nil }
        check(probe.released("root") && probe.released("destination"), "Both native lifetimes eventually retire")

        let before = SnapshotExportProbe()
        let canceled = AtlasSnapshotExportModel(transport: AtlasSnapshotExportTransport { _, _ in before.enter(); return valid })
        check(canceled.save(source: source(), destination: "/out/project.fcbs", destinationAccess: nil), "Queued request is admitted once")
        canceled.cancel()
        await wait("pre-entry cancellation did not drain") { !canceled.busy }
        check(before.calls == 0 && canceled.receipt == nil && canceled.notice.contains("before the writer started"),
            "Cancellation before worker entry proves no foreign call or destination effect")
        check(!canceled.save(source: source("relative"), destination: "/out/project.fcbs", destinationAccess: nil)
            && before.calls == 0, "Invalid source is refused before queuing")
        let grantA = SnapshotExportGrant(before, "a"), grantB = SnapshotExportGrant(before, "b"), project = UUID()
        let a = AtlasSnapshotExportSource(root: "/répo", project: project, browserOwner: 9, access: grantA)
        let b = AtlasSnapshotExportSource(root: "/re\u{301}po", project: project, browserOwner: 9, access: grantA)
        let other = AtlasSnapshotExportSource(root: a.root, project: project, browserOwner: 9, access: grantB)
        check(a.identity != b.identity && a.identity != other.identity,
            "Save-panel review cannot conflate normalized roots or replaced grants")
    }
    private static func SelfTransport(_ result: String, probe: SnapshotExportProbe,
        release: DispatchSemaphore) -> AtlasSnapshotExportTransport {
        AtlasSnapshotExportTransport { _, flag in
            precondition(!Thread.isMainThread)
            probe.enter()
            precondition(release.wait(timeout: .now() + .seconds(5)) == .success)
            precondition(flag.isCanceled)
            return result
        }
    }
    @MainActor private static func effectAndUnknownOutcomes() async throws {
        for effect in [AtlasSnapshotExportEffect.none, .incomplete, .written] {
            let value = try encode(failure(effect)), probe = SnapshotExportProbe()
            let model = AtlasSnapshotExportModel(transport: AtlasSnapshotExportTransport { _, flag in
                probe.enter(); flag.cancel(); return value
            })
            model.save(source: source(), destination: "/out/project.fcbs", destinationAccess: nil)
            await wait("effect receipt did not drain") { !model.busy }
            check(model.receipt?.effect == effect && !model.canOpenSavedArchive && probe.calls == 1,
                "An error effect remains explicit and never auto-opens or retries")
        }
        for value in [nil, "{}", try encode(success("/out/other.fcbs"))] {
            let probe = SnapshotExportProbe()
            let model = AtlasSnapshotExportModel(transport: AtlasSnapshotExportTransport { _, flag in
                probe.enter(); flag.cancel(); return value
            })
            model.save(source: source(), destination: "/out/project.fcbs", destinationAccess: nil)
            await wait("unverified result did not drain") { !model.busy }
            check(model.receipt == nil && !model.canOpenSavedArchive && model.notice.contains("could not be verified")
                && probe.calls == 1, "Lost or contradictory receipt keeps the outcome unknown without retry")
        }
    }
}
