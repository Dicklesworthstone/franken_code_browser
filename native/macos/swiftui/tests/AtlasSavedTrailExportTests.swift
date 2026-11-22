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
    private func locked<T>(_ work: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return work() }
    var exports: [AtlasSavedTrailExportRequest] { locked { requests } }
    var receiptReads: Int { locked { lookups } }
    var didEnter: Bool { locked { entered } }
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
        return AtlasSavedTrailTransport(run: inner.run, export: { [self] owner, request in
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
        }, exportReceipt: { [self] owner, operation in
            precondition(!Thread.isMainThread)
            return locked {
                lookups += 1
                guard !lostReceipts, let record = receipts[owner], record.0 == operation else { return nil }
                return record.1
            }
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
        print("Saved trail export identity, partial coverage, receipt recovery, cancel/close and grant-drain fixtures passed")
    }
}
