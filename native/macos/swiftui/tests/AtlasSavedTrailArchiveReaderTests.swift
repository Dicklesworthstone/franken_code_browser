import Foundation
import Dispatch

/// Inject only the new foreign handoff. Existing retained-reader fixtures own
/// reader admission, exact bytes, pages and retirement; Rust fixtures prove
/// actual selected-archive envelope/digest/member resolution.
private final class SavedTrailArchiveReaderFixture: @unchecked Sendable {
    let trail = SavedTrailFixture()
    let resume = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var visits: [(UInt64, AtlasSavedTrailArchiveReaderRequest)] = []
    private var response: String?
    private var nextFault: String?
    private var blocked = false
    private var entered = false
    private var canceled = false
    private func locked<T>(_ work: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return work() }
    var requests: [(UInt64, AtlasSavedTrailArchiveReaderRequest)] { locked { visits } }
    var lastReply: String? { locked { response } }
    var didEnter: Bool { locked { entered } }
    var observedCancellation: Bool { locked { canceled } }
    func fault(_ value: String) { locked { nextFault = value } }
    func blockNext() { locked { blocked = true; entered = false; canceled = false } }
    var saved: AtlasSavedTransport {
        let inner = trail.saved
        return AtlasSavedTransport(create: inner.create, run: { [self] owner, request in
            guard let response = inner.run(owner, request) else { return nil }
            switch request {
            case .open, .info:
                var out = trail.object(response)
                out["last_trail_write_attempt"] = "0"; out["last_trail_write_operation"] = NSNull()
                return trail.base.encode(out)
            default: return response
            }
        }, cancel: inner.cancel, close: inner.close, retirementFailed: inner.retirementFailed, readerDomain: inner.readerDomain)
    }
    var transport: AtlasSavedTrailTransport {
        var value = trail.trails
        value.write = { _, _ in preconditionFailure("supplemental source entered primary authoring") }
        value.writeReceipt = { _, _ in nil }
        value.openArchiveReader = { [self] owner, destination, request, cancellation in
            precondition(!Thread.isMainThread && owner == request.trail.identity.owner)
            let state = locked { () -> (Bool, String?) in
                visits.append((destination, request)); entered = true
                let state = (blocked, nextFault); blocked = false; nextFault = nil; return state
            }
            if state.0 { precondition(resume.wait(timeout: .now() + .seconds(5)) == .success) }
            locked { canceled = cancellation.isCanceled }
            if state.1 == "engine-error" {
                return trail.base.encode(["schema": "fcb.saved-repository/1", "status": "error",
                    "owner": String(owner), "error": ["code": "TRAIL_OTHER_ARCHIVE"]])
            }
            guard let initialized = trail.base.savedTransport.run(owner, .openMember(reader: destination, member: 0)) else { return nil }
            var out = trail.object(initialized)
            let entry = request.entry
            out.merge([
                "command": "trail-archive-open-reader", "trail_generation": String(request.trail.generation),
                "trail_digest": request.trail.digest, "item": String(entry.id), "entry": trail.entry(entry.id),
                "member": "0", "file_id": String(request.trail.identity.members + 17),
                "source_revision": String(request.trail.identity.members + 17),
                "source_digest": entry.sourceDigest, "source_bytes": String(entry.sourceBytes),
                "path": trail.base.path(entry.path.bytes, entry.path.display),
                "offset": String(entry.range.start), "selection": ["start": String(entry.range.start), "end": String(entry.range.end)],
                "resolved_member": trail.base.memberObject(0),
                "source_archive": ["path": trail.base.path(Array(request.path.utf8), request.path),
                    "digest": entry.archiveDigest, "members_total": "5", "captured_members": "4", "discovery_complete": true],
                "archive_validation": [
                    "routing": "verified-archive-digest", "duplicate_policy": "first-verified-handle",
                    "supplied_archives": "2", "validated_archives": "2", "distinct_archives": "2",
                    "duplicate_archives": "0", "opened_archive_paths": "1", "validation_complete": true,
                    "validation_bytes_admitted": "4096", "validation_bytes_read": "4095", "validation_read_calls": "9",
                    "retained_directory_bytes": "512", "peak_directory_bytes": "512", "max_archives": "8",
                    "max_validation_bytes": String(request.validationBytes), "max_directory_bytes": "67108864",
                    "validation_admission": "observed-file-length-plus-one-eof-probe",
                    "validation_scope": "explicit-supplemental-archive-envelopes"
                ],
                "archive_scope": "explicit-supplemental-archive", "archive_path_reopened": false,
                "accepted_trail_unchanged": true, "accepted_query_unchanged": true,
                "primary_member_bytes_read": "0", "primary_member_read_calls": "0"
            ]) { _, next in next }
            if state.1 == "bad-digest" {
                var source = out["source_archive"] as! [String: Any]
                source["digest"] = String(repeating: "f", count: 64); out["source_archive"] = source
            }
            let json = trail.base.encode(out)
            locked { response = json }
            return state.1 == "lost" ? nil : json
        }
        return value
    }
}

enum AtlasSavedTrailArchiveReaderTests {
    @MainActor private static func wait(_ condition: @escaping @MainActor () -> Bool) async {
        for _ in 0..<3000 { if condition() { return }; try? await Task.sleep(for: .milliseconds(1)) }
        preconditionFailure("Selected-archive trail reader did not reach the expected state")
    }
    @MainActor static func run() async throws {
        let fixture = SavedTrailArchiveReaderFixture()
        let model = AtlasSavedRepositoryModel(transport: fixture.saved, trails: fixture.transport)
        model.open(path: "/saved/primary.fcbs", accessLease: nil); await wait { !model.busy }
        model.attachTrail(path: "/saved/guide.fcbt", accessLease: nil); await wait { !model.busy }
        let page = model.trailPage!, entry = page.items[1], identity = model.info!.identity
        let selectedMember = model.selectedMember?.id, queryGeneration = model.info?.queryGeneration
        let initial = try await model.openTrailItem(page.items[0], from: page, readerTransport: fixture.trail.base.readerTransport())
        precondition(model.canAuthorTrail(from: initial) && model.canFollowDocumentLinks(from: initial))
        precondition(model.canOpenTrailArchive(entry, from: page))
        let path = "/saved/other-e\u{301}.fcbs"
        let external = try await model.openTrailArchive(entry, from: page, path: path, accessLease: nil,
            readerTransport: fixture.trail.base.readerTransport())
        precondition(external.canUseTrailRange && !external.reader.canRefresh && external.archiveIdentity == nil)
        precondition(external.supplementalTrailSource?.archiveDigest == entry.archiveDigest)
        precondition(external.trailNavigationIdentity == identity && model.canNavigateTrail(from: external))
        precondition(!model.canFollowDocumentLinks(from: external) && !model.canAuthorTrail(from: external))
        precondition(model.info?.identity == identity && model.trailPage?.matchesAttachment(page) == true)
        precondition(model.selectedMember?.id == selectedMember && model.info?.queryGeneration == queryGeneration)
        precondition(model.selectedTrailItem == entry.id)
        let handle = try await external.withTrailReader { $0 }
        precondition(handle == external.reader.page?.identity.owner, "exact range lost its independently retained reader")

        var history = AtlasSavedReaderHistory(initial)
        precondition(history.accept(external), "same-trail navigation across archive identities was rejected")
        let later = try await model.openTrailItem(at: 6, from: external, readerTransport: fixture.trail.base.readerTransport())
        precondition(later.archiveIdentity == identity && history.accept(later))
        history.back(); precondition(history.current.id == external.id && history.current.canUseTrailRange)
        history.back(); precondition(history.current.id == initial.id)
        history.forward(); precondition(history.current.id == external.id)

        let (destination, request) = fixture.requests[0]
        let valid = fixture.trail.object(fixture.lastReply)
        for kind in ["command", "owner", "source-digest", "archive-digest", "archive-path", "file", "revision",
            "member", "range", "entry", "catalog", "source-count", "validation", "validation-budget", "validation-counts",
            "primary-read", "changed-trail", "reopened-primary", "reader-digest", "window"] {
            var bad = valid
            switch kind {
            case "command": bad["command"] = "trail-open-reader"
            case "owner": bad["owner"] = String(identity.owner + 1)
            case "source-digest": bad["source_digest"] = String(repeating: "f", count: 64)
            case "archive-digest", "archive-path", "source-count":
                var source = bad["source_archive"] as! [String: Any]
                if kind == "archive-digest" { source["digest"] = identity.digest }
                else if kind == "source-count" { source["members_total"] = "0" }
                else {
                    let changed = "/saved/other-é.fcbs"
                    source["path"] = fixture.trail.base.path(Array(changed.utf8), changed)
                }
                bad["source_archive"] = source
            case "file": bad["file_id"] = String(identity.members)
            case "revision": bad["source_revision"] = "18446744073709551615"
            case "member": bad["member"] = "1"
            case "range": bad["selection"] = ["start": "1", "end": "1"]
            case "entry": var row = bad["entry"] as! [String: Any]; row["rationale"] = "different note"; bad["entry"] = row
            case "catalog":
                var member = bad["resolved_member"] as! [String: Any]; member["source_digest"] = identity.digest
                bad["resolved_member"] = member
            case "validation", "validation-budget", "validation-counts":
                var validation = bad["archive_validation"] as! [String: Any]
                if kind == "validation" { validation["validation_complete"] = false }
                else if kind == "validation-budget" { validation["validation_bytes_read"] = "268435457" }
                else { validation["distinct_archives"] = "1" }
                bad["archive_validation"] = validation
            case "primary-read": bad["primary_member_bytes_read"] = "1"
            case "changed-trail": bad["accepted_trail_unchanged"] = false
            case "reopened-primary": bad["archive_path_reopened"] = true
            case "reader-digest":
                var reader = bad["reader"] as! [String: Any]; reader["capture_sha256"] = identity.digest; bad["reader"] = reader
            default:
                var window = bad["window"] as! [String: Any]; window["owner"] = String(destination + 1); bad["window"] = window
            }
            do {
                _ = try AtlasSavedTrailArchiveReaderReceipt.decode(fixture.trail.base.encode(bad), request: request, destination: destination)
                preconditionFailure("Malformed selected-archive handoff admitted: \(kind)")
            } catch {}
        }
        for invalidPath in ["relative.fcbs", "/nul\0.fcbs", "/a/../b.fcbs", "/" + String(repeating: "x", count: 16_384)] {
            do {
                _ = try AtlasSavedTrailArchiveReaderRequest(trail: request.trail, entry: entry, path: invalidPath)
                preconditionFailure("Invalid selected archive path admitted")
            } catch {}
        }
        do {
            _ = try AtlasSavedTrailArchiveReaderRequest(trail: request.trail, entry: page.items[0], path: path)
            preconditionFailure("Current-archive entry granted a supplemental open")
        } catch {}
        do {
            _ = try AtlasSavedTrailArchiveReaderRequest(trail: request.trail, entry: entry, path: path, validationBytes: 268435457)
            preconditionFailure("Unbounded archive validation admitted")
        } catch {}

        for fault in ["engine-error", "bad-digest", "lost"] {
            let count = fixture.requests.count
            fixture.fault(fault)
            do {
                _ = try await model.openTrailArchive(entry, from: page, path: path, accessLease: nil,
                    readerTransport: fixture.trail.base.readerTransport())
                preconditionFailure("Failed selected-archive handoff replaced the current reader")
            } catch {}
            let rejected = fixture.requests.last!.0
            await wait { fixture.trail.base.calls.contains("reader-close:\(rejected)") }
            precondition(fixture.requests.count == count + 1 && !model.busy)
            precondition(history.current.id == external.id && external.canUseTrailRange && initial.canUseTrailRange)
            precondition(model.info?.identity == identity && model.trailPage?.matchesAttachment(page) == true)
        }

        final class Grant {}
        @MainActor func begin(_ grant: AnyObject?) -> Task<AtlasSavedReaderCapture, Error> {
            Task {
                try await model.openTrailArchive(entry, from: page, path: path, accessLease: grant,
                    readerTransport: fixture.trail.base.readerTransport())
            }
        }
        var grant: Grant? = Grant(); weak var weakGrant = grant
        fixture.blockNext(); let canceled = begin(grant); grant = nil
        await wait { fixture.didEnter }
        canceled.cancel()
        precondition(model.busy && weakGrant != nil && !model.canOpenTrailArchive(entry, from: page))
        fixture.resume.signal()
        do { _ = try await canceled.value; preconditionFailure("Canceled selected-archive reader delivered") } catch {}
        await wait { !model.busy && weakGrant == nil }
        precondition(fixture.observedCancellation && external.canUseTrailRange)

        model.attachTrail(path: "/saved/replacement.fcbt", accessLease: nil); await wait { !model.busy }
        precondition(!model.canNavigateTrail(from: external) && external.canUseTrailRange)
        let visitsBeforeStale = fixture.requests.count
        do {
            _ = try await model.openTrailArchive(entry, from: page, path: path, accessLease: nil,
                readerTransport: fixture.trail.base.readerTransport())
            preconditionFailure("Stale attached trail authorized source opening")
        } catch {}
        precondition(fixture.requests.count == visitsBeforeStale)
        let replacement = model.trailPage!, nextEntry = replacement.items[1]
        var closingGrant: Grant? = Grant(); weak var weakClosingGrant = closingGrant
        @MainActor func beginClosing(_ grant: AnyObject?) -> Task<AtlasSavedReaderCapture, Error> {
            Task {
                try await model.openTrailArchive(nextEntry, from: replacement, path: path, accessLease: grant,
                    readerTransport: fixture.trail.base.readerTransport())
            }
        }
        fixture.blockNext(); let closing = beginClosing(closingGrant); closingGrant = nil
        await wait { fixture.didEnter }
        model.close(); precondition(weakClosingGrant != nil && model.busy)
        fixture.resume.signal()
        do { _ = try await closing.value; preconditionFailure("Closed primary delivered a pending trail reader") } catch {}
        await wait { !model.busy && weakClosingGrant == nil }
        precondition(external.canUseTrailRange && initial.canUseTrailRange && !model.canNavigateTrail(from: external))
        precondition(!fixture.trail.base.calls.contains("LIVE-OPEN"))
        history.close()

        let legacy = AtlasSavedRepositoryModel(transport: fixture.saved, trails: fixture.trail.trails)
        legacy.open(path: "/saved/legacy.fcbs", accessLease: nil); await wait { !legacy.busy }
        legacy.attachTrail(path: "/saved/guide.fcbt", accessLease: nil); await wait { !legacy.busy }
        precondition(!legacy.canOpenTrailArchive(legacy.trailPage!.items[1], from: legacy.trailPage!))
        legacy.close()
        print("Selected-archive trail reader identity, history, exact ranges, refusal, cancellation and grant lifetime fixtures passed")
    }
}
