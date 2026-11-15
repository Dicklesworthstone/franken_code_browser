// Fixed handoff envelopes exercise production ownership/validation, not Rust I/O.
import Foundation
import Dispatch

private final class Counts: @unchecked Sendable {
    private let lock = NSLock()
    private var values: [String: Int] = [:]
    func add(_ key: String) { lock.lock(); values[key, default: 0] += 1; lock.unlock() }
    func get(_ key: String) -> Int { lock.lock(); defer { lock.unlock() }; return values[key, default: 0] }
}
private final class Access: @unchecked Sendable {
    let counts: Counts
    init(_ counts: Counts) { self.counts = counts }
    deinit { counts.add("grantReleased") }
}

@main private enum AtlasSearchCaptureTests {
    private static let identity = AtlasSearchCaptureIdentity(owner: 42, manifest: 3, layout: 9, generation: 7)
    private static let hit = SearchHit(id: 0, path: "src.rs", sourcePath: "src.rs", start: 2, end: 6,
        captureSHA256: String(repeating: "a", count: 64), captureByteLength: 20)
    private static var witness: AtlasSearchCaptureWitness { .init(hit: hit, file: 6, revision: 7, pathHex: "7372632e7273") }
    private static func envelope() -> [String: Any] {
        let path: [String: Any] = ["encoding": "unix-bytes", "hex": witness.pathHex, "display": "src.rs"]
        return ["schema": "fcb.atlas-search/1", "status": "ok", "command": "open-reader", "owner": "42",
            "source_manifest": "3", "layout_revision": "9", "query_generation": "7", "hit_id": "1",
            "file_id": "6", "source_revision": "7", "capture_sha256": hit.captureSHA256!,
            "capture_byte_length": "20", "original_range": ["start": "2", "end": "6"], "path": path,
            "source_observation": "retained-search-capture", "source_reopened": false, "reader_owner": "100",
            "reader": ["schema": "fcb.reader-session/1", "status": "ok", "command": "info", "owner": "100",
                "file_id": "1", "source_revision": "1", "captured_bytes": "20", "path": path,
                "capture_origin": "host-supplied", "initial_source_bytes_read": "0", "initial_read_calls": "0",
                "additional_source_bytes_read": "0", "native_presented": false, "encoding": "utf16le",
                "accepted_query_generation": NSNull(), "future_field": "preserve me"]]
    }
    private static func json(_ object: [String: Any]) throws -> String {
        String(decoding: try JSONSerialization.data(withJSONObject: object), as: UTF8.self)
    }
    @MainActor private static var checks = 0
    @MainActor private static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        precondition(condition(), message); checks += 1
    }
    @MainActor private static func wait(_ condition: () -> Bool) async {
        for _ in 0..<1000 {
            if condition() { return }
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        preconditionFailure("Timed out")
    }
    @MainActor private static func rejected(_ object: [String: Any], _ message: String) throws {
        do {
            _ = try AtlasSearchCaptureFactory.validatedReader(json(object), receiver: 100, identity: identity, witness: witness)
            preconditionFailure("Accepted " + message)
        } catch AtlasSearchError.invalidResponse { checks += 1 }
    }
    private static func retirementOwner(_ counts: Counts, handle: UInt64) throws -> AtlasSearchCaptureOwner {
        let owner = try AtlasSearchCaptureOwner(create: {
            counts.add("create")
            return handle
        }, close: { current in
            precondition(current == handle)
            counts.add("close")
            if Thread.isMainThread { counts.add("closeOnMain") }
            guard counts.get("allowClose") > 0 else { return false }
            counts.add("closed")
            counts.add("closed:\(current)")
            return true
        }, retirementFailed: { counts.add("failure") })
        owner.retainAccess(AtlasSearchAccessLease(Access(counts)))
        return owner
    }
    @MainActor static func main() async throws {
        let valid = try json(envelope())
        let reader = try AtlasSearchCaptureFactory.validatedReader(valid, receiver: 100, identity: identity, witness: witness)
        let decoded = try JSONSerialization.jsonObject(with: Data(reader.utf8)) as! [String: Any]
        check(decoded["owner"] as? String == "100" && decoded["file_id"] as? String == "1", "Independent receiver IDs are not relabeled")
        check(decoded["encoding"] as? String == "utf16le", "UTF-16 capture is not coerced into UTF-8")
        check(decoded["future_field"] as? String == "preserve me", "Original nested response fields preserved")
        for key in ["owner", "source_manifest", "layout_revision", "query_generation", "hit_id", "file_id", "source_revision", "reader_owner", "capture_byte_length"] {
            var bad = envelope(); bad[key] = "999"; try rejected(bad, "foreign " + key)
            bad[key] = "01"; try rejected(bad, "noncanonical " + key)
            bad[key] = 1; try rejected(bad, "numeric instead of string " + key)
        }
        for (key, value) in [("schema", "wrong"), ("status", "error"), ("command", "page"),
                             ("capture_sha256", String(repeating: "b", count: 64)), ("source_observation", "live-file")] {
            var bad = envelope(); bad[key] = value; try rejected(bad, key)
        }
        var bad = envelope(); bad["source_reopened"] = true; try rejected(bad, "reopened source")
        bad = envelope(); bad["source_reopened"] = 0; try rejected(bad, "integer boolean")
        for key in ["start", "end"] {
            bad = envelope(); var range = bad["original_range"] as! [String: Any]; range[key] = "3"
            bad["original_range"] = range; try rejected(bad, "changed selected " + key)
        }
        bad = envelope(); bad["path"] = ["encoding": "unix-bytes", "hex": "7372632e7079"]; try rejected(bad, "changed source path")
        for (key, value) in [("owner", "42"), ("file_id", "0"), ("source_revision", "01"),
                             ("captured_bytes", "21"), ("capture_origin", "regular-file-observation-not-atomic"),
                             ("initial_source_bytes_read", "20"), ("initial_read_calls", "1"),
                             ("additional_source_bytes_read", "1"), ("schema", "wrong"), ("command", "window")] {
            bad = envelope(); var receiver = bad["reader"] as! [String: Any]; receiver[key] = value
            bad["reader"] = receiver; try rejected(bad, "receiver " + key)
        }
        bad = envelope(); var receiver = bad["reader"] as! [String: Any]
        receiver["path"] = ["encoding": "unix-bytes", "hex": "612e7273"]
        bad["reader"] = receiver; try rejected(bad, "receiver path mismatch")
        do {
            _ = try AtlasSearchCaptureFactory.validatedReader(String(repeating: "x", count: 4 * 1024 * 1024 + 1),
                receiver: 100, identity: identity, witness: witness)
            preconditionFailure("Oversize handoff admitted")
        } catch AtlasSearchError.invalidResponse { checks += 1 }
        let refusedCreates = Counts()
        for _ in 0..<128 {
            do {
                _ = try AtlasSearchCaptureOwner(create: { refusedCreates.add("create"); return 0 },
                    close: { _ in preconditionFailure("A failed creation owns no handle to close") },
                    retirementFailed: { preconditionFailure("A failed creation owns no retirement") })
                preconditionFailure("Zero handle admitted")
            } catch AtlasSearchError.unavailable { }
        }
        check(refusedCreates.get("create") == 128, "Failed creation releases its reserved admission")
        do {
            let spellingOwner = try AtlasSearchCaptureOwner(create: { 42 }, close: { _ in true }, retirementFailed: {})
            var spellingHit = hit; spellingHit.actualText = "é"
            let spellingWitness = AtlasSearchCaptureWitness(hit: spellingHit, file: 6, revision: 7, pathHex: witness.pathHex)
            let spellingCapture = AtlasSearchCaptureFactory.make(owner: spellingOwner, identity: identity,
                root: "/repo", needle: "É", witnesses: [spellingWitness], caseSensitive: false,
                importReader: { _, _, _, _ in valid })
            let exact = spellingCapture.target(spellingHit)
            check(exact?.needle.utf8.elementsEqual("é".utf8) == true && exact?.caseSensitive == false,
                "Insensitive capture authorizes an exact query of the matched source spelling")
            var aliased = spellingHit; aliased.actualText = "e\u{301}"
            check(spellingCapture.target(aliased) == nil,
                "Canonical-equivalent source spelling cannot borrow the original occurrence witness")
            let missing = AtlasSearchCaptureFactory.make(owner: spellingOwner, identity: identity,
                root: "/repo", needle: "x", witnesses: [witness], caseSensitive: false,
                importReader: { _, _, _, _ in valid })
            check(missing.target(hit) == nil, "Insensitive capture without actual source spelling cannot activate")
            let wrongExact = AtlasSearchCaptureFactory.make(owner: spellingOwner, identity: identity,
                root: "/repo", needle: "É", witnesses: [spellingWitness],
                importReader: { _, _, _, _ in valid })
            check(wrongExact.target(spellingHit) == nil, "Sensitive capture cannot substitute different query spelling")
        }
        let counts = Counts()
        var owner: AtlasSearchCaptureOwner? = try AtlasSearchCaptureOwner(create: { 42 }, close: { handle in
            precondition(handle == 42)
            counts.add("close")
            precondition(counts.get("grantReleased") == 0, "Grant must survive retirement")
            return counts.get("close") == 3
        }, retirementFailed: { counts.add("failure") })
        weak var weakOwner = owner
        var capture: AtlasSearchCapture? = AtlasSearchCaptureFactory.make(owner: owner!, identity: identity,
            root: "/repo", needle: "x", witnesses: [witness], importReader: { atlas, reader, generation, hit in
                precondition(atlas == 42 && reader == 100 && generation == 7 && hit == 1)
                counts.add("import")
                return valid
            })
        capture!.retainAccess(AtlasSearchAccessLease(Access(counts)))
        var target: AtlasSearchCapturedHit? = capture!.target(hit)
        check(target?.path == "src.rs" && target?.root == "/repo", "Target preserves explicit origin")
        let forged = SearchHit(id: 0, path: "src.rs", sourcePath: "src.rs", start: 3, end: 6,
            captureSHA256: hit.captureSHA256, captureByteLength: 20)
        check(capture!.target(forged) == nil, "Same row ID cannot borrow another range")
        let otherPath = SearchHit(id: 0, path: "src.rs", sourcePath: "other.rs", start: 2, end: 6,
            captureSHA256: hit.captureSHA256, captureByteLength: 20)
        check(capture!.target(otherPath) == nil, "Display label does not authorize a raw path")
        owner = nil; capture = nil
        check(weakOwner != nil && counts.get("close") == 0, "Selected target pins source after report is dropped")
        _ = try target!.openReader(100)
        check(counts.get("import") == 1, "Activation uses the pinned import capability")
        target = nil
        await wait { counts.get("close") == 3 && counts.get("grantReleased") == 1 }
        check(weakOwner == nil && counts.get("failure") == 0, "Last target retires handle with bounded contention retry")
        do {
            let failed = Counts()
            var denied: AtlasSearchCaptureOwner? = try retirementOwner(failed, handle: 9)
            weak var weakDenied = denied
            denied = nil
            await wait { failed.get("failure") == 1 }
            check(weakDenied == nil && failed.get("close") == 8,
                "Refused retirement drops the presentation owner after one bounded close pass")
            check(failed.get("grantReleased") == 0 && failed.get("closed") == 0,
                "A refused native close keeps the source grant and handle alive")
            check(failed.get("closeOnMain") == 0, "Retirement never runs on the interaction thread")

            failed.add("allowClose")
            var replacement: AtlasSearchCaptureOwner? = try retirementOwner(failed, handle: 10)
            await wait { failed.get("closed") == 1 && failed.get("grantReleased") == 1 }
            check(replacement?.call { $0 } == 10 && failed.get("closed:9") == 1,
                "A new capture retries the quarantined handle exactly once after contention ends")
            replacement = nil
            await wait { failed.get("closed") == 2 && failed.get("grantReleased") == 2 }
            check(failed.get("closed:10") == 1, "Replacement capture has independent retirement")
        }
        do {
            let saturated = Counts()
            var owners: [AtlasSearchCaptureOwner] = []
            for handle in UInt64(1)...4 {
                owners.append(try retirementOwner(saturated, handle: handle))
            }
            owners.removeAll()
            await wait { saturated.get("failure") >= 4 }
            var refused = 0
            for _ in 0..<128 {
                do {
                    _ = try retirementOwner(saturated, handle: 5)
                    preconditionFailure("Quarantined handles did not consume atlas admission")
                } catch AtlasSearchError.unavailable { refused += 1 }
            }
            check(refused == 128 && saturated.get("create") == 4,
                "Four failed closes block native creation, including repeated capture attempts")
            check(saturated.get("grantReleased") == 0 && saturated.get("closed") == 0,
                "Full quarantine retains every native root grant")

            saturated.add("allowClose")
            var replacement: AtlasSearchCaptureOwner?
            // Admission does not wait on the retirement lane. A request can be
            // coalesced with a retry already running, so explicitly retry until
            // the finite old-owner set drains and one new owner is admitted.
            for _ in 0..<1000 {
                if replacement == nil {
                    do { replacement = try retirementOwner(saturated, handle: 5) }
                    catch AtlasSearchError.unavailable { }
                }
                if saturated.get("closed") == 4 && saturated.get("grantReleased") == 4,
                   replacement != nil { break }
                try? await Task.sleep(nanoseconds: 1_000_000)
            }
            check(replacement?.call { $0 } == 5 && saturated.get("create") == 5,
                "Recovered admission creates exactly one new native owner")
            check(saturated.get("closed") == 4 && saturated.get("grantReleased") == 4,
                "Recovery releases all four grants only after confirmed native closes")
            check((UInt64(1)...4).allSatisfy { saturated.get("closed:\($0)") == 1 },
                "Quarantined owners each close successfully once")
            replacement = nil
            await wait { saturated.get("closed") == 5 && saturated.get("grantReleased") == 5 }
            check(saturated.get("create") == saturated.get("closed") && saturated.get("closeOnMain") == 0,
                "Retirement conserves native ownership and stays off the interaction thread")
        }
        // The exact production lock serializes reader import with a paused
        // foreign step. Other owners remain independently usable.
        let serial = try AtlasSearchCaptureOwner(create: { 1 }, close: { _ in true }, retirementFailed: {})
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        let done = Counts()
        DispatchQueue.global().async {
            serial.call { _ in entered.signal(); release.wait(); done.add("first") }
        }
        check(entered.wait(timeout: .now() + 5) == .success, "Foreign step entered")
        DispatchQueue.global().async { serial.call { _ in precondition(done.get("first") == 1); done.add("second") } }
        let independent = try AtlasSearchCaptureOwner(create: { 2 }, close: { _ in true }, retirementFailed: {})
        check(independent.call { $0 } == 2 && done.get("second") == 0, "Another owner does not wait on this source")
        release.signal(); await wait { done.get("second") == 1 }
        checks += 1
        print("AtlasSearchCaptureTests: \(checks) checks passed")
    }
}
