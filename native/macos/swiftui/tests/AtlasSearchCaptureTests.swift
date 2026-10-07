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
        do {
            _ = try AtlasSearchCaptureOwner(handle: 0, close: { _ in true }, retirementFailed: {})
            preconditionFailure("Zero handle admitted")
        } catch AtlasSearchError.unavailable { checks += 1 }
        let counts = Counts()
        var owner: AtlasSearchCaptureOwner? = try AtlasSearchCaptureOwner(handle: 42, close: { handle in
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
        let failed = Counts()
        var denied: AtlasSearchCaptureOwner? = try AtlasSearchCaptureOwner(handle: 9, close: { _ in failed.add("close"); return false },
            retirementFailed: { failed.add("failure") })
        _ = denied
        denied = nil
        await wait { failed.get("failure") == 1 }
        check(failed.get("close") == 8, "Permanent retirement refusal is bounded and reported")
        // The exact production lock serializes reader import with a paused
        // foreign step. Other owners remain independently usable.
        let serial = try AtlasSearchCaptureOwner(handle: 1, close: { _ in true }, retirementFailed: {})
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        let done = Counts()
        DispatchQueue.global().async {
            serial.call { _ in entered.signal(); release.wait(); done.add("first") }
        }
        check(entered.wait(timeout: .now() + 5) == .success, "Foreign step entered")
        DispatchQueue.global().async { serial.call { _ in precondition(done.get("first") == 1); done.add("second") } }
        let independent = try AtlasSearchCaptureOwner(handle: 2, close: { _ in true }, retirementFailed: {})
        check(independent.call { $0 } == 2 && done.get("second") == 0, "Another owner does not wait on this source")
        release.signal(); await wait { done.get("second") == 1 }
        checks += 1
        print("AtlasSearchCaptureTests: \(checks) checks passed")
    }
}
