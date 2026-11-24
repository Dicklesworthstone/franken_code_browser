import Foundation
import CryptoKit

@main struct AtlasFolderPersistenceTests {
    static func check(_ value: Bool, _ message: String = "atlas-view regression") {
        precondition(value, message)
    }
    static func snapshot(_ label: String) throws -> AtlasFolderNavigation.Snapshot {
        var navigation = AtlasFolderNavigation()
        for directory in ["docs", "src", "tests"] {
            let place = try AtlasFolderDestination(directory: AtlasDirectoryFocus(rawPath: Array(directory.utf8)),
                fileScope: .rust, customExtensions: "", anchorRawPath: Array((directory + "/lib.rs").utf8),
                cameraAnchor: AtlasParcelCameraAnchor(relativeX: 0.2, relativeY: 0.8, zoomFactor: 3))
            check(navigation.commit(navigation.visit(place), resolved: place))
        }
        let back = navigation.back()!
        check(navigation.commit(back, resolved: back.target))
        _ = try navigation.addLandmark(label: label, destination: navigation.current!)
        return navigation.snapshot()
    }
    static func store() -> AtlasFolderViewStore {
        let directory = FileManager.default.temporaryDirectory.resolvingSymlinksInPath()
            .appendingPathComponent("fcb-atlas-views-" + UUID().uuidString, isDirectory: true)
        print("Retained atlas-view fixture: \(directory.path)")
        return AtlasFolderViewStore(directory: directory)
    }
    static func expect(_ expected: AtlasFolderPersistenceError, _ body: () throws -> Void) {
        do { try body(); preconditionFailure("expected \(expected)") }
        catch { check(error as? AtlasFolderPersistenceError == expected, "unexpected \(error)") }
    }
    static func data(_ store: AtlasFolderViewStore, _ context: AtlasFolderPersistenceContext,
        suffix: String = "") throws -> Data {
        try Data(contentsOf: store.directory.appendingPathComponent(context.filename + suffix))
    }

    static func roundTripRawIdentityAndPolicy() throws {
        let store = store()
        let context = try AtlasFolderPersistenceContext(root: "/Projects/caf\u{e9}", policy: 0)
        let alias = try AtlasFolderPersistenceContext(root: "/Projects/cafe\u{301}", policy: 0)
        let policy = try AtlasFolderPersistenceContext(root: context.root, policy: 1)
        check(context != alias && context.filename != alias.filename && context.filename != policy.filename)
        try check(store.load(context).snapshot == nil)
        let original = try snapshot("Implementation")
        let receipt = try store.save(original, context: context, expecting: nil)
        check(receipt.revision != nil && receipt.synchronizationConfirmed)
        try check(store.load(context).snapshot == original)
        try check(store.load(alias).snapshot == nil && store.load(policy).snapshot == nil)
        let rawPlace = try AtlasFolderDestination(directory: AtlasDirectoryFocus(rawPath: [0xff, 47, 0xfe]),
            fileScope: .all, customExtensions: "", anchorRawPath: [0xff, 47, 0xfe, 47, 0xfd],
            cameraAnchor: AtlasParcelCameraAnchor(relativeX: -0.3, relativeY: 2.5, zoomFactor: 0.25))
        var raw = AtlasFolderNavigation()
        raw.rebaseCurrent(rawPlace)
        _ = try raw.addLandmark(label: "Original bytes", destination: rawPlace)
        _ = try store.save(raw.snapshot(), context: policy, expecting: nil)
        try check(store.load(policy).snapshot == raw.snapshot() && store.load(context).snapshot == original,
            "raw Unix paths survive and switching discovery policies preserves both views")
        var revived = AtlasFolderNavigation()
        try revived.restore(try store.load(context).snapshot!)
        check(revived.canBack && revived.canForward && revived.current == original.history.current
            && revived.landmarks == original.landmarks)
    }

    static func conflictAndPreviousGeneration() throws {
        let store = store(), context = try AtlasFolderPersistenceContext(root: "/Project", policy: 0)
        let first = try snapshot("First")
        let old = try store.save(first, context: context, expecting: nil)
        let firstBytes = try data(store, context)
        let newer = try store.save(snapshot("Second"), context: context, expecting: old.revision)
        let acceptedBytes = try data(store, context)
        try check(newer.revision != old.revision && data(store, context, suffix: ".previous") == firstBytes)
        expect(.conflict) { _ = try store.save(first, context: context, expecting: old.revision) }
        try check(data(store, context) == acceptedBytes && data(store, context, suffix: ".previous") == firstBytes,
            "stale windows cannot replace either accepted generation")
        let corrupt = Data("unreadable prior view".utf8)
        try corrupt.write(to: store.directory.appendingPathComponent(context.filename + ".previous"))
        expect(.corrupt) { _ = try store.save(first, context: context, expecting: newer.revision) }
        try check(data(store, context) == acceptedBytes && data(store, context, suffix: ".previous") == corrupt,
            "damaged personal history is preserved instead of overwritten by autosave")
    }

    static func rewriting(_ original: Data, _ mutate: (inout [String: Any]) -> Void) throws -> Data {
        var seal = try JSONSerialization.jsonObject(with: original) as! [String: Any]
        var body = try JSONSerialization.jsonObject(with: Data(base64Encoded: seal["body"] as! String)!) as! [String: Any]
        mutate(&body)
        let encoded = try JSONSerialization.data(withJSONObject: body, options: [.sortedKeys])
        seal["body"] = encoded.base64EncodedString()
        seal["sha256"] = Data(SHA256.hash(data: encoded)).base64EncodedString()
        return try JSONSerialization.data(withJSONObject: seal, options: [.sortedKeys])
    }

    static func corruptRecordsRemainUntouched() throws {
        let store = store(), context = try AtlasFolderPersistenceContext(root: "/Project", policy: 0)
        let original = try snapshot("Landmark")
        let receipt = try store.save(original, context: context, expecting: nil)
        let originalBytes = try data(store, context)
        var brokenChecksum = try JSONSerialization.jsonObject(with: originalBytes) as! [String: Any]
        brokenChecksum["sha256"] = Data(repeating: 0, count: 32).base64EncodedString()
        var malformed: [Data] = [Data("not a saved view".utf8),
            try JSONSerialization.data(withJSONObject: brokenChecksum, options: [.sortedKeys]),
            Data(repeating: 0, count: AtlasFolderViewStore.maximumRecordBytes + 1)]
        malformed.append(try rewriting(originalBytes) { $0["version"] = 99 })
        malformed.append(try rewriting(originalBytes) { $0["root"] = Data("/Other".utf8).base64EncodedString() })
        malformed.append(try rewriting(originalBytes) { $0["policy"] = 1 })
        malformed.append(try rewriting(originalBytes) {
            var current = $0["current"] as! [String: Any]
            current["directory"] = Data("../escape".utf8).base64EncodedString(); $0["current"] = current
        })
        malformed.append(try rewriting(originalBytes) {
            var current = $0["current"] as! [String: Any]
            current["parcel"] = Data("outside/lib.rs".utf8).base64EncodedString(); $0["current"] = current
        })
        malformed.append(try rewriting(originalBytes) {
            var current = $0["current"] as! [String: Any]
            current["camera"] = ["x": 0, "y": 0, "zoom": 0]; $0["current"] = current
        })
        malformed.append(try rewriting(originalBytes) {
            let marks = $0["landmarks"] as! [[String: Any]]; $0["landmarks"] = marks + marks
        })
        malformed.append(try rewriting(originalBytes) { $0["previous"] = Array(repeating: $0["current"]!, count: 129) })
        for bytes in malformed {
            try bytes.write(to: store.directory.appendingPathComponent(context.filename))
            expect(.corrupt) { _ = try store.load(context) }
            expect(.corrupt) { _ = try store.save(original, context: context, expecting: receipt.revision) }
            try check(data(store, context) == bytes, "refused load/save preserves original invalid bytes")
        }
    }

    static func quotasAndSymlinks() throws {
        let store = store(), original = try snapshot("Bounded")
        for index in 0..<AtlasFolderViewStore.maximumContexts {
            let context = try AtlasFolderPersistenceContext(root: "/Project/\(index)", policy: 0)
            _ = try store.save(original, context: context, expecting: nil)
        }
        let full = try AtlasFolderPersistenceContext(root: "/Project/full", policy: 0)
        expect(.limit) { _ = try store.save(original, context: full, expecting: nil) }
        try check(store.load(full).snapshot == nil)
        let first = try AtlasFolderPersistenceContext(root: "/Project/0", policy: 0)
        let accepted = try store.load(first)
        _ = try store.save(original, context: first, expecting: accepted.revision)
        try check(store.load(first).snapshot == original, "a full root budget still permits existing views")
        let outside = store.directory.deletingLastPathComponent()
            .appendingPathComponent("fcb-atlas-view-sentinel-" + UUID().uuidString)
        let sentinel = Data("original source untouched".utf8)
        try sentinel.write(to: outside)
        try FileManager.default.createSymbolicLink(at: store.directory.appendingPathComponent(full.filename),
            withDestinationURL: outside)
        expect(.unsafeLocation) { _ = try store.load(full) }
        expect(.unsafeLocation) { _ = try store.save(original, context: full, expecting: nil) }
        try check(Data(contentsOf: outside) == sentinel)
    }

    final class Gate: @unchecked Sendable {
        private let lock = NSLock()
        private var entered = 0
        let blocked: Set<Int>
        let release = DispatchSemaphore(value: 0)
        init(blocked: Set<Int>) { self.blocked = blocked }
        var count: Int { lock.lock(); defer { lock.unlock() }; return entered }
        func enter() {
            precondition(!Thread.isMainThread, "production view I/O must enter the utility worker")
            lock.lock(); entered += 1; let call = entered; lock.unlock()
            if blocked.contains(call) { release.wait() }
        }
    }
    @MainActor static func until(_ condition: () -> Bool) async throws {
        let deadline = ContinuousClock.now + .seconds(10)
        while !condition() {
            check(ContinuousClock.now < deadline, "worker did not finish bounded fixture")
            try await Task.sleep(for: .milliseconds(1))
        }
    }

    @MainActor static func staleLoadsCannotPublish() async throws {
        let store = store()
        let a = try AtlasFolderPersistenceContext(root: "/A", policy: 0)
        let b = try AtlasFolderPersistenceContext(root: "/B", policy: 1)
        let savedA = try snapshot("A"), savedB = try snapshot("B")
        _ = try store.save(savedA, context: a, expecting: nil)
        _ = try store.save(savedB, context: b, expecting: nil)
        let gate = Gate(blocked: [1])
        let model = AtlasFolderPersistence(makeStore: { gate.enter(); return store })
        var oldDeliveries = 0, accepted: AtlasFolderNavigation.Snapshot?
        model.open(root: a.root, policy: a.policy) { _ in oldDeliveries += 1 }
        try await until { gate.count == 1 }
        model.open(root: b.root, policy: b.policy) { accepted = $0 }
        gate.release.signal()
        try await until { !model.isLoading }
        check(oldDeliveries == 0 && accepted == savedB && model.matches(root: b.root, policy: 1))
        model.close()
    }

    @MainActor static func latestSaveDrainsAcrossRootClose() async throws {
        let store = store(), a = try AtlasFolderPersistenceContext(root: "/A", policy: 0)
        let original = try snapshot("Original"), intermediate = try snapshot("Intermediate"), latest = try snapshot("Latest")
        _ = try store.save(original, context: a, expecting: nil)
        let gate = Gate(blocked: [2])
        let model = AtlasFolderPersistence(makeStore: { gate.enter(); return store })
        model.open(root: a.root, policy: a.policy) { _ in }
        try await until { !model.isLoading }
        model.save(intermediate)
        try await until { gate.count == 2 }
        model.save(latest)
        // Going back to the confirmed value cannot leave an older write queued.
        model.save(original)
        var reopened = false
        model.open(root: "/B", policy: 0) { _ in reopened = true }
        gate.release.signal()
        try await until { reopened }
        try check(gate.count == 4 && store.load(a).snapshot == original,
            "one entered write and one latest write drain before the new root load")
        check(!model.isSaving && model.matches(root: "/B", policy: 0))
        model.close()
    }

    @MainActor static func acceptedLoadMayImmediatelySave() async throws {
        let store = store(), context = try AtlasFolderPersistenceContext(root: "/New", policy: 0)
        let value = try snapshot("Reentrant acceptance")
        let gate = Gate(blocked: [])
        let model = AtlasFolderPersistence(makeStore: { gate.enter(); return store })
        model.open(root: context.root, policy: context.policy) { _ in model.save(value) }
        try await until { !model.isLoading && !model.isSaving }
        try check(gate.count == 2 && store.load(context).snapshot == value,
            "accepted callbacks cannot create a second concurrent worker owner")
        model.close()
    }

    static func exactFileTargetsRoundTripWithoutWidening() throws {
        let store = store(), context = try AtlasFolderPersistenceContext(root: "/FileTargets", policy: 0)
        func body(_ sealed: Data) throws -> [String: Any] {
            let seal = try JSONSerialization.jsonObject(with: sealed) as! [String: Any]
            return try JSONSerialization.jsonObject(with: Data(base64Encoded: seal["body"] as! String)!) as! [String: Any]
        }
        let legacy = try snapshot("Legacy folder")
        let previous = try store.save(legacy, context: context, expecting: nil)
        let legacyBytes = try data(store, context)
        let legacyBody = try body(legacyBytes)
        check(legacyBody["version"] as? Int == 1
            && (legacyBody["current"] as! [String: Any])["file"] == nil,
            "folder-only views retain the original wire format")
        try check(store.load(context).snapshot == legacy, "legacy folder records remain readable")

        let fileA = try AtlasFileFocus(rawPath: Array("late.rs".utf8))
        let fileB = try AtlasFileFocus(rawPath: [0xff, 46, 114, 115])
        let camera = AtlasParcelCameraAnchor(relativeX: 0.2, relativeY: 0.8, zoomFactor: 3)!
        func place(_ file: AtlasFileFocus) throws -> AtlasFolderDestination {
            try AtlasFolderDestination(catalogTarget: .file(file), fileScope: .all, customExtensions: "",
                anchorRawPath: file.rawPath, cameraAnchor: camera)
        }
        let a = try place(fileA), b = try place(fileB)
        let root = try AtlasFolderDestination(directory: .project, fileScope: .all, customExtensions: "")
        var navigation = AtlasFolderNavigation()
        for destination in [root, a, b] {
            check(navigation.commit(navigation.visit(destination), resolved: destination))
        }
        let back = navigation.back()!
        check(navigation.commit(back, resolved: back.target))
        _ = try navigation.addLandmark(label: "Raw file", destination: b)
        let saved = navigation.snapshot()
        let receipt = try store.save(saved, context: context, expecting: previous.revision)
        let accepted = try data(store, context), acceptedBody = try body(accepted)
        check(acceptedBody["version"] as? Int == 2,
            "older readers must refuse file scopes instead of treating them as folders")
        try check(store.load(context).snapshot == saved && data(store, context, suffix: ".previous") == legacyBytes)
        var restored = AtlasFolderNavigation()
        try restored.restore(try store.load(context).snapshot!)
        check(restored.current == a && restored.back()?.target == root && restored.forward()?.target == b
            && restored.landmarks[0].destination == b, "saved file history, raw names and camera anchors survive")

        var marksOnly = AtlasFolderNavigation()
        marksOnly.rebaseCurrent(root)
        _ = try marksOnly.addLandmark(label: "Only file landmark", destination: b)
        var historyOnly = AtlasFolderNavigation()
        historyOnly.rebaseCurrent(b)
        check(historyOnly.commit(historyOnly.visit(root), resolved: root))
        for (name, value) in [("LandmarkOnly", marksOnly.snapshot()), ("HistoryOnly", historyOnly.snapshot())] {
            let other = try AtlasFolderPersistenceContext(root: "/FileTargets/" + name, policy: 0)
            _ = try store.save(value, context: other, expecting: nil)
            let encoded = try body(data(store, other))
            check(encoded["version"] as? Int == 2, "a file anywhere in the record requires version 2")
            try check(store.load(other).snapshot == value)
        }

        var malformed: [Data] = []
        malformed.append(try rewriting(accepted) { $0["version"] = 1 })
        for (key, value) in [
            ("directory", Data("other".utf8).base64EncodedString() as Any),
            ("file", Data("../escape".utf8).base64EncodedString() as Any),
            ("parcel", Data("sibling.rs".utf8).base64EncodedString() as Any),
            ("camera", NSNull() as Any), ("parcel", NSNull() as Any)
        ] {
            malformed.append(try rewriting(accepted) {
                var current = $0["current"] as! [String: Any]
                current[key] = value; $0["current"] = current
            })
        }
        for bytes in malformed {
            try bytes.write(to: store.directory.appendingPathComponent(context.filename))
            expect(.corrupt) { _ = try store.load(context) }
            expect(.corrupt) { _ = try store.save(saved, context: context, expecting: receipt.revision) }
            try check(data(store, context) == bytes,
                "refused file target preserves the existing saved bytes and accepted in-memory view")
            check(restored.current == a && restored.forward()?.target == b)
        }
    }

    @MainActor static func main() async throws {
        try exactFileTargetsRoundTripWithoutWidening()
        try roundTripRawIdentityAndPolicy()
        try conflictAndPreviousGeneration()
        try corruptRecordsRemainUntouched()
        try quotasAndSymlinks()
        try await staleLoadsCannotPublish()
        try await latestSaveDrainsAcrossRootClose()
        try await acceptedLoadMayImmediatelySave()
        print("8 durable atlas-view regression groups passed")
    }
}
