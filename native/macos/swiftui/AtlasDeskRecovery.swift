import Foundation
#if canImport(Darwin)
import Darwin
#elseif canImport(Glibc)
import Glibc
#endif

// Darwin and Glibc also import a struct named flock. Use an unambiguous scalar
// entry point for the system C function, as in the existing native bridge.
// int flock(int descriptor, int operation); no pointer ownership crosses here.
@_silgen_name("flock") private func atlasDeskRecoveryFlock(_ descriptor: Int32, _ operation: Int32) -> Int32

/// An explicitly selected recovery copy. Its checkpoint is a retained desk, not
/// permission to reopen the source paths recorded inside that desk.
struct AtlasDeskRecoveryEntry: Identifiable, Equatable, Sendable {
    let id: UUID
    let session: UUID
    let url: URL
    let savedAt: Date
    let sourceBytes: UInt64
    let checkpointBytes: UInt64
    let confirmed: Bool
    fileprivate let storageIdentity: String?

    init(id: UUID, session: UUID, url: URL, savedAt: Date, sourceBytes: UInt64,
        checkpointBytes: UInt64, confirmed: Bool = true, storageIdentity: String? = nil) {
        self.id = id; self.session = session; self.url = url; self.savedAt = savedAt
        self.sourceBytes = sourceBytes; self.checkpointBytes = checkpointBytes
        self.confirmed = confirmed; self.storageIdentity = storageIdentity
    }

    var canRestore: Bool { checkpointBytes >= 56 && checkpointBytes <= 64 * 1024 * 1024 }
}

enum AtlasDeskRecoveryError: Error, Equatable, Sendable {
    case unavailable, unsafeLocation, busy, quota, sessionLimit, generationLimit
    case invalidCheckpoint, changed, unconfirmedSave, filesystem

    var message: String {
        switch self {
        case .unavailable:
            return "Private reading desk recovery is unavailable. Save a checkpoint to a location you choose."
        case .unsafeLocation:
            return "The recovery folder could not be verified as private, owned storage. Existing copies were preserved."
        case .busy:
            return "Another window is using reading desk recovery. Try again after that operation finishes."
        case .quota:
            return "Reading desk recovery has reached its disk budget. Remove an unneeded recovery copy or save a checkpoint elsewhere."
        case .sessionLimit:
            return "Recovery already contains the maximum number of reading desks. Remove an unneeded desk’s copies or save a checkpoint elsewhere."
        case .generationLimit:
            return "This desk has recovery files that could not be retired safely. Review and remove an unneeded or unconfirmed copy, or save a checkpoint elsewhere."
        case .invalidCheckpoint:
            return "The recovery checkpoint does not match its verified save receipt. The previous recovery copies were preserved."
        case .changed:
            return "The selected recovery copy changed or is no longer available. Refresh the recovery list before trying again."
        case .unconfirmedSave:
            return "Recovery was not confirmed. The previous recovery copies were preserved; the current desk still needs to be saved."
        case .filesystem:
            return "The recovery filesystem operation did not complete. Existing recovery copies may still be available; refresh the list or save a checkpoint elsewhere."
        }
    }
}

/// Bounded, opt-in recovery for accepted Reading Desk revisions. Initialization
/// is inert; every method belongs on the existing serialized desk I/O worker.
///
/// The core remains the checkpoint serializer and integrity authority. A native
/// commit record is published only after a successful, decoded core receipt and
/// matching regular-file length and a full receipt-bound checksum. Opening a copy still uses the
/// core's full checksum and invariant validation. Nothing here reads live sources.
///
/// Store-owned I/O is descriptor-relative and never follows symlinks. The bridge
/// currently accepts a pathname, so its write/read is additionally bracketed by
/// directory-identity checks. This private, current-user directory is not a claim
/// of confinement against a hostile process running as the same user.
struct AtlasDeskRecoveryStore: Sendable {
    /// Must validate all bytes through this already-open regular file against
    /// the core save receipt. Returning normally is the only successful outcome.
    /// No callback may close the borrowed descriptor or change its cursor.
    typealias IntegrityVerifier = @Sendable (Int32, UInt64, [UInt8]) throws -> Void

    struct Limits: Sendable {
        let maximumSessions: Int
        let maximumBytes: UInt64
        let maximumCheckpointBytes: UInt64

        init(maximumSessions: Int = 8, maximumBytes: UInt64 = 256 * 1024 * 1024,
            maximumCheckpointBytes: UInt64 = 64 * 1024 * 1024) {
            self.maximumSessions = maximumSessions
            self.maximumBytes = maximumBytes
            self.maximumCheckpointBytes = maximumCheckpointBytes
        }
        static let standard = Self()
    }

    let directory: URL
    let limits: Limits
    private let verifyIntegrity: IntegrityVerifier
    private static let recordLimit = 4096
    private static let markerName = ".fcb-reading-desk-recovery-v1"
    private static let lockName = ".recovery.lock"
    private static let marker = Data("Franken Code Browser reading desk recovery\nversion 1\n".utf8)

    init(directory: URL, limits: Limits = .standard,
        verifyIntegrity: @escaping IntegrityVerifier = { _, _, _ in throw AtlasDeskRecoveryError.unavailable }) {
        self.directory = directory
        self.limits = limits
        self.verifyIntegrity = verifyIntegrity
    }

    static func applicationSupport(verifyIntegrity: @escaping IntegrityVerifier) throws -> Self {
        guard let base = FileManager.default.urls(for: .applicationSupportDirectory,
            in: .userDomainMask).first else { throw AtlasDeskRecoveryError.unavailable }
        return Self(directory: base.appendingPathComponent("FrankenCodeBrowser", isDirectory: true)
            .appendingPathComponent("ReadingDeskRecovery-v1", isDirectory: true), verifyIntegrity: verifyIntegrity)
    }

    /// A read never creates storage, prunes copies, or replaces the open desk.
    func list() throws -> [AtlasDeskRecoveryEntry] {
        try withDirectory(create: false) { fd in
            let inventory = try scan(fd)
            let confirmed = inventory.records.values.flatMap { records in
                records.sorted { $0.sequence > $1.sequence }.prefix(2).map(\.entry)
            }
            return (confirmed + inventory.unconfirmed).sorted {
                if $0.savedAt != $1.savedAt { return $0.savedAt > $1.savedAt }
                return $0.id.uuidString < $1.id.uuidString
            }
        } ?? []
    }

    /// The closure must invoke the existing create-only checkpoint writer and
    /// decode its receipt against the exact accepted desk revision being saved.
    /// It executes while the cross-window/process lock is held, off the UI thread.
    func save(session: UUID, owner: UInt64, revision: UInt64, sourceBytes: UInt64,
        write: (URL) throws -> AtlasDeskSaveReceipt) throws -> AtlasDeskRecoveryEntry {
        guard owner != 0, sourceBytes <= limits.maximumCheckpointBytes else {
            throw AtlasDeskRecoveryError.invalidCheckpoint
        }
        guard let saved = try withDirectory(create: true, body: { fd in
            var inventory = try scan(fd)
            // Finish an interrupted post-commit rotation before allocating any
            // new generation. Only verified older copies of this desk qualify.
            if (inventory.records[session]?.count ?? 0) > 2 {
                try retireOlderCopies(session, inventory: inventory, fd: fd)
                inventory = try scan(fd)
            }
            if inventory.generations[session] == nil && inventory.generations.count >= limits.maximumSessions {
                throw AtlasDeskRecoveryError.sessionLimit
            }
            // At most current + previous + one in-flight/crash-overlap generation.
            guard (inventory.generations[session]?.count ?? 0) < 3 else {
                throw AtlasDeskRecoveryError.generationLimit
            }
            let (reservation, overflow) = limits.maximumCheckpointBytes.addingReportingOverflow(UInt64(Self.recordLimit))
            guard !overflow, inventory.bytes <= limits.maximumBytes,
                  reservation <= limits.maximumBytes - inventory.bytes else {
                throw AtlasDeskRecoveryError.quota
            }
            let previousSequence = inventory.records[session]?.map(\.sequence).max() ?? 0
            guard previousSequence < UInt64.max else { throw AtlasDeskRecoveryError.generationLimit }
            let id = UUID(), names = Names(session: session, generation: id)
            try requireAbsent(names.checkpoint, fd: fd)
            try requireAbsent(names.record, fd: fd)
            let url = directory.appendingPathComponent(names.checkpoint, isDirectory: false)
            var publicationStarted = false
            do {
                try verifyDirectoryIdentity(fd)
                let receipt = try write(url)
                try verifyDirectoryIdentity(fd)
                guard receipt.succeeded, receipt.effect == "complete-file-and-parent-sync-requested",
                      receipt.errorCode == nil, receipt.sourceBytes == sourceBytes,
                      receipt.checkpointBytes >= 56, receipt.checkpointBytes <= limits.maximumCheckpointBytes,
                      receipt.written == receipt.checkpointBytes, let digest = receipt.digest,
                      Self.validDigest(digest) else { throw AtlasDeskRecoveryError.unconfirmedSave }
                try verifyCheckpoint(names.checkpoint, bytes: receipt.checkpointBytes, digest: digest, fd: fd)
                let milliseconds = Int64((Date().timeIntervalSince1970 * 1000).rounded(.down))
                let record = Record(schema: "fcb.native-desk-recovery/1", session: names.session,
                    generation: names.generation, sequence: String(previousSequence + 1), owner: String(owner),
                    revision: String(revision), savedAtMilliseconds: String(milliseconds),
                    sourceBytes: String(sourceBytes), checkpointBytes: String(receipt.checkpointBytes), digest: digest)
                let data = try JSONEncoder().encode(record)
                guard data.count <= Self.recordLimit else { throw AtlasDeskRecoveryError.invalidCheckpoint }
                // Once publication starts, preserve an uncertain result for
                // inspection/recovery instead of deleting a possibly committed copy.
                publicationStarted = true
                try writeNewFile(names.record, data: data, fd: fd)
                try syncDirectory(fd)
                guard let stored = record.stored(names: names, directory: directory, limits: limits) else {
                    throw AtlasDeskRecoveryError.invalidCheckpoint
                }
                // A cleanup failure cannot turn a confirmed save into a failure.
                // The next write must finish rotation before allocating again.
                // Cooperating writers remain excluded by the same lock. Reuse
                // the fully verified inventory and this just-verified save;
                // rescanning every payload would repeat the full hash pass.
                inventory.records[session, default: []].append(stored)
                try? retireOlderCopies(session, inventory: inventory, fd: fd)
                return stored.entry
            } catch {
                if !publicationStarted {
                    // Only this invocation's unique, unpublished candidate is
                    // disposable. Unknown files and prior copies are untouched.
                    try? unlinkManagedFile(names.checkpoint, fd: fd, allowMissing: true)
                    try? syncDirectory(fd)
                }
                throw error
            }
        }) else { throw AtlasDeskRecoveryError.unavailable }
        return saved
    }

    /// Revalidate a selected entry and pin it against cooperating writers for the
    /// entire core restore. The core checks the complete document before adoption.
    func withCheckpoint<T: Sendable>(_ entry: AtlasDeskRecoveryEntry,
        read: (URL) throws -> T) throws -> T {
        guard let value = try withDirectory(create: false, body: { fd in
            if entry.confirmed { _ = try selected(entry, fd: fd) }
            else { _ = try selectedUnconfirmed(entry, fd: fd) }
            guard entry.canRestore, entry.checkpointBytes <= limits.maximumCheckpointBytes else {
                throw AtlasDeskRecoveryError.invalidCheckpoint
            }
            try verifyDirectoryIdentity(fd)
            let result = try read(entry.url)
            try verifyDirectoryIdentity(fd)
            return result
        }) else { throw AtlasDeskRecoveryError.changed }
        return value
    }

    /// Explicit user deletion only. No quota policy evicts another desk's notes.
    func remove(_ entry: AtlasDeskRecoveryEntry) throws {
        guard try withDirectory(create: false, body: { fd in
            if entry.confirmed {
                let stored = try selected(entry, fd: fd)
                try removeStored(stored, fd: fd)
            } else {
                let names = try selectedUnconfirmed(entry, fd: fd)
                try unlinkManagedFile(names.record, fd: fd, allowMissing: true)
                try syncDirectory(fd)
                try unlinkManagedFile(names.checkpoint, fd: fd, allowMissing: true)
                try syncDirectory(fd)
            }
            return true
        }) == true else { throw AtlasDeskRecoveryError.changed }
    }

    private struct Names {
        let session: String
        let generation: String
        var checkpoint: String { "desk_\(session)_\(generation).fcbdesk" }
        var record: String { "desk_\(session)_\(generation).json" }
        init(session: UUID, generation: UUID) {
            self.session = session.uuidString.lowercased()
            self.generation = generation.uuidString.lowercased()
        }
        init?(_ filename: String) {
            let stem: String
            if filename.hasSuffix(".fcbdesk") { stem = String(filename.dropLast(8)) }
            else if filename.hasSuffix(".json") { stem = String(filename.dropLast(5)) }
            else { return nil }
            let pieces = stem.split(separator: "_", omittingEmptySubsequences: false)
            guard pieces.count == 3, pieces[0] == "desk",
                  let session = UUID(uuidString: String(pieces[1])), let generation = UUID(uuidString: String(pieces[2])),
                  session.uuidString.lowercased() == pieces[1], generation.uuidString.lowercased() == pieces[2] else { return nil }
            self.init(session: session, generation: generation)
        }
    }

    private struct Record: Codable {
        let schema, session, generation, sequence, owner, revision, savedAtMilliseconds: String
        let sourceBytes, checkpointBytes, digest: String
        func stored(names: Names, directory: URL, limits: Limits) -> Stored? {
            guard schema == "fcb.native-desk-recovery/1", session == names.session, generation == names.generation,
                  let sessionID = UUID(uuidString: session), let generationID = UUID(uuidString: generation),
                  let sequenceValue = UInt64(sequence), sequenceValue > 0, String(sequenceValue) == sequence,
                  let ownerValue = UInt64(owner), ownerValue > 0, String(ownerValue) == owner,
                  let revisionValue = UInt64(revision), String(revisionValue) == revision,
                  let milliseconds = Int64(savedAtMilliseconds), milliseconds >= 0,
                  milliseconds <= 253_402_300_799_000, String(milliseconds) == savedAtMilliseconds,
                  let source = UInt64(sourceBytes), String(source) == sourceBytes,
                  let bytes = UInt64(checkpointBytes), String(bytes) == checkpointBytes,
                  bytes >= 56, bytes <= limits.maximumCheckpointBytes, source <= bytes,
                  AtlasDeskRecoveryStore.validDigest(digest) else { return nil }
            let entry = AtlasDeskRecoveryEntry(id: generationID, session: sessionID,
                url: directory.appendingPathComponent(names.checkpoint, isDirectory: false),
                savedAt: Date(timeIntervalSince1970: Double(milliseconds) / 1000),
                sourceBytes: source, checkpointBytes: bytes)
            return Stored(entry: entry, names: names, sequence: sequenceValue, digest: digest)
        }
    }

    private struct Stored {
        let entry: AtlasDeskRecoveryEntry
        let names: Names
        let sequence: UInt64
        let digest: String
    }
    private struct Inventory {
        var bytes: UInt64 = 0
        var generations: [UUID: Set<UUID>] = [:]
        var records: [UUID: [Stored]] = [:]
        var unconfirmed: [AtlasDeskRecoveryEntry] = []
    }

    private func scan(_ fd: Int32) throws -> Inventory {
        var inventory = Inventory()
        var files: [String: stat] = [:]
        var candidates: [String: Names] = [:]
        for name in try filenames(fd) {
            let file = try openRegularFile(name, fd: fd)
            let info: stat
            do { info = try fileInfo(file) } catch { close(file); throw error }
            close(file)
            let (total, overflow) = inventory.bytes.addingReportingOverflow(UInt64(info.st_size))
            guard !overflow else { throw AtlasDeskRecoveryError.quota }
            inventory.bytes = total
            guard let names = Names(name), let session = UUID(uuidString: names.session),
                  let generation = UUID(uuidString: names.generation) else { continue }
            files[name] = info
            candidates[names.checkpoint] = names
            inventory.generations[session, default: []].insert(generation)
            guard name == names.record else { continue }
            do {
                let stored = try readStored(names, fd: fd)
                if inventory.records[session, default: []].contains(where: { $0.sequence == stored.sequence }) {
                    throw AtlasDeskRecoveryError.changed
                }
                inventory.records[session, default: []].append(stored)
            } catch AtlasDeskRecoveryError.invalidCheckpoint {
                // A torn/corrupt generation never hides an earlier valid copy.
                // Preserve it, count its bytes, and do not select it for retirement.
                continue
            }
        }
        for names in candidates.values {
            guard let session = UUID(uuidString: names.session), let generation = UUID(uuidString: names.generation),
                  !(inventory.records[session] ?? []).contains(where: { $0.entry.id == generation }) else { continue }
            let checkpoint = files[names.checkpoint], record = files[names.record]
            guard let observed = checkpoint ?? record else { continue }
            // A crash may leave the only copy before its commit record exists.
            // Keep it visible for explicit core-validated restore or deletion;
            // never promote it to a confirmed save, or infer source byte counts.
            inventory.unconfirmed.append(AtlasDeskRecoveryEntry(id: generation, session: session,
                url: directory.appendingPathComponent(names.checkpoint, isDirectory: false),
                savedAt: modifiedAt(observed), sourceBytes: 0,
                checkpointBytes: checkpoint.map { UInt64($0.st_size) } ?? 0, confirmed: false,
                storageIdentity: signature(checkpoint) + "/" + signature(record)))
        }
        return inventory
    }

    private func readStored(_ names: Names, fd: Int32) throws -> Stored {
        let data = try readFile(names.record, maximum: Self.recordLimit, fd: fd)
        guard let record = try? JSONDecoder().decode(Record.self, from: data),
              let stored = record.stored(names: names, directory: directory, limits: limits) else {
            throw AtlasDeskRecoveryError.invalidCheckpoint
        }
        do {
            try verifyCheckpoint(names.checkpoint, bytes: stored.entry.checkpointBytes, digest: stored.digest, fd: fd)
        } catch AtlasDeskRecoveryError.changed { throw AtlasDeskRecoveryError.invalidCheckpoint }
        return stored
    }

    private func selected(_ entry: AtlasDeskRecoveryEntry, fd: Int32) throws -> Stored {
        let names = Names(session: entry.session, generation: entry.id)
        guard entry.url == directory.appendingPathComponent(names.checkpoint, isDirectory: false) else {
            throw AtlasDeskRecoveryError.changed
        }
        let stored = try readStored(names, fd: fd)
        guard stored.entry == entry else { throw AtlasDeskRecoveryError.changed }
        return stored
    }

    private func selectedUnconfirmed(_ entry: AtlasDeskRecoveryEntry, fd: Int32) throws -> Names {
        guard !entry.confirmed, try scan(fd).unconfirmed.contains(entry) else {
            throw AtlasDeskRecoveryError.changed
        }
        return Names(session: entry.session, generation: entry.id)
    }

    private func retireOlderCopies(_ session: UUID, inventory: Inventory, fd: Int32) throws {
        let records = (inventory.records[session] ?? []).sorted { $0.sequence > $1.sequence }
        for retired in records.dropFirst(2) { try removeStored(retired, fd: fd) }
    }

    private func removeStored(_ stored: Stored, fd: Int32) throws {
        _ = try selected(stored.entry, fd: fd)
        // Unpublish first; an interrupted deletion leaves an unlisted checkpoint,
        // never a commit record pointing to a deliberately removed payload.
        try unlinkManagedFile(stored.names.record, fd: fd)
        try syncDirectory(fd)
        try unlinkManagedFile(stored.names.checkpoint, fd: fd)
        try syncDirectory(fd)
    }

    private static func validDigest(_ value: String) -> Bool {
        value.utf8.count == 64 && value.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }

    private func verifyCheckpoint(_ name: String, bytes: UInt64, digest: String, fd: Int32) throws {
        let file = try openRegularFile(name, fd: fd)
        defer { close(file) }
        let before = try fileInfo(file)
        guard bytes >= 56, bytes <= limits.maximumCheckpointBytes, UInt64(before.st_size) == bytes else {
            throw AtlasDeskRecoveryError.invalidCheckpoint
        }
        let header = try readRange(file, offset: 0, count: 24)
        let expected: [UInt8] = [0x46, 0x43, 0x42, 0x4b, 0x44, 0x45, 0x53, 0x4b, 1, 0]
        guard Array(header.prefix(10)) == expected, header[10] <= 1, header[11] == 0,
              header[12..<16].allSatisfy({ $0 == 0 }) else { throw AtlasDeskRecoveryError.invalidCheckpoint }
        var payload: UInt64 = 0
        for (index, byte) in header[16..<24].enumerated() { payload |= UInt64(byte) << (index * 8) }
        guard payload == bytes - 56 else { throw AtlasDeskRecoveryError.invalidCheckpoint }
        let trailer = try readRange(file, offset: off_t(bytes - 32), count: 32)
        let hexadecimal = Array("0123456789abcdef".utf8)
        let observed = String(decoding: trailer.flatMap { [hexadecimal[Int($0 >> 4)], hexadecimal[Int($0 & 15)]] }, as: UTF8.self)
        guard observed == digest, Self.validDigest(digest) else {
            throw AtlasDeskRecoveryError.invalidCheckpoint
        }
        let hexadecimalBytes = Array(digest.utf8)
        func nibble(_ byte: UInt8) -> UInt8 { byte <= 57 ? byte - 48 : byte - 87 }
        let expectedDigest = stride(from: 0, to: hexadecimalBytes.count, by: 2).map {
            nibble(hexadecimalBytes[$0]) << 4 | nibble(hexadecimalBytes[$0 + 1])
        }
        // A copied trailer is not evidence that the retained note/source body
        // still exists intact. Complete verification must precede confirmation,
        // list selection and any automatic retirement of an earlier good copy.
        try verifyIntegrity(file, bytes, expectedDigest)
        let after = try fileInfo(file)
        guard sameFile(before, after), signature(before) == signature(after) else {
            throw AtlasDeskRecoveryError.invalidCheckpoint
        }
    }

    private func withDirectory<T>(create: Bool, body: (Int32) throws -> T) throws -> T? {
        guard (1...8).contains(limits.maximumSessions), limits.maximumCheckpointBytes >= 56,
              limits.maximumCheckpointBytes <= 64 * 1024 * 1024,
              limits.maximumBytes <= 256 * 1024 * 1024 else { throw AtlasDeskRecoveryError.unavailable }
        guard let fd = try openDirectory(create: create) else { return nil }
        defer { close(fd) }
        let flags = O_RDWR | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK | (create ? O_CREAT : 0)
        let lock = openat(fd, Self.lockName, flags, mode_t(0o600))
        guard lock >= 0 else { throw AtlasDeskRecoveryError.unsafeLocation }
        defer { close(lock) }
        try requirePrivateRegular(try fileInfo(lock))
        guard atlasDeskRecoveryFlock(lock, LOCK_EX | LOCK_NB) == 0 else {
            if errno == EWOULDBLOCK || errno == EAGAIN { throw AtlasDeskRecoveryError.busy }
            throw AtlasDeskRecoveryError.filesystem
        }
        defer { _ = atlasDeskRecoveryFlock(lock, LOCK_UN) }
        let visibleLock = try openRegularFile(Self.lockName, fd: fd)
        defer { close(visibleLock) }
        guard sameFile(try fileInfo(lock), try fileInfo(visibleLock)) else { throw AtlasDeskRecoveryError.unsafeLocation }
        do {
            guard try readFile(Self.markerName, maximum: 256, fd: fd) == Self.marker else {
                throw AtlasDeskRecoveryError.unsafeLocation
            }
        } catch AtlasDeskRecoveryError.changed {
            guard create, try filenames(fd).allSatisfy({ $0 == Self.lockName }) else {
                throw AtlasDeskRecoveryError.unsafeLocation
            }
            try writeNewFile(Self.markerName, data: Self.marker, fd: fd)
            try syncDirectory(fd)
        }
        return try body(fd)
    }

    private func openDirectory(create: Bool) throws -> Int32? {
        let path = directory.path
        guard directory.isFileURL, path.hasPrefix("/"), path.utf8.count < 4096,
              !path.utf8.contains(0) else { throw AtlasDeskRecoveryError.unsafeLocation }
        let components = path.split(separator: "/")
        guard !components.isEmpty, components.allSatisfy({ $0 != "." && $0 != ".." && $0.utf8.count <= 255 }) else {
            throw AtlasDeskRecoveryError.unsafeLocation
        }
        var current = open("/", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard current >= 0 else { throw AtlasDeskRecoveryError.filesystem }
        do {
            for component in components {
                let name = String(component)
                var next = openat(current, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
                if next < 0 && errno == ENOENT {
                    if !create { close(current); return nil }
                    if mkdirat(current, name, mode_t(0o700)) != 0 && errno != EEXIST {
                        throw AtlasDeskRecoveryError.filesystem
                    }
                    try syncDirectory(current)
                    next = openat(current, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
                }
                guard next >= 0 else { throw AtlasDeskRecoveryError.unsafeLocation }
                close(current)
                current = next
            }
            let info = try fileInfo(current)
            guard (info.st_mode & mode_t(S_IFMT)) == mode_t(S_IFDIR), info.st_uid == geteuid(),
                  info.st_mode & mode_t(0o077) == 0 else { throw AtlasDeskRecoveryError.unsafeLocation }
            return current
        } catch { close(current); throw error }
    }

    private func verifyDirectoryIdentity(_ fd: Int32) throws {
        guard let visible = try openDirectory(create: false) else { throw AtlasDeskRecoveryError.changed }
        defer { close(visible) }
        guard sameFile(try fileInfo(fd), try fileInfo(visible)) else { throw AtlasDeskRecoveryError.changed }
    }

    private func filenames(_ fd: Int32) throws -> [String] {
        // openat(".") has its own directory offset; dup would share the original
        // descriptor's seek position and make a second scan silently empty.
        let copy = openat(fd, ".", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard copy >= 0 else { throw AtlasDeskRecoveryError.filesystem }
        guard let stream = fdopendir(copy) else { close(copy); throw AtlasDeskRecoveryError.filesystem }
        defer { closedir(stream) }
        var names: [String] = []
        while true {
            errno = 0
            guard let entry = readdir(stream) else {
                guard errno == 0 else { throw AtlasDeskRecoveryError.filesystem }
                break
            }
            var bytes = entry.pointee.d_name
            let capacity = MemoryLayout.size(ofValue: bytes)
            let name = withUnsafePointer(to: &bytes) { pointer in
                pointer.withMemoryRebound(to: CChar.self, capacity: capacity) {
                    String(cString: $0)
                }
            }
            if name == "." || name == ".." { continue }
            guard names.count < 2 + limits.maximumSessions * 8 else { throw AtlasDeskRecoveryError.generationLimit }
            names.append(name)
        }
        return names.sorted()
    }

    private func fileInfo(_ fd: Int32) throws -> stat {
        var info = stat()
        guard fstat(fd, &info) == 0, info.st_size >= 0 else { throw AtlasDeskRecoveryError.filesystem }
        return info
    }
    private func sameFile(_ first: stat, _ second: stat) -> Bool {
        first.st_dev == second.st_dev && first.st_ino == second.st_ino
    }
    private func modifiedAt(_ info: stat) -> Date {
#if canImport(Darwin)
        let time = info.st_mtimespec
#else
        let time = info.st_mtim
#endif
        return Date(timeIntervalSince1970: Double(time.tv_sec) + Double(time.tv_nsec) / 1_000_000_000)
    }
    private func signature(_ value: stat?) -> String {
        guard let info = value else { return "absent" }
#if canImport(Darwin)
        let modified = info.st_mtimespec, changed = info.st_ctimespec
#else
        let modified = info.st_mtim, changed = info.st_ctim
#endif
        return "\(info.st_dev):\(info.st_ino):\(info.st_size):\(modified.tv_sec):\(modified.tv_nsec):\(changed.tv_sec):\(changed.tv_nsec)"
    }
    private func requirePrivateRegular(_ info: stat) throws {
        guard (info.st_mode & mode_t(S_IFMT)) == mode_t(S_IFREG), info.st_uid == geteuid(),
              info.st_nlink == 1, info.st_mode & mode_t(0o077) == 0 else { throw AtlasDeskRecoveryError.unsafeLocation }
    }
    private func openRegularFile(_ name: String, fd: Int32) throws -> Int32 {
        let file = openat(fd, name, O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK)
        guard file >= 0 else {
            if errno == ENOENT { throw AtlasDeskRecoveryError.changed }
            throw AtlasDeskRecoveryError.unsafeLocation
        }
        do { try requirePrivateRegular(try fileInfo(file)); return file }
        catch { close(file); throw error }
    }
    private func requireAbsent(_ name: String, fd: Int32) throws {
        var info = stat()
        guard fstatat(fd, name, &info, AT_SYMLINK_NOFOLLOW) != 0, errno == ENOENT else {
            throw AtlasDeskRecoveryError.changed
        }
    }
    private func readRange(_ fd: Int32, offset: off_t, count: Int) throws -> [UInt8] {
        var bytes = [UInt8](repeating: 0, count: count), completed = 0
        while completed < count {
            let received = bytes.withUnsafeMutableBytes { buffer in
                pread(fd, buffer.baseAddress!.advanced(by: completed), count - completed, offset + off_t(completed))
            }
            if received < 0 && errno == EINTR { continue }
            guard received > 0 else { throw AtlasDeskRecoveryError.invalidCheckpoint }
            completed += received
        }
        return bytes
    }
    private func readFile(_ name: String, maximum: Int, fd: Int32) throws -> Data {
        let file = try openRegularFile(name, fd: fd)
        defer { close(file) }
        let before = try fileInfo(file)
        guard before.st_size <= maximum else { throw AtlasDeskRecoveryError.invalidCheckpoint }
        let bytes = try readRange(file, offset: 0, count: Int(before.st_size))
        let after = try fileInfo(file)
        guard before.st_size == after.st_size else { throw AtlasDeskRecoveryError.changed }
        return Data(bytes)
    }
    private func writeNewFile(_ name: String, data: Data, fd: Int32) throws {
        let file = openat(fd, name, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, mode_t(0o600))
        guard file >= 0 else { throw AtlasDeskRecoveryError.filesystem }
        defer { close(file) }
        try requirePrivateRegular(try fileInfo(file))
        var completed = 0
        while completed < data.count {
            let written = data.withUnsafeBytes { buffer in
                write(file, buffer.baseAddress!.advanced(by: completed), data.count - completed)
            }
            if written < 0 && errno == EINTR { continue }
            guard written > 0 else { throw AtlasDeskRecoveryError.filesystem }
            completed += written
        }
        guard fsync(file) == 0 else { throw AtlasDeskRecoveryError.filesystem }
    }
    private func syncDirectory(_ fd: Int32) throws {
        guard fsync(fd) == 0 else { throw AtlasDeskRecoveryError.filesystem }
    }
    private func unlinkManagedFile(_ name: String, fd: Int32, allowMissing: Bool = false) throws {
        let file: Int32
        do { file = try openRegularFile(name, fd: fd) }
        catch AtlasDeskRecoveryError.changed where allowMissing { return }
        defer { close(file) }
        var visible = stat()
        guard fstatat(fd, name, &visible, AT_SYMLINK_NOFOLLOW) == 0,
              sameFile(try fileInfo(file), visible) else { throw AtlasDeskRecoveryError.changed }
        try requirePrivateRegular(visible)
        guard unlinkat(fd, name, 0) == 0 else { throw AtlasDeskRecoveryError.filesystem }
    }
}
