import Foundation
import Dispatch

/// Fixed writer outcomes exercise native receipt admission and ownership. The
/// production Rust fixtures own index construction, file publication and pins.
private final class SavedIndexBuildFixture: @unchecked Sendable {
    struct Call: Sendable {
        let owner, operation: UInt64
        let destination: String
        let limits: AtlasSavedIndexBuildLimits
    }
    let base = SavedRepositoryFixture(searchVectors: true)
    let entered = DispatchSemaphore(value: 0), resume = DispatchSemaphore(value: 0)
    let pin = String(repeating: "c", count: 64)
    private let lock = NSLock()
    private var attempts: [UInt64: UInt64] = [:]
    private var receipts: [UInt64: (UInt64, String)] = [:]
    private var operations: [Call] = []
    private var receiptOperations: [UInt64] = []
    private var blocked = false
    private var fault: String?
    private var failReceipts = 0
    private func locked<T>(_ body: () -> T) -> T { lock.lock(); defer { lock.unlock() }; return body() }
    var calls: [Call] { locked { operations } }
    var receiptReads: [UInt64] { locked { receiptOperations } }
    func corruptNext(_ kind: String, receiptFailures: Int = 0) {
        locked { fault = kind; failReceipts = receiptFailures }
    }
    func blockNextBuild() { locked { blocked = true } }
    func object(_ json: String) -> [String: Any] {
        try! JSONSerialization.jsonObject(with: Data(json.utf8)) as! [String: Any]
    }
    func receipt(_ call: Call, effect: AtlasSavedIndexBuildEffect = .fileAndParentSyncRequested,
        succeeded: Bool = true, encoded: Bool = true, hasStats: Bool = true) -> [String: Any] {
        var out = base.header(call.owner, "index-build")
        let bytes: UInt64 = encoded ? 4096 : 0
        let written: UInt64 = effect.completeFile ? bytes : (effect == .incomplete ? 17 : 0)
        out.merge([
            "status": succeeded ? "ok" : "error", "operation": String(call.operation),
            "last_index_build_attempt": String(call.operation),
            "destination": base.path(Array(call.destination.utf8), call.destination),
            "max_file_bytes": String(call.limits.fileBytes), "max_source_bytes": String(call.limits.sourceBytes),
            "max_grams": String(call.limits.grams), "max_grams_per_file": "65536", "max_scratch_bytes": "1048576",
            "max_index_bytes": "25165824", "effect": effect.rawValue, "bytes_written": String(written),
            "index_bytes": String(bytes), "index_manifest_bytes": encoded ? "512" : "0",
            "index_pin": encoded ? pin as Any : NSNull(), "index_kind": "demand-paged-postings-v1",
            "index_pin_scope": "page-manifest-envelope-sha256", "index_members": hasStats ? "5" as Any : NSNull(),
            "index_indexed_files": hasStats ? "3" as Any : NSNull(), "index_uncovered_files": hasStats ? "1" as Any : NSNull(),
            "index_unavailable_files": hasStats ? "1" as Any : NSNull(), "index_unique_grams": hasStats ? "123" as Any : NSNull(),
            "index_build_source_bytes": hasStats ? "1000" as Any : NSNull(), "coverage_complete": false,
            "member_bytes_read": hasStats ? "1000" : "0", "member_read_calls": hasStats ? "4" : "0",
            "attach_eligible": effect.completeFile && encoded, "index_attached": false,
            "accepted_index_unchanged": true, "accepted_query_unchanged": true,
            "source_derived_sensitive": true, "encrypted": false, "overwritten": false, "power_loss_qualified": false,
            "error": succeeded ? NSNull() : ["code": "SAVED_INDEX_WRITE_CANCELED", "next_action": "Inspect the chosen destination before an explicit retry."]
        ]) { _, next in next }
        return out
    }
    var transport: AtlasSavedTransport {
        let saved = base.savedTransport
        return AtlasSavedTransport(create: saved.create, run: { [self] owner, request in
            switch request {
            case .open, .info:
                guard let reply = saved.run(owner, request) else { return nil }
                return locked {
                    var out = object(reply)
                    out["last_index_build_attempt"] = String(attempts[owner] ?? 0)
                    out["last_index_build_operation"] = receipts[owner].map { String($0.0) as Any } ?? NSNull()
                    return base.encode(out)
                }
            case .buildIndex(let operation, let destination, let limits):
                precondition(!Thread.isMainThread)
                let call = Call(owner: owner, operation: operation, destination: destination, limits: limits)
                let enteredState: (Bool, String?)? = locked {
                    guard operation > (attempts[owner] ?? 0) else { return nil }
                    attempts[owner] = operation; operations.append(call)
                    let value = (blocked, fault); blocked = false; fault = nil; return value
                }
                guard let (pause, kind) = enteredState else { return nil }
                if pause {
                    entered.signal()
                    precondition(resume.wait(timeout: .now() + .seconds(5)) == .success)
                }
                let effect: AtlasSavedIndexBuildEffect = kind == "none" ? .none
                    : kind == "partial" ? .incomplete : kind == "unsynced" ? .fileSyncUnconfirmed : .fileAndParentSyncRequested
                let terminal = receipt(call, effect: effect, succeeded: !["none", "partial", "unsynced"].contains(kind ?? ""))
                let encoded = base.encode(terminal)
                locked { receipts[owner] = (operation, encoded) }
                if kind == "lost" { return nil }
                if kind == "wrong-reply" {
                    var corrupted = terminal; corrupted["operation"] = String(operation + 1)
                    return base.encode(corrupted)
                }
                return encoded
            case .indexBuildReceipt(let operation):
                precondition(!Thread.isMainThread)
                return locked {
                    receiptOperations.append(operation)
                    if failReceipts > 0 { failReceipts -= 1; return nil }
                    guard let (retained, json) = receipts[owner], retained == operation else { return nil }
                    return json
                }
            default: return saved.run(owner, request)
            }
        }, cancel: saved.cancel, close: saved.close, retirementFailed: saved.retirementFailed, readerDomain: base.domain)
    }
}

private final class IndexBuildGrant {}

@main private enum AtlasSavedIndexBuildTests {
    @MainActor private static func wait(_ message: String, _ ready: () -> Bool) async {
        for _ in 0..<2000 { if ready() { return }; try? await Task.sleep(nanoseconds: 1_000_000) }
        preconditionFailure(message)
    }
    private static func reject(_ work: () throws -> Void) {
        do { try work(); preconditionFailure("invalid index publication response accepted") } catch {}
    }
    @MainActor static func main() async throws {
        let f = SavedIndexBuildFixture(), destination = "/exports/index-e\u{301}.fcbd"
        let browser = AtlasSavedRepositoryModel(transport: f.transport)
        browser.open(path: "/saved/repository.fcbs", accessLease: nil)
        await wait("archive open") { !browser.busy }
        let identity = browser.info!.identity
        precondition(browser.canBuildIndex && browser.info!.lastIndexBuildAttempt == 0)
        let request = try AtlasSavedIndexBuildRequest(identity: identity, operation: UInt64.max,
            destination: destination)
        let call = SavedIndexBuildFixture.Call(owner: identity.owner, operation: request.operation,
            destination: destination, limits: .standard)
        let decode: ([String: Any]) throws -> AtlasSavedIndexBuildReceipt = {
            try AtlasSavedIndexBuildReceipt.decode(f.base.encode($0), request: request)
        }
        let valid = try decode(f.receipt(call))
        precondition(valid.attachEligible && valid.succeeded && valid.operation == UInt64.max)
        precondition(valid.destination.bytes == Array(destination.utf8) && valid.pin == f.pin)
        precondition(valid.coverage?.indexed == 3 && !valid.coverageComplete)
        for effect in AtlasSavedIndexBuildEffect.allCases {
            let outcome = try decode(f.receipt(call, effect: effect, succeeded: false))
            precondition(outcome.effect == effect && outcome.attachEligible == effect.completeFile)
            precondition(!outcome.succeeded && outcome.errorCode == "SAVED_INDEX_WRITE_CANCELED")
        }
        let early = try decode(f.receipt(call, effect: .none, succeeded: false, encoded: false, hasStats: false))
        let encodingFailure = try decode(f.receipt(call, effect: .none, succeeded: false, encoded: false))
        precondition(early.coverage == nil && early.pin == nil && encodingFailure.coverage != nil)
        var interrupted = f.receipt(call)
        interrupted["member_read_calls"] = "4194304"
        let retried = try decode(interrupted)
        precondition(retried.memberCalls == 4_194_304, "Interrupted reads can exceed consumed bytes")

        let invalidFields: [(String, Any)] = [
            ("schema", "fcb.saved-repository/2"), ("command", "index-attach"), ("owner", "99"),
            ("archive_digest", String(repeating: "a", count: 64)), ("operation", "1"), ("last_index_build_attempt", "1"),
            ("destination", f.base.path(Array("/elsewhere.fcbd".utf8), "/elsewhere.fcbd")),
            ("max_source_bytes", "0"), ("max_grams_per_file", "65535"), ("max_index_bytes", "0"),
            ("index_pin", String(repeating: "C", count: 64)), ("index_pin_scope", "whole-file-sha256"),
            ("index_kind", "raw-source"), ("bytes_written", "4097"), ("index_manifest_bytes", "4096"),
            ("index_members", "6"), ("index_uncovered_files", "2"), ("index_unavailable_files", "0"),
            ("index_unique_grams", "2097153"), ("index_build_source_bytes", "67108865"), ("coverage_complete", true),
            ("member_bytes_read", "67108865"), ("member_read_calls", "4194305"), ("attach_eligible", false),
            ("index_attached", true), ("accepted_query_unchanged", false), ("accepted_index_unchanged", false),
            ("source_derived_sensitive", false), ("encrypted", true), ("overwritten", true), ("power_loss_qualified", true),
            ("status", "error"), ("error", ["code": "wrong", "next_action": "wrong"])
        ]
        for (key, value) in invalidFields {
            var forged = f.receipt(call); forged[key] = value
            reject { _ = try decode(forged) }
        }
        for key in ["index_pin", "index_members", "index_uncovered_files", "error", "effect"] {
            var missing = f.receipt(call); missing.removeValue(forKey: key)
            reject { _ = try decode(missing) }
        }
        var partialSummary = f.receipt(call); partialSummary["index_indexed_files"] = NSNull()
        reject { _ = try decode(partialSummary) }
        var inventedWrite = f.receipt(call, effect: .none, succeeded: false); inventedWrite["bytes_written"] = "1"
        reject { _ = try decode(inventedWrite) }
        var successWithoutSync = f.receipt(call); successWithoutSync["effect"] = "complete-file-sync-unconfirmed"
        reject { _ = try decode(successWithoutSync) }
        var incompleteWithoutEncoding = f.receipt(call, effect: .none, succeeded: false, encoded: false, hasStats: false)
        incompleteWithoutEncoding["effect"] = "destination-created-incomplete"
        reject { _ = try decode(incompleteWithoutEncoding) }

        let zeroLimits = AtlasSavedIndexBuildLimits(fileBytes: 0, sourceBytes: 0, grams: 0)
        let zeroRequest = try AtlasSavedIndexBuildRequest(identity: identity, operation: 1,
            destination: "/zero.fcbd", limits: zeroLimits)
        var zero = f.receipt(.init(owner: identity.owner, operation: 1, destination: "/zero.fcbd", limits: zeroLimits))
        zero["index_indexed_files"] = "0"; zero["index_uncovered_files"] = "4"; zero["index_unique_grams"] = "0"
        zero["index_build_source_bytes"] = "0"; zero["member_bytes_read"] = "0"; zero["member_read_calls"] = "0"
        let emptyIndex = try AtlasSavedIndexBuildReceipt.decode(f.base.encode(zero), request: zeroRequest)
        precondition(emptyIndex.attachEligible && emptyIndex.coverage?.uncovered == 4)
        for invalid in ["relative.fcbd", "/", "/a/../index.fcbd", "/a//index.fcbd", "/a\0.fcbd",
            "/" + String(repeating: "x", count: 16_384)] {
            reject { _ = try AtlasSavedIndexBuildRequest(identity: identity, operation: 1, destination: invalid) }
        }
        reject { _ = try AtlasSavedIndexBuildRequest(identity: identity, operation: 0, destination: destination) }
        reject { _ = try AtlasSavedIndexBuildRequest(identity: identity, operation: 1, destination: destination,
            limits: .init(fileBytes: 262_145, sourceBytes: 1, grams: 1)) }

        let legacy = f.base.info(identity.owner)
        let oldInfo = try AtlasSavedInfo.decode(f.base.encode(legacy), handle: identity.owner,
            expectedPath: Array("/saved/repository.fcbs".utf8))
        precondition(!oldInfo.indexBuildStateKnown)
        for (attempt, operation) in [("0", "1"), ("1", "0"), ("1", "2")] {
            var invalid = legacy; invalid["last_index_build_attempt"] = attempt
            invalid["last_index_build_operation"] = operation
            reject { _ = try AtlasSavedInfo.decode(f.base.encode(invalid), handle: identity.owner,
                expectedPath: Array("/saved/repository.fcbs".utf8)) }
        }
        var half = legacy; half["last_index_build_attempt"] = "0"
        reject { _ = try AtlasSavedInfo.decode(f.base.encode(half), handle: identity.owner,
            expectedPath: Array("/saved/repository.fcbs".utf8)) }

        browser.attachIndex(path: "/existing.fcbd", pin: String(repeating: "a", count: 64), accessLease: nil)
        await wait("old index attached") { !browser.busy }
        browser.searchInput = "ana"; browser.search()
        await wait("accepted source search") { !browser.busy }
        let page = browser.searchPage!, oldIndex = browser.index!.generation
        let reader = try await browser.openHit(page.hits[0], from: page, readerTransport: f.base.readerTransport())
        browser.buildIndex(destination: destination, accessLease: IndexBuildGrant())
        await wait("index published") { !browser.busy }
        precondition(f.calls.count == 1 && f.calls[0].operation == 1 && f.calls[0].limits == .standard)
        precondition(browser.canAttachBuiltIndex && browser.indexBuildReceipt!.pin == f.pin)
        precondition(browser.index!.generation == oldIndex && browser.searchPage!.generation == page.generation && reader.reader.canPin)
        precondition(f.base.calls.filter { $0.hasPrefix("open:") }.count == 1, "build must not reopen the archive pathname")
        precondition(f.base.calls.filter { $0.hasPrefix("index-attach:") }.count == 1, "build must not attach automatically")
        browser.indexPinInput = String(repeating: "b", count: 64)
        browser.attachBuiltIndex()
        await wait("explicit built index attachment") { !browser.busy }
        precondition(browser.index!.pin == f.pin && browser.indexPath!.utf8.elementsEqual(destination.utf8))
        precondition(browser.searchPage!.generation == page.generation && reader.reader.canPin)

        for (kind, eligible) in [("none", false), ("partial", false), ("unsynced", true)] {
            let priorIndex = browser.index!.generation
            f.corruptNext(kind)
            browser.buildIndex(destination: "/exports/\(kind).fcbd", accessLease: nil)
            await wait("effect-aware failure") { !browser.busy }
            precondition(browser.indexBuildOutcomeKnown && !browser.indexBuildReceipt!.succeeded)
            precondition(browser.canAttachBuiltIndex == eligible && browser.index!.generation == priorIndex)
            precondition(browser.searchPage!.generation == page.generation && reader.reader.canPin)
        }
        for kind in ["lost", "wrong-reply"] {
            let before = f.calls.count
            f.corruptNext(kind)
            browser.buildIndex(destination: "/exports/\(kind).fcbd", accessLease: nil)
            await wait("retained receipt recovered") { !browser.busy }
            precondition(f.calls.count == before + 1 && browser.canAttachBuiltIndex)
            precondition(f.receiptReads.last == browser.indexBuildRequest!.operation)
        }
        f.corruptNext("lost", receiptFailures: 1)
        browser.buildIndex(destination: "/exports/unconfirmed.fcbd", accessLease: nil)
        await wait("unknown publication outcome") { !browser.busy }
        precondition(!browser.indexBuildOutcomeKnown && !browser.canAttachBuiltIndex)
        let writes = f.calls.count
        browser.buildIndex(destination: "/exports/unconfirmed.fcbd", accessLease: nil)
        precondition(!browser.busy && f.calls.count == writes, "unknown destination must not be retried")
        browser.reloadIndexBuildReceipt()
        await wait("explicit receipt recovery") { !browser.busy }
        precondition(browser.indexBuildOutcomeKnown && browser.canAttachBuiltIndex && f.calls.count == writes)

        f.blockNextBuild()
        var grant: IndexBuildGrant? = IndexBuildGrant()
        weak var retainedGrant = grant
        browser.buildIndex(destination: "/exports/canceled.fcbd", accessLease: grant)
        grant = nil
        await wait("entered writer") { f.entered.wait(timeout: .now()) == .success }
        browser.cancel(); browser.closeArchive()
        precondition(browser.busy && browser.indexBuildInProgress && browser.indexBuildCancellationRequested)
        precondition(browser.info != nil && retainedGrant != nil && !browser.canAttachBuiltIndex)
        let inFlight = f.calls.count
        browser.buildIndex(destination: "/exports/overlap.fcbd", accessLease: nil)
        precondition(f.calls.count == inFlight)
        f.resume.signal()
        await wait("canceled writer drained and reconciled") { !browser.busy }
        precondition(browser.canAttachBuiltIndex && browser.indexBuildReceipt!.succeeded)
        precondition(f.calls.count == inFlight && f.receiptReads.last == browser.indexBuildRequest!.operation)
        precondition(reader.reader.canPin && retainedGrant != nil)
        browser.closeArchive()
        await wait("finished destination grant released") { retainedGrant == nil }
        precondition(browser.info == nil && browser.indexBuildRequest == nil && reader.reader.canPin)
        reader.reader.close(); browser.close()

        let closing = SavedIndexBuildFixture(), closed = AtlasSavedRepositoryModel(transport: closing.transport)
        closed.open(path: "/saved/repository.fcbs", accessLease: nil)
        await wait("closing fixture open") { !closed.busy }
        closing.blockNextBuild()
        var closingGrant: IndexBuildGrant? = IndexBuildGrant()
        weak var heldWhileClosing = closingGrant
        closed.buildIndex(destination: "/exports/close.fcbd", accessLease: closingGrant); closingGrant = nil
        await wait("closing writer entered") { closing.entered.wait(timeout: .now()) == .success }
        closed.close()
        precondition(closed.busy && closed.closed && closed.info == nil && heldWhileClosing != nil)
        precondition(!closing.base.calls.contains { $0.hasPrefix("saved-close:") })
        closing.resume.signal()
        await wait("closed writer released resources") {
            !closed.busy && heldWhileClosing == nil && closing.base.calls.contains { $0.hasPrefix("saved-close:") }
        }
        precondition(closed.indexBuildReceipt == nil && closing.calls.count == 1 && closing.receiptReads.isEmpty)
        print("AtlasSavedIndexBuildTests: qualified receipts, explicit attachment, lost/canceled outcomes and grant draining passed")
    }
}
