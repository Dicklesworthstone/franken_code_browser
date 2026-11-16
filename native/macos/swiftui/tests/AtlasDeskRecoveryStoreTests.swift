import Foundation
#if canImport(Darwin)
import Darwin
#elseif canImport(Glibc)
import Glibc
#endif

// FCBK 1.1, empty desk: four zero counts, absent history cursor, no active
// pane, and the production envelope's SHA-256 trailer. Storage tests inject
// the completed writer; they do not claim to qualify the Rust bridge or fsync
// behavior under power loss. Core restore fixtures cover document invariants.
private let checkpoint = Data(base64Encoded:
    "RkNCS0RFU0sBAAEAAAAAADAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD//////////wAAAAAAAAAAkEyUK9QL+T/OOJf8rSWpA9GT7vvKn7Q0dd/o1l4zy68=")!
private let digest = "904c942bd40bf93fce3897fcad25a903d193eefbca9fb43475dfe8d65e33cbaf"
private enum FixtureError: Error { case interrupted }

private func check(_ condition: Bool, _ message: String = "fixture assertion failed") {
    precondition(condition, message)
}

private func receipt(sourceBytes: UInt64 = 0, digest value: String = digest) -> AtlasDeskSaveReceipt {
    AtlasDeskSaveReceipt(effect: "complete-file-and-parent-sync-requested", written: UInt64(checkpoint.count),
        checkpointBytes: UInt64(checkpoint.count), sourceBytes: sourceBytes, digest: value,
        errorCode: nil, succeeded: true)
}

private func writeFixture(_ bytes: Data, to url: URL, createOnly: Bool = true) {
    let flags = O_WRONLY | O_NOFOLLOW | O_CLOEXEC | (createOnly ? O_CREAT | O_EXCL : O_TRUNC)
    let fd = open(url.path, flags, mode_t(0o600))
    check(fd >= 0, "fixture open failed")
    defer { close(fd) }
    var completed = 0
    while completed < bytes.count {
        let count = bytes.withUnsafeBytes { buffer in
            write(fd, buffer.baseAddress!.advanced(by: completed), bytes.count - completed)
        }
        if count < 0 && errno == EINTR { continue }
        check(count > 0, "fixture write failed")
        completed += count
    }
    check(fsync(fd) == 0, "fixture file sync failed")
    let parent = open(url.deletingLastPathComponent().path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
    check(parent >= 0, "fixture parent open failed")
    defer { close(parent) }
    check(fsync(parent) == 0, "fixture parent sync failed")
}

@discardableResult private func save(_ store: AtlasDeskRecoveryStore, session: UUID, revision: UInt64) throws -> AtlasDeskRecoveryEntry {
    try store.save(session: session, owner: UInt64.max, revision: revision, sourceBytes: 0) { url in
        writeFixture(checkpoint, to: url)
        return receipt()
    }
}

private func expectError(_ expected: AtlasDeskRecoveryError, _ work: () throws -> Void) {
    do { try work(); preconditionFailure("operation unexpectedly succeeded") }
    catch let error as AtlasDeskRecoveryError { check(error == expected, "unexpected recovery failure: \(error)") }
    catch { preconditionFailure("unexpected fixture error: \(error)") }
}

private func recordURL(_ entry: AtlasDeskRecoveryEntry) -> URL {
    entry.url.deletingPathExtension().appendingPathExtension("json")
}

private func fileNames(_ directory: URL) throws -> Set<String> {
    Set(try FileManager.default.contentsOfDirectory(atPath: directory.path))
}

/// Real filesystem lifecycle coverage, run only by the independent verifier.
/// Fixtures remain available for inspection; no repository data is touched.
@main struct AtlasDeskRecoveryStoreTests {
    static func main() throws {
        // macOS commonly exposes temporaryDirectory through /var -> /private/var.
        // Resolve this explicitly selected test root; production traversal keeps
        // its strict no-follow policy for every ancestor of app-private storage.
        let root = FileManager.default.temporaryDirectory.resolvingSymlinksInPath()
            .appendingPathComponent("fcb-desk-recovery-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false,
            attributes: [.posixPermissions: 0o700])
        try lifecycle(root)
        try failedWrites(root)
        try quotaAndSessionBound(root)
        try orphanRecovery(root)
        try confinement(root)
        print("Reading desk recovery filesystem regressions passed. Retained fixtures: \(root.path)")
    }

    private static func store(_ root: URL, _ name: String, sessions: Int = 8) -> AtlasDeskRecoveryStore {
        AtlasDeskRecoveryStore(directory: root.appendingPathComponent(name, isDirectory: true),
            limits: .init(maximumSessions: sessions, maximumCheckpointBytes: UInt64(checkpoint.count)))
    }

    private static func lifecycle(_ root: URL) throws {
        let store = Self.store(root, "lifecycle"), session = UUID()
        check(!FileManager.default.fileExists(atPath: store.directory.path))
        check(try store.list().isEmpty)
        check(!FileManager.default.fileExists(atPath: store.directory.path), "listing must not create storage")
        let first = try save(store, session: session, revision: UInt64.max)
        let encoded = try JSONSerialization.jsonObject(with: Data(contentsOf: recordURL(first))) as! [String: Any]
        check(encoded["owner"] as? String == String(UInt64.max))
        check(encoded["revision"] as? String == String(UInt64.max), "wide identities must not pass through floating point")
        check(first.confirmed && first.canRestore)
        let unknown = store.directory.appendingPathComponent("unrelated-private-note.txt")
        writeFixture(Data("preserve this unrelated file".utf8), to: unknown)
        let second = try save(store, session: session, revision: 2)
        let third = try save(store, session: session, revision: 3)
        check(first.id != second.id && second.id != third.id)
        check(!FileManager.default.fileExists(atPath: first.url.path))
        check(Set(try store.list().map(\.id)) == Set([second.id, third.id]), "keep latest and previous only")
        check(try Data(contentsOf: unknown) == Data("preserve this unrelated file".utf8))

        let anotherWindow = AtlasDeskRecoveryStore(directory: store.directory, limits: store.limits)
        let other = try save(anotherWindow, session: UUID(), revision: 1)
        check(try store.list().count == 3, "windows must have separate generation namespaces")
        let restored = try store.withCheckpoint(third) { url in try Data(contentsOf: url) }
        check(restored == checkpoint)
        try store.withCheckpoint(third) { _ in
            expectError(.busy) { _ = try anotherWindow.list() }
            expectError(.busy) { try anotherWindow.remove(third) }
            expectError(.busy) { _ = try save(anotherWindow, session: session, revision: 4) }
        }
        try store.remove(second)
        check(!FileManager.default.fileExists(atPath: second.url.path))
        expectError(.changed) { _ = try store.withCheckpoint(second) { _ -> Void in preconditionFailure("stale read entered") } }
        check(Set(try store.list().map(\.id)) == Set([third.id, other.id]))
        let forged = AtlasDeskRecoveryEntry(id: third.id, session: third.session, url: unknown,
            savedAt: third.savedAt, sourceBytes: third.sourceBytes, checkpointBytes: third.checkpointBytes)
        expectError(.changed) { try store.remove(forged) }
        check(try Data(contentsOf: unknown) == Data("preserve this unrelated file".utf8))
    }

    private static func failedWrites(_ root: URL) throws {
        let store = Self.store(root, "failures"), session = UUID()
        let previous = try save(store, session: session, revision: 1)
        let before = try fileNames(store.directory)
        expectError(.unconfirmedSave) {
            _ = try store.save(session: session, owner: 4, revision: 2, sourceBytes: 0) { url in
                writeFixture(checkpoint.prefix(40), to: url)
                return AtlasDeskSaveReceipt(effect: "destination-created-incomplete", written: 40,
                    checkpointBytes: UInt64(checkpoint.count), sourceBytes: 0, digest: digest,
                    errorCode: "fixture-interruption", succeeded: false)
            }
        }
        expectError(.invalidCheckpoint) {
            _ = try store.save(session: session, owner: 4, revision: 3, sourceBytes: 0) { url in
                writeFixture(checkpoint, to: url)
                return receipt(digest: String(repeating: "0", count: 64))
            }
        }
        expectError(.unconfirmedSave) {
            _ = try store.save(session: session, owner: 4, revision: 4, sourceBytes: 1) { url in
                writeFixture(checkpoint, to: url)
                return receipt()
            }
        }
        do {
            _ = try store.save(session: session, owner: 4, revision: 5, sourceBytes: 0) { url in
                writeFixture(checkpoint, to: url)
                throw FixtureError.interrupted
            }
            preconditionFailure("interrupted writer unexpectedly succeeded")
        } catch FixtureError.interrupted { }
        check(try fileNames(store.directory) == before, "unpublished candidates must not accumulate after ordinary failures")
        check(try store.list() == [previous], "failed attempts must preserve the last confirmed generation")
        check(try Data(contentsOf: previous.url) == checkpoint)
    }

    private static func quotaAndSessionBound(_ root: URL) throws {
        let store = Self.store(root, "quota", sessions: 1), firstSession = UUID()
        let entry = try save(store, session: firstSession, revision: 1)
        var total: UInt64 = 0
        for name in try fileNames(store.directory) {
            let attributes = try FileManager.default.attributesOfItem(atPath: store.directory.appendingPathComponent(name).path)
            total += (attributes[.size] as! NSNumber).uint64Value
        }
        let bounded = AtlasDeskRecoveryStore(directory: store.directory, limits: .init(maximumSessions: 1,
            maximumBytes: total + 4096 + UInt64(checkpoint.count) - 1,
            maximumCheckpointBytes: UInt64(checkpoint.count)))
        expectError(.quota) {
            _ = try bounded.save(session: firstSession, owner: 4, revision: 2, sourceBytes: 0) { _ in
                preconditionFailure("a denied reservation entered the writer")
            }
        }
        expectError(.sessionLimit) { _ = try save(store, session: UUID(), revision: 1) }
        check(try store.list() == [entry])
        try store.remove(entry)
        let replacement = try save(store, session: UUID(), revision: 1)
        check(try store.list() == [replacement], "explicit deletion must release a desk slot")
    }

    private static func orphanRecovery(_ root: URL) throws {
        let store = Self.store(root, "orphan", sessions: 1), session = UUID()
        let committed = try save(store, session: session, revision: 1)
        // Simulate a crash after checkpoint synchronization, before publication.
        let movedRecord = root.appendingPathComponent("orphan-record-for-inspection.json")
        try FileManager.default.moveItem(at: recordURL(committed), to: movedRecord)
        let orphan = try store.list().first!
        check(orphan.id == committed.id && !orphan.confirmed && orphan.canRestore && orphan.sourceBytes == 0)
        let selected = try store.withCheckpoint(orphan) { url in try Data(contentsOf: url) }
        check(selected == checkpoint, "explicit core validation must be able to inspect a crash-only checkpoint")
        // A changed candidate cannot inherit an earlier inspection/deletion choice.
        writeFixture(checkpoint + Data([0]), to: orphan.url, createOnly: false)
        expectError(.changed) { try store.remove(orphan) }
        expectError(.changed) { _ = try store.withCheckpoint(orphan) { _ -> Void in preconditionFailure("changed orphan entered reader") } }
        let changed = try store.list().first!
        check(!changed.confirmed && changed.checkpointBytes == 105)
        try store.remove(changed)
        check(try store.list().isEmpty)
        check(FileManager.default.fileExists(atPath: movedRecord.path), "unrelated inspection copy must survive")

        let recordOnly = try save(store, session: UUID(), revision: 1)
        let movedCheckpoint = root.appendingPathComponent("missing-payload-for-inspection.fcbdesk")
        try FileManager.default.moveItem(at: recordOnly.url, to: movedCheckpoint)
        let incomplete = try store.list().first!
        check(!incomplete.confirmed && !incomplete.canRestore && incomplete.checkpointBytes == 0)
        expectError(.invalidCheckpoint) {
            _ = try store.withCheckpoint(incomplete) { _ -> Void in preconditionFailure("record-only generation entered reader") }
        }
        try store.remove(incomplete)
        check(try store.list().isEmpty, "record-only crashes must be visible and explicitly removable")

        let previous = try save(store, session: session, revision: 2)
        let torn = try save(store, session: session, revision: 3)
        writeFixture(Data("{\"schema\":".utf8), to: recordURL(torn), createOnly: false)
        let listed = try store.list()
        check(listed.contains(previous) && listed.contains(where: { $0.id == torn.id && !$0.confirmed }))
        let unconfirmed = listed.first { !$0.confirmed }!
        try store.remove(unconfirmed)
        check(try store.list() == [previous], "torn metadata must not hide or delete an earlier valid copy")
    }

    private static func confinement(_ root: URL) throws {
        let publicDirectory = root.appendingPathComponent("public-permissions", isDirectory: true)
        try FileManager.default.createDirectory(at: publicDirectory, withIntermediateDirectories: false,
            attributes: [.posixPermissions: 0o755])
        check(chmod(publicDirectory.path, mode_t(0o755)) == 0)
        expectError(.unsafeLocation) { _ = try AtlasDeskRecoveryStore(directory: publicDirectory).list() }
        check(try fileNames(publicDirectory).isEmpty)

        let protected = Self.store(root, "protected"), entry = try save(protected, session: UUID(), revision: 1)
        let linkedDirectory = root.appendingPathComponent("linked-directory", isDirectory: true)
        try FileManager.default.createSymbolicLink(atPath: linkedDirectory.path, withDestinationPath: protected.directory.path)
        expectError(.unsafeLocation) { _ = try AtlasDeskRecoveryStore(directory: linkedDirectory).list() }

        let external = root.appendingPathComponent("external-payload.fcbdesk")
        try FileManager.default.moveItem(at: entry.url, to: external)
        try FileManager.default.createSymbolicLink(atPath: entry.url.path, withDestinationPath: external.path)
        expectError(.unsafeLocation) { _ = try protected.list() }
        expectError(.unsafeLocation) { try protected.remove(entry) }
        check(try Data(contentsOf: external) == checkpoint, "no-follow operations must preserve symlink targets")

        let hardLinks = Self.store(root, "hard-links"), linked = try save(hardLinks, session: UUID(), revision: 1)
        let otherName = hardLinks.directory.appendingPathComponent("another-hard-link")
        check(link(linked.url.path, otherName.path) == 0)
        expectError(.unsafeLocation) { _ = try hardLinks.list() }
        check(try Data(contentsOf: linked.url) == checkpoint)
    }
}
