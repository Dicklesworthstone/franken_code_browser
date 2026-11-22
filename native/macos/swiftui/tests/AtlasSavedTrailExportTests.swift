import Foundation
import Dispatch

/// Inject the foreign writer only. The production model owns worker isolation,
/// frozen trail admission, reply validation, effect recovery and grant lifetime.
private final class SavedTrailExportFixture: @unchecked Sendable {
    let trail = SavedTrailFixture()
    let resume = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var attempts: [UInt64: UInt64] = [:]
    private var receipts: [UInt64: (UInt64, String)] = [:]
    private var requests: [AtlasSavedTrailExportRequest] = []
    private var lookups = 0
    private var nextFault: String?
    private var lostReceipts = false
    private var blocked = false
    private var entered = false
    private var canceledMixedExport = false
    private func locked<T>(_ work: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return work() }
    var exports: [AtlasSavedTrailExportRequest] { locked { requests } }
    var receiptReads: Int { locked { lookups } }
    var didEnter: Bool { locked { entered } }
    var observedMixedCancellation: Bool { locked { canceledMixedExport } }
    func fault(_ name: String) { locked { nextFault = name } }
    func restore() { locked { lostReceipts = false } }
    func blockNext() { locked { blocked = true; entered = false } }
    var saved: AtlasSavedTransport {
        let inner = trail.saved
        return AtlasSavedTransport(create: inner.create, run: { [self] owner, request in
            guard let response = inner.run(owner, request) else { return nil }
            switch request {
            case .open, .info:
                return locked {
                    var out = trail.object(response)
                    out["last_trail_export_attempt"] = String(attempts[owner] ?? 0)
                    out["last_trail_export_operation"] = receipts[owner].map { String($0.0) as Any } ?? NSNull()
                    return trail.base.encode(out)
                }
            default: return response
            }
        }, cancel: inner.cancel, close: inner.close, retirementFailed: inner.retirementFailed, readerDomain: inner.readerDomain)
    }
    var transport: AtlasSavedTrailTransport {
        let inner = trail.trails
        let export: @Sendable (UInt64, AtlasSavedTrailExportRequest) -> String? = { [self] owner, request in
            precondition(!Thread.isMainThread)
            let state = locked { () -> (Bool, String?) in
                precondition(request.operation > (attempts[owner] ?? 0))
                attempts[owner] = request.operation; requests.append(request)
                let value = (blocked, nextFault); blocked = false; nextFault = nil; entered = true
                return value
            }
            if state.0 { precondition(resume.wait(timeout: .now() + .seconds(5)) == .success) }
            let response = trail.base.encode(reply(request, failure: state.1 == "refused"))
            return locked {
                receipts[owner] = (request.operation, response)
                if state.1 == "unknown" { lostReceipts = true }
                return state.1 == "lost" || state.1 == "unknown" ? nil : response
            }
        }
        return AtlasSavedTrailTransport(run: inner.run, export: { owner, request in
            precondition(request.additionalArchives.isEmpty, "supplemental authority reached the legacy ABI")
            return export(owner, request)
        }, exportReceipt: { [self] owner, operation in
            precondition(!Thread.isMainThread)
            return locked {
                lookups += 1
                guard !lostReceipts, let record = receipts[owner], record.0 == operation else { return nil }
                return record.1
            }
        }, exportWithArchives: { [self] owner, request, cancellation in
            precondition(!request.additionalArchives.isEmpty)
            let response = export(owner, request)
            locked { canceledMixedExport = cancellation.isCanceled }
            return response
        })
    }
    func reply(_ request: AtlasSavedTrailExportRequest, failure: Bool = false) -> [String: Any] {
        var out = trail.base.header(request.identity.owner, "trail-export")
        let l = request.limits, total = request.trail.items, unavailable = min(total, 5)
        out.merge([
            "status": failure ? "error" : "ok", "operation": String(request.operation),
            "last_trail_export_attempt": String(request.operation), "trail_generation": String(request.trail.generation),
            "trail_digest": request.trail.digest, "destination": trail.base.path(Array(request.destination.utf8), request.destination),
            "pack_schema": "fcb.trail-source-pack/1", "character_representation": "unicode-scalars-in-serialized-utf8-json",
            "archive_scope": "retained-single-archive",
            "budgets": ["entries": String(l.entries), "verification_source_bytes": String(l.verificationBytes),
                "selected_original_bytes": String(l.selectedBytes), "export_bytes": String(l.outputBytes), "export_characters": String(l.outputScalars)],
            "coverage": ["total_entries": String(total), "entries_visited": String(total),
                "verified_entries": String(total - unavailable), "unavailable_entries": String(unavailable),
                "omitted_entry_limit": "0", "omitted_source_limit": "0", "omitted_output_limit": "0",
                "verification_bytes_admitted": "500", "source_bytes_read": "500", "selected_original_bytes": "64"],
            "coverage_complete": false, "export_bytes": "6000", "export_characters": "5950",
            "export_digest": String(repeating: "a", count: 64), "member_bytes_read": "500", "member_read_calls": "29",
            "effect": failure ? "none" : "complete-file-and-parent-sync-requested",
            "bytes_written": failure ? "0" : "6000", "complete_artifact": !failure,
            "error": failure ? ["code": "SAVED_TRAIL_EXPORT_DESTINATION_EXISTS"] as Any : NSNull(),
            "archive_path_reopened": false, "live_source_reopened": false,
            "accepted_trail_unchanged": true, "accepted_query_unchanged": true,
            "source_derived_sensitive": true, "redacted": false, "encrypted": false,
            "overwritten": false, "power_loss_qualified": false
        ]) { _, new in new }
        if !request.additionalArchives.isEmpty {
            let count = UInt64(request.additionalArchives.count)
            out["archive_scope"] = "retained-primary-and-explicit-supplemental-archives"
            out["additional_archives"] = request.additionalArchives.enumerated().map { index, path in
                ["ordinal": String(index), "path": trail.base.path(Array(path.utf8), path), "opened": true,
                 "digest": index == 0 ? String(repeating: "b", count: 64) : request.identity.digest] as [String: Any]
            }
            out["archive_validation"] = [
                "routing": "verified-archive-digest", "duplicate_policy": "first-verified-handle",
                "supplied_archives": String(count + 1), "validated_archives": String(count + 1),
                "distinct_archives": "2", "duplicate_archives": String(count - 1), "opened_archive_paths": String(count),
                "validation_complete": true, "validation_bytes_admitted": "4096", "validation_bytes_read": "4094",
                "validation_read_calls": "12", "retained_directory_bytes": "512", "peak_directory_bytes": "768",
                "max_archives": "8", "max_validation_bytes": String(l.archiveValidationBytes), "max_directory_bytes": "67108864",
                "validation_admission": "observed-file-length-plus-one-eof-probe",
                "validation_scope": "explicit-supplemental-archive-envelopes"
            ] as [String: Any]
        }
        return out
    }
}

enum AtlasSavedTrailExportTests {
    @MainActor private static func wait(_ condition: @escaping @MainActor () -> Bool) async {
        for _ in 0..<3000 {
            if condition() { return }
            try? await Task.sleep(for: .milliseconds(1))
        }
        preconditionFailure("Saved trail export did not reach the expected state")
    }
    @MainActor static func run() async throws {
        let fixture = SavedTrailExportFixture()
        let model = AtlasSavedRepositoryModel(transport: fixture.saved, trails: fixture.transport)
        model.open(path: "/saved/repository.fcbs", accessLease: nil)
        await wait { !model.busy }
        model.attachTrail(path: "/saved/guide.fcbt", accessLease: nil)
        await wait { !model.busy }
        let parent = AtlasSavedTrailParent(model.trailPage!)
        precondition(model.canExportTrail)
        let request = model.exportTrail(parent, destination: "/export/source.json", limits: .init(), accessLease: nil)!
        await wait { !model.busy }
        precondition(model.trailExportOutcomeKnown && model.trailExportReceipt?.succeeded == true)
        precondition(model.trailExportReceipt?.completeArtifact == true && model.trailExportReceipt?.coverageComplete == false)
        precondition(model.trailExportReceipt?.unavailable == 5 && parent.matches(model.trailPage!))
        precondition(fixture.exports.count == 1 && fixture.receiptReads == 0)

        for key in ["archive_digest", "trail_digest", "trail_generation", "coverage_complete", "export_characters", "complete_artifact"] {
            var invalid = fixture.reply(request)
            switch key {
            case "archive_digest", "trail_digest": invalid[key] = String(repeating: "b", count: 64)
            case "trail_generation": invalid[key] = "9"
            case "coverage_complete": invalid[key] = true
            case "export_characters": invalid[key] = "6001"
            default: invalid[key] = false
            }
            do {
                _ = try AtlasSavedTrailExportReceipt.decode(fixture.trail.base.encode(invalid), request: request)
                preconditionFailure("Invalid export receipt admitted: \(key)")
            } catch {}
        }
        fixture.fault("lost")
        _ = model.exportTrail(parent, destination: "/export/lost.json", limits: .init(), accessLease: nil)
        await wait { !model.busy }
        precondition(fixture.exports.count == 2 && fixture.receiptReads == 1 && model.trailExportOutcomeKnown)
        fixture.fault("unknown")
        _ = model.exportTrail(parent, destination: "/export/unknown.json", limits: .init(), accessLease: nil)
        await wait { !model.busy }
        precondition(!model.trailExportOutcomeKnown && fixture.exports.count == 3)
        precondition(model.exportTrail(parent, destination: "/export/unknown.json", limits: .init(), accessLease: nil) == nil)
        precondition(fixture.exports.count == 3, "unknown write was automatically retried")
        fixture.restore(); model.reloadTrailExportReceipt(); await wait { !model.busy }
        precondition(model.trailExportOutcomeKnown && fixture.exports.count == 3)
        fixture.fault("refused")
        _ = model.exportTrail(parent, destination: "/export/existing.json", limits: .init(), accessLease: nil)
        await wait { !model.busy }
        precondition(model.trailExportOutcomeKnown && model.trailExportReceipt?.effect == .none)
        precondition(model.trailExportReceipt?.succeeded == false && parent.matches(model.trailPage!))

        final class Grant {}
        var grant: Grant? = Grant(); weak var weakGrant = grant
        fixture.blockNext()
        _ = model.exportTrail(parent, destination: "/export/canceled.json", limits: .init(), accessLease: grant)
        grant = nil
        await wait { fixture.didEnter }
        model.cancel()
        precondition(model.busy && weakGrant != nil && !model.canExportTrail)
        fixture.resume.signal(); await wait { !model.busy }
        await wait { weakGrant == nil }
        precondition(model.trailExportOutcomeKnown && model.trailExportReceipt?.completeArtifact == true,
            "late cancellation hid the actual completed write")
        precondition(fixture.exports.count == 5)

        model.attachTrail(path: "/saved/new.fcbt", accessLease: nil)
        await wait { !model.busy }
        precondition(model.exportTrail(parent, destination: "/export/stale.json", limits: .init(), accessLease: nil) == nil)
        let current = AtlasSavedTrailParent(model.trailPage!)
        var closingGrant: Grant? = Grant(); weak var weakClosingGrant = closingGrant
        fixture.blockNext()
        _ = model.exportTrail(current, destination: "/export/closed.json", limits: .init(), accessLease: closingGrant)
        closingGrant = nil; await wait { fixture.didEnter }
        model.close(); precondition(weakClosingGrant != nil && model.busy)
        fixture.resume.signal(); await wait { !model.busy }; await wait { weakClosingGrant == nil }
        precondition(model.closed && model.trailExportReceipt == nil && fixture.exports.count == 6)
        try await mixedArchiveExports()
        print("Saved trail export identity, mixed archive admission, receipt recovery, cancel/close and grant-drain fixtures passed")
    }
    @MainActor private static func mixedArchiveExports() async throws {
        let fixture = SavedTrailExportFixture()
        let model = AtlasSavedRepositoryModel(transport: fixture.saved, trails: fixture.transport)
        model.open(path: "/saved/repository.fcbs", accessLease: nil); await wait { !model.busy }
        model.attachTrail(path: "/saved/guide.fcbt", accessLease: nil); await wait { !model.busy }
        let parent = AtlasSavedTrailParent(model.trailPage!)
        // Canonically equivalent Unicode names remain separate exact paths.
        let paths = ["/saved/e\u{301}.fcbs", "/saved/é.fcbs"]
        precondition(model.canExportTrailWithArchives)
        let request = model.exportTrail(parent, destination: "/export/mixed.json", limits: .init(),
            accessLease: nil, additionalArchives: paths)!
        await wait { !model.busy }
        let receipt = model.trailExportReceipt!
        precondition(receipt.succeeded && receipt.completeArtifact && !receipt.coverageComplete)
        precondition(receipt.archiveValidation?.supplied == 3 && receipt.archiveValidation?.validated == 3)
        precondition(receipt.archiveValidation?.distinct == 2 && receipt.archiveValidation?.duplicates == 1)
        precondition(receipt.archiveValidation?.openedPaths == 2 && receipt.archiveValidation?.bytesRead == 4094)
        precondition(parent.matches(model.trailPage!) && fixture.exports.count == 1)
        precondition(fixture.exports[0].additionalArchives[0].utf8.elementsEqual(paths[0].utf8))

        for invalidPaths in [
            Array(repeating: "/saved/extra.fcbs", count: 8), ["relative.fcbs"], ["/saved/nul\0.fcbs"],
            ["/saved/../extra.fcbs"], ["/" + String(repeating: "a", count: 16_384)], [request.destination]
        ] {
            do {
                _ = try AtlasSavedTrailExportRequest(trail: parent, operation: 2, destination: request.destination,
                    limits: .init(), additionalArchives: invalidPaths)
                preconditionFailure("Invalid supplemental archive authority admitted")
            } catch {}
        }
        var excessive = AtlasSavedTrailExportLimits()
        excessive.archiveValidationBytes += 1
        do {
            _ = try AtlasSavedTrailExportRequest(trail: parent, operation: 2, destination: "/export/limit.json",
                limits: excessive, additionalArchives: paths)
            preconditionFailure("Unbounded archive validation admitted")
        } catch {}
        precondition(model.exportTrail(parent, destination: paths[0], limits: .init(), accessLease: nil,
            additionalArchives: paths) == nil)
        precondition(fixture.exports.count == 1)

        func rejected(_ object: [String: Any]) {
            do {
                _ = try AtlasSavedTrailExportReceipt.decode(fixture.trail.base.encode(object), request: request)
                preconditionFailure("Malformed mixed-archive receipt admitted")
            } catch {}
        }
        for key in ["supplied_archives", "validated_archives", "distinct_archives", "duplicate_archives",
                    "opened_archive_paths", "validation_bytes_read", "validation_bytes_admitted",
                    "retained_directory_bytes", "peak_directory_bytes", "max_archives", "max_validation_bytes",
                    "max_directory_bytes", "validation_read_calls", "routing", "duplicate_policy",
                    "validation_admission", "validation_scope", "validation_complete"] {
            var bad = fixture.reply(request), validation = bad["archive_validation"] as! [String: Any]
            switch key {
            case "supplied_archives", "validated_archives", "distinct_archives", "opened_archive_paths": validation[key] = "4"
            case "duplicate_archives": validation[key] = "0"
            case "validation_bytes_read": validation[key] = "4097"
            case "validation_bytes_admitted": validation[key] = "268435457"
            case "retained_directory_bytes": validation[key] = "769"
            case "peak_directory_bytes": validation[key] = "67108865"
            case "validation_read_calls": validation[key] = "01"
            case "validation_complete": validation[key] = false
            default: validation[key] = "wrong"
            }
            bad["archive_validation"] = validation; rejected(bad)
        }
        for kind in ["missing-validation", "missing-inventory", "inventory-count", "path", "ordinal", "digest", "unopened", "scope"] {
            var bad = fixture.reply(request)
            var archives = bad["additional_archives"] as! [[String: Any]]
            switch kind {
            case "missing-validation": bad.removeValue(forKey: "archive_validation")
            case "missing-inventory": bad.removeValue(forKey: "additional_archives")
            case "inventory-count": archives.removeLast(); bad["additional_archives"] = archives
            case "path":
                archives[0]["path"] = fixture.trail.base.path(Array(paths[1].utf8), paths[1])
                bad["additional_archives"] = archives
            case "ordinal": archives[0]["ordinal"] = "1"; bad["additional_archives"] = archives
            case "digest": archives[0]["digest"] = "not-a-digest"; bad["additional_archives"] = archives
            case "unopened": archives[0]["opened"] = false; bad["additional_archives"] = archives
            default: bad["archive_scope"] = "retained-single-archive"
            }
            rejected(bad)
        }
        // A failed envelope may have opened and read one candidate, but cannot
        // claim any source selection or destination effect.
        var failed = fixture.reply(request)
        var archives = failed["additional_archives"] as! [[String: Any]]
        archives[0]["digest"] = NSNull()
        archives[1]["digest"] = NSNull(); archives[1]["opened"] = false
        failed["additional_archives"] = archives
        var validation = failed["archive_validation"] as! [String: Any]
        for key in ["validated_archives", "distinct_archives", "opened_archive_paths"] { validation[key] = "1" }
        validation["duplicate_archives"] = "0"; validation["validation_complete"] = false
        failed["archive_validation"] = validation
        failed["status"] = "error"; failed["effect"] = "none"
        failed["error"] = ["code": "SAVED_TRAIL_EXPORT_ARCHIVE_INVALID"]
        failed["complete_artifact"] = false; failed["export_digest"] = NSNull()
        for key in ["export_bytes", "export_characters", "bytes_written", "member_bytes_read", "member_read_calls"] { failed[key] = "0" }
        var coverage = failed["coverage"] as! [String: String]
        for key in Array(coverage.keys) where key != "total_entries" { coverage[key] = "0" }
        failed["coverage"] = coverage
        let failedReceipt = try AtlasSavedTrailExportReceipt.decode(fixture.trail.base.encode(failed), request: request)
        precondition(!failedReceipt.succeeded && failedReceipt.effect == .none && failedReceipt.archiveValidation?.complete == false)
        var falseSuccess = failed
        falseSuccess["status"] = "ok"; falseSuccess["error"] = NSNull(); rejected(falseSuccess)

        // Seven maximum-length paths require a larger bounded receipt cap;
        // escaped display bytes must not turn a valid receipt into unknown work.
        let longPaths = (0..<7).map { "/\(String(repeating: "\n", count: 16_370))\($0).fcbs" }
        let largeRequest = try AtlasSavedTrailExportRequest(trail: parent, operation: 2, destination: "/export/large.json",
            limits: .init(), additionalArchives: longPaths)
        let largeReply = fixture.trail.base.encode(fixture.reply(largeRequest))
        precondition(largeReply.utf8.count > 256 * 1024 && largeReply.utf8.count < 2 * 1024 * 1024)
        _ = try AtlasSavedTrailExportReceipt.decode(largeReply, request: largeRequest)

        fixture.fault("lost")
        _ = model.exportTrail(parent, destination: "/export/mixed-lost.json", limits: .init(), accessLease: nil,
            additionalArchives: paths)
        await wait { !model.busy }
        precondition(model.trailExportOutcomeKnown && fixture.exports.count == 2 && fixture.receiptReads == 1)

        final class Grant {}
        final class GrantGroup {
            let files: [Grant]
            init(_ files: [Grant]) { self.files = files }
        }
        var first: Grant? = Grant(), second: Grant? = Grant(), destination: Grant? = Grant()
        weak var weakFirst = first, weakSecond = second, weakDestination = destination
        var group: GrantGroup? = GrantGroup([first!, second!])
        fixture.blockNext()
        _ = model.exportTrail(parent, destination: "/export/mixed-canceled.json", limits: .init(), accessLease: destination,
            additionalArchives: paths, archiveAccessLease: group)
        first = nil; second = nil; destination = nil; group = nil
        await wait { fixture.didEnter }
        model.cancel()
        precondition(model.busy && !model.canExportTrailWithArchives)
        precondition(weakFirst != nil && weakSecond != nil && weakDestination != nil)
        fixture.resume.signal(); await wait { !model.busy }
        await wait { weakFirst == nil && weakSecond == nil && weakDestination == nil }
        precondition(fixture.observedMixedCancellation && fixture.exports.count == 3)
        precondition(model.trailExportOutcomeKnown && model.trailExportReceipt?.completeArtifact == true)
        precondition(parent.matches(model.trailPage!))

        var closing: Grant? = Grant(); weak var weakClosing = closing
        fixture.blockNext()
        _ = model.exportTrail(parent, destination: "/export/mixed-closed.json", limits: .init(), accessLease: nil,
            additionalArchives: paths, archiveAccessLease: closing)
        closing = nil; await wait { fixture.didEnter }
        model.close(); precondition(weakClosing != nil && model.busy)
        fixture.resume.signal(); await wait { !model.busy }; await wait { weakClosing == nil }
        precondition(model.closed && model.trailExportReceipt == nil && fixture.exports.count == 4)

        var legacy = fixture.transport
        legacy.exportWithArchives = nil
        let legacyModel = AtlasSavedRepositoryModel(transport: fixture.saved, trails: legacy)
        legacyModel.open(path: "/saved/repository.fcbs", accessLease: nil); await wait { !legacyModel.busy }
        legacyModel.attachTrail(path: "/saved/guide.fcbt", accessLease: nil); await wait { !legacyModel.busy }
        precondition(legacyModel.canExportTrail && !legacyModel.canExportTrailWithArchives)
        let legacyParent = AtlasSavedTrailParent(legacyModel.trailPage!)
        precondition(legacyModel.exportTrail(legacyParent, destination: "/export/unsupported.json", limits: .init(),
            accessLease: nil, additionalArchives: paths) == nil)
        precondition(fixture.exports.count == 4)
        legacyModel.close()
    }

}
