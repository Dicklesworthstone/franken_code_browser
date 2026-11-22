import Foundation
import Observation
import CryptoKit
#if canImport(Darwin)
import Darwin
#else
import Glibc
#endif

@_silgen_name("flock") private func atlasFolderViewFlock(_ descriptor: Int32, _ operation: Int32) -> Int32

enum AtlasFolderPersistenceError: Error, Equatable, Sendable {
    case invalidState, corrupt, conflict, busy, limit, unsafeLocation, unavailable

    var message: String {
        switch self {
        case .invalidState: return "This atlas view could not be saved because its navigation data is invalid."
        case .corrupt: return "Saved atlas views could not be verified. Existing saved data has been preserved."
        case .conflict: return "Another window saved this project's view. Reload saved views before replacing that version."
        case .busy: return "Saved atlas views are in use. Retry saving when the other operation finishes."
        case .limit: return "Saved atlas views reached their bounded storage allowance. The current view remains available in this window."
        case .unsafeLocation: return "The atlas-view storage location could not be verified. Existing files have been preserved."
        case .unavailable: return "Atlas views could not be saved on this Mac. The current view remains available in this window."
        }
    }
}

/// A personal map locator, never a root access grant or an exact captured-source
/// identity. Raw UTF-8 root bytes distinguish canonically equivalent path names.
struct AtlasFolderPersistenceContext: Equatable, Sendable {
    let root: String
    let policy: UInt64
    init(root: String, policy: UInt64) throws {
        guard root.hasPrefix("/"), root.utf8.count <= 16_384, !root.utf8.contains(0),
              policy <= 1 else { throw AtlasFolderPersistenceError.invalidState }
        self.root = root; self.policy = policy
    }
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.policy == rhs.policy && lhs.root.utf8.elementsEqual(rhs.root.utf8)
    }
    var filename: String {
        let key = Data(("fcb.atlas-views/1\u{0}\(policy)\u{0}" + root).utf8)
        return SHA256.hash(data: key).map { String(format: "%02x", $0) }.joined() + ".view"
    }
}

struct AtlasFolderPersistenceReceipt: Sendable {
    let revision: UUID?
    let snapshot: AtlasFolderNavigation.Snapshot?
    let synchronizationConfirmed: Bool
}

/// Separate from all derived project caches. Records are checksum-checked,
/// versioned and bound to root bytes plus discovery policy. A cooperating
/// process must take the same nonblocking lock; stale windows cannot overwrite
/// a newer accepted generation. I/O runs only on the native utility worker.
struct AtlasFolderViewStore: Sendable {
    static let maximumRecordBytes = 2 * 1024 * 1024
    static let maximumContexts = 16
    static let maximumStoreBytes: UInt64 = 64 * 1024 * 1024
    private static let marker = Data("fcb.atlas-views/1\n".utf8)
    let directory: URL

    static func applicationSupport() throws -> Self {
        guard let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first else {
            throw AtlasFolderPersistenceError.unavailable
        }
        return Self(directory: base.appendingPathComponent("FrankenCodeBrowser", isDirectory: true)
            .appendingPathComponent("AtlasViews", isDirectory: true))
    }

    func load(_ context: AtlasFolderPersistenceContext) throws -> AtlasFolderPersistenceReceipt {
        try withDirectory(create: false) { fd in
            guard let stored = try read(context.filename, fd: fd) else {
                return AtlasFolderPersistenceReceipt(revision: nil, snapshot: nil, synchronizationConfirmed: true)
            }
            return try decode(stored.data, context: context)
        } ?? AtlasFolderPersistenceReceipt(revision: nil, snapshot: nil, synchronizationConfirmed: true)
    }

    func save(_ snapshot: AtlasFolderNavigation.Snapshot, context: AtlasFolderPersistenceContext,
        expecting revision: UUID?) throws -> AtlasFolderPersistenceReceipt {
        let next = UUID()
        let data = try encode(snapshot, context: context, revision: next)
        guard let receipt = try withDirectory(create: true, body: { fd in
            let existing = try read(context.filename, fd: fd)
            let previous = try existing.map { try decode($0.data, context: context) }
            guard previous?.revision == revision else { throw AtlasFolderPersistenceError.conflict }
            try admitWrite(fd, creating: existing == nil)
            let backupName = context.filename + ".previous"
            let backup = try read(backupName, fd: fd)
            // A malformed prior generation is evidence to preserve, not a slot
            // that an unrelated successful save is allowed to overwrite.
            if let backup { _ = try decode(backup.data, context: context) }
            if let existing {
                try replace(backupName, data: existing.data, expecting: backup?.identity, fd: fd)
            }
            do {
                try replace(context.filename, data: data, expecting: existing?.identity, fd: fd)
                return AtlasFolderPersistenceReceipt(revision: next, snapshot: snapshot, synchronizationConfirmed: true)
            } catch {
                // rename may have committed before directory synchronization
                // failed. Reconcile the operation ID before telling the caller
                // whether retrying with the former generation would be safe.
                if let visible = try? read(context.filename, fd: fd),
                   let accepted = try? decode(visible.data, context: context), accepted.revision == next {
                    return AtlasFolderPersistenceReceipt(revision: next, snapshot: snapshot, synchronizationConfirmed: false)
                }
                throw error
            }
        }) else { throw AtlasFolderPersistenceError.unavailable }
        return receipt
    }

    private struct Seal: Codable { let body: Data; let sha256: Data }
    private struct Record: Codable {
        let version: Int
        let root: Data
        let policy: UInt64
        let revision: UUID
        let current: Place?
        let previous, following: [Place]
        let landmarks: [Landmark]
    }
    private struct Landmark: Codable {
        let id: UUID
        let label: String
        let destination: Place
    }
    private struct Place: Codable {
        let directory: Data
        let scope, extensions: String
        let parcel: Data?
        let camera: Camera?
        init(_ destination: AtlasFolderDestination) {
            directory = Data(destination.directory.rawPath)
            switch destination.fileScope {
            case .all: scope = "all"
            case .markdown: scope = "markdown"
            case .python: scope = "python"
            case .rust: scope = "rust"
            case .custom: scope = "custom"
            }
            extensions = destination.customExtensions
            parcel = destination.anchorRawPath.map { Data($0) }
            camera = destination.cameraAnchor.map { Camera(x: $0.relativeX, y: $0.relativeY, zoom: $0.zoomFactor) }
        }
        func destination() throws -> AtlasFolderDestination {
            let fileScope: AtlasFileScope
            switch scope {
            case "all": fileScope = .all
            case "markdown": fileScope = .markdown
            case "python": fileScope = .python
            case "rust": fileScope = .rust
            case "custom": fileScope = .custom
            default: throw AtlasFolderPersistenceError.corrupt
            }
            var anchor: AtlasParcelCameraAnchor?
            if let camera {
                guard let valid = AtlasParcelCameraAnchor(relativeX: camera.x, relativeY: camera.y, zoomFactor: camera.zoom) else {
                    throw AtlasFolderPersistenceError.corrupt
                }
                anchor = valid
            }
            return try AtlasFolderDestination(directory: AtlasDirectoryFocus(rawPath: Array(directory)),
                fileScope: fileScope, customExtensions: extensions,
                anchorRawPath: parcel.map(Array.init), cameraAnchor: anchor)
        }
        var rawBytes: Int { directory.count + scope.utf8.count + extensions.utf8.count + (parcel?.count ?? 0) + 64 }
    }
    private struct Camera: Codable { let x, y, zoom: Double }

    private func encode(_ snapshot: AtlasFolderNavigation.Snapshot, context: AtlasFolderPersistenceContext,
        revision: UUID) throws -> Data {
        var check = AtlasFolderNavigation()
        do { try check.restore(snapshot) } catch { throw AtlasFolderPersistenceError.invalidState }
        let record = Record(version: 1, root: Data(context.root.utf8), policy: context.policy, revision: revision,
            current: snapshot.history.current.map(Place.init),
            previous: snapshot.history.previous.map(Place.init), following: snapshot.history.following.map(Place.init),
            landmarks: snapshot.landmarks.map { Landmark(id: $0.id, label: $0.label, destination: Place($0.destination)) })
        // Bound aggregate input before JSON/base64 growth. Existing navigation
        // remains usable when unusually long paths exceed persistence admission.
        try validateSize(record)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        let body = try encoder.encode(record)
        guard body.count <= 1024 * 1024 else { throw AtlasFolderPersistenceError.limit }
        let sealed = try encoder.encode(Seal(body: body, sha256: Data(SHA256.hash(data: body))))
        guard sealed.count <= Self.maximumRecordBytes else { throw AtlasFolderPersistenceError.limit }
        return sealed
    }

    private func decode(_ data: Data, context: AtlasFolderPersistenceContext) throws -> AtlasFolderPersistenceReceipt {
        do {
            guard data.count <= Self.maximumRecordBytes else { throw AtlasFolderPersistenceError.corrupt }
            let decoder = JSONDecoder()
            let seal = try decoder.decode(Seal.self, from: data)
            guard seal.body.count <= 1024 * 1024, seal.sha256 == Data(SHA256.hash(data: seal.body)) else {
                throw AtlasFolderPersistenceError.corrupt
            }
            let record = try decoder.decode(Record.self, from: seal.body)
            guard record.version == 1, record.root == Data(context.root.utf8), record.policy == context.policy else {
                throw AtlasFolderPersistenceError.corrupt
            }
            try validateSize(record)
            let snapshot = AtlasFolderNavigation.Snapshot(
                history: .init(current: try record.current?.destination(),
                    previous: try record.previous.map { try $0.destination() },
                    following: try record.following.map { try $0.destination() }),
                landmarks: try record.landmarks.map {
                    .init(id: $0.id, label: $0.label, destination: try $0.destination.destination())
                })
            var check = AtlasFolderNavigation()
            try check.restore(snapshot)
            return AtlasFolderPersistenceReceipt(revision: record.revision, snapshot: snapshot, synchronizationConfirmed: true)
        } catch { throw AtlasFolderPersistenceError.corrupt }
    }

    private func validateSize(_ record: Record) throws {
        guard record.previous.count + record.following.count <= 128, record.landmarks.count <= 32 else {
            throw AtlasFolderPersistenceError.limit
        }
        let places = record.previous + record.following + record.landmarks.map(\.destination)
            + (record.current.map { [$0] } ?? [])
        let bytes = places.reduce(record.root.count) { $0 + $1.rawBytes }
            + record.landmarks.reduce(0) { $0 + $1.label.utf8.count }
        guard bytes <= 512 * 1024 else { throw AtlasFolderPersistenceError.limit }
    }

    private struct Identity: Equatable {
        let device, inode: UInt64
        let size: Int64
        let modifiedSeconds, modifiedNanos, changedSeconds, changedNanos: Int64
        init(_ info: stat) {
            device = UInt64(info.st_dev); inode = UInt64(info.st_ino); size = Int64(info.st_size)
#if canImport(Darwin)
            let modified = info.st_mtimespec, changed = info.st_ctimespec
#else
            let modified = info.st_mtim, changed = info.st_ctim
#endif
            modifiedSeconds = Int64(modified.tv_sec); modifiedNanos = Int64(modified.tv_nsec)
            changedSeconds = Int64(changed.tv_sec); changedNanos = Int64(changed.tv_nsec)
        }
    }
    private struct Read { let data: Data; let identity: Identity }

    private func info(_ fd: Int32) throws -> stat {
        var value = stat()
        guard fstat(fd, &value) == 0, value.st_size >= 0 else { throw AtlasFolderPersistenceError.unavailable }
        return value
    }
    private func regular(_ info: stat) throws {
        guard info.st_mode & mode_t(S_IFMT) == mode_t(S_IFREG), info.st_uid == geteuid(),
              info.st_nlink == 1, info.st_mode & mode_t(0o077) == 0 else {
            throw AtlasFolderPersistenceError.unsafeLocation
        }
    }
    private func openDirectory(create: Bool) throws -> Int32? {
        guard directory.isFileURL, directory.path.hasPrefix("/"), directory.path.utf8.count < 4096,
              !directory.path.utf8.contains(0) else { throw AtlasFolderPersistenceError.unsafeLocation }
        let parts = directory.path.split(separator: "/", omittingEmptySubsequences: true)
        guard !parts.isEmpty, parts.allSatisfy({ $0 != "." && $0 != ".." && $0.utf8.count <= 255 }) else {
            throw AtlasFolderPersistenceError.unsafeLocation
        }
        var fd = open("/", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard fd >= 0 else { throw AtlasFolderPersistenceError.unavailable }
        do {
            for component in parts {
                let name = String(component)
                var next = openat(fd, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
                if next < 0 && errno == ENOENT {
                    if !create { close(fd); return nil }
                    guard mkdirat(fd, name, mode_t(0o700)) == 0 || errno == EEXIST else {
                        throw AtlasFolderPersistenceError.unavailable
                    }
                    guard fsync(fd) == 0 else { throw AtlasFolderPersistenceError.unavailable }
                    next = openat(fd, name, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
                }
                guard next >= 0 else { throw AtlasFolderPersistenceError.unsafeLocation }
                close(fd); fd = next
            }
            let value = try info(fd)
            guard value.st_mode & mode_t(S_IFMT) == mode_t(S_IFDIR), value.st_uid == geteuid(),
                  value.st_mode & mode_t(0o077) == 0 else { throw AtlasFolderPersistenceError.unsafeLocation }
            return fd
        } catch { close(fd); throw error }
    }

    private func withDirectory<Value>(create: Bool, body: (Int32) throws -> Value) throws -> Value? {
        guard let fd = try openDirectory(create: create) else { return nil }
        defer { close(fd) }
        let lock = openat(fd, ".lock", O_RDWR | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC | (create ? O_CREAT : 0), mode_t(0o600))
        guard lock >= 0 else { throw AtlasFolderPersistenceError.unsafeLocation }
        defer { close(lock) }
        try regular(info(lock))
        guard atlasFolderViewFlock(lock, LOCK_EX | LOCK_NB) == 0 else { throw AtlasFolderPersistenceError.busy }
        defer { _ = atlasFolderViewFlock(lock, LOCK_UN) }
        let visibleLock = openat(fd, ".lock", O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)
        guard visibleLock >= 0 else { throw AtlasFolderPersistenceError.unsafeLocation }
        defer { close(visibleLock) }
        let openedLock = try info(lock), currentLock = try info(visibleLock)
        try regular(currentLock)
        guard openedLock.st_dev == currentLock.st_dev, openedLock.st_ino == currentLock.st_ino else {
            throw AtlasFolderPersistenceError.conflict
        }
        if let marker = try read(".owner", fd: fd) {
            guard marker.data == Self.marker else { throw AtlasFolderPersistenceError.unsafeLocation }
        } else {
            guard create, try names(fd) == [".lock"] else { throw AtlasFolderPersistenceError.unsafeLocation }
            try replace(".owner", data: Self.marker, expecting: nil, fd: fd)
        }
        return try body(fd)
    }

    private func names(_ fd: Int32) throws -> [String] {
        let descriptor = openat(fd, ".", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard descriptor >= 0 else { throw AtlasFolderPersistenceError.unavailable }
        guard let stream = fdopendir(descriptor) else { close(descriptor); throw AtlasFolderPersistenceError.unavailable }
        defer { closedir(stream) }
        var result: [String] = []
        while true {
            errno = 0
            guard let entry = readdir(stream) else {
                guard errno == 0 else { throw AtlasFolderPersistenceError.unavailable }
                break
            }
            var raw = entry.pointee.d_name
            let capacity = MemoryLayout.size(ofValue: raw)
            let name = withUnsafePointer(to: &raw) { p in
                p.withMemoryRebound(to: CChar.self, capacity: capacity) { String(cString: $0) }
            }
            if name == "." || name == ".." { continue }
            guard result.count < 80 else { throw AtlasFolderPersistenceError.limit }
            result.append(name)
        }
        return result.sorted()
    }

    private func admitWrite(_ fd: Int32, creating: Bool) throws {
        let names = try names(fd)
        guard !creating || names.filter({ $0.hasSuffix(".view") }).count < Self.maximumContexts else {
            throw AtlasFolderPersistenceError.limit
        }
        var bytes: UInt64 = 0
        for name in names {
            var value = stat()
            guard fstatat(fd, name, &value, AT_SYMLINK_NOFOLLOW) == 0 else { throw AtlasFolderPersistenceError.unavailable }
            try regular(value)
            guard value.st_size >= 0 else { throw AtlasFolderPersistenceError.unsafeLocation }
            let (sum, overflow) = bytes.addingReportingOverflow(UInt64(value.st_size))
            guard !overflow else { throw AtlasFolderPersistenceError.limit }
            bytes = sum
        }
        guard bytes <= Self.maximumStoreBytes - UInt64(Self.maximumRecordBytes * 2) else {
            throw AtlasFolderPersistenceError.limit
        }
    }

    private func read(_ name: String, fd: Int32) throws -> Read? {
        let file = openat(fd, name, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)
        guard file >= 0 else {
            if errno == ENOENT { return nil }
            throw AtlasFolderPersistenceError.unsafeLocation
        }
        defer { close(file) }
        let before = try info(file)
        try regular(before)
        guard before.st_size <= Self.maximumRecordBytes else { throw AtlasFolderPersistenceError.corrupt }
        var data = Data(count: Int(before.st_size)), offset = 0
        while offset < data.count {
            let count = data.withUnsafeMutableBytes { buffer in
                pread(file, buffer.baseAddress!.advanced(by: offset), buffer.count - offset, off_t(offset))
            }
            if count < 0 && errno == EINTR { continue }
            guard count > 0 else { throw AtlasFolderPersistenceError.corrupt }
            offset += count
        }
        guard Identity(before) == Identity(try info(file)) else { throw AtlasFolderPersistenceError.conflict }
        return Read(data: data, identity: Identity(before))
    }

    private func replace(_ name: String, data: Data, expecting expected: Identity?, fd: Int32) throws {
        guard data.count <= Self.maximumRecordBytes else { throw AtlasFolderPersistenceError.limit }
        let temporary = "candidate-" + UUID().uuidString.lowercased() + ".tmp"
        let file = openat(fd, temporary, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, mode_t(0o600))
        guard file >= 0 else { throw AtlasFolderPersistenceError.unavailable }
        // Only this operation's newly created name can be retired. Original or
        // corrupt user-state records are never removed by a failed save.
        defer { close(file); _ = unlinkat(fd, temporary, 0) }
        try regular(info(file))
        var offset = 0
        while offset < data.count {
            let count = data.withUnsafeBytes { buffer in
                write(file, buffer.baseAddress!.advanced(by: offset), buffer.count - offset)
            }
            if count < 0 && errno == EINTR { continue }
            guard count > 0 else { throw AtlasFolderPersistenceError.unavailable }
            offset += count
        }
        guard fsync(file) == 0 else { throw AtlasFolderPersistenceError.unavailable }
        let visible = try read(name, fd: fd)
        guard visible?.identity == expected else { throw AtlasFolderPersistenceError.conflict }
        guard let current = try openDirectory(create: false) else { throw AtlasFolderPersistenceError.conflict }
        defer { close(current) }
        let opened = try info(fd), path = try info(current)
        guard opened.st_dev == path.st_dev, opened.st_ino == path.st_ino else {
            throw AtlasFolderPersistenceError.conflict
        }
        guard renameat(fd, temporary, fd, name) == 0, fsync(fd) == 0 else {
            throw AtlasFolderPersistenceError.unavailable
        }
    }
}

/// One entered operation, one coalesced save, and one newest load. Closing a
/// project revokes load delivery but lets an accepted personal-state write drain.
/// Unlike cancelable source I/O, a completed rename is never relabeled canceled.
@Observable @MainActor final class AtlasFolderPersistence {
    private enum Request: Sendable {
        case load(AtlasFolderPersistenceContext)
        case save(AtlasFolderPersistenceContext, UUID?, AtlasFolderNavigation.Snapshot)
        var context: AtlasFolderPersistenceContext {
            switch self { case .load(let c), .save(let c, _, _): return c }
        }
        var isLoad: Bool { if case .load = self { return true }; return false }
    }
    private struct Job: Sendable { let id, owner: UUID; var request: Request }
    private(set) var isLoading = false
    private(set) var isSaving = false
    private(set) var notice = ""
    private(set) var hasUnsavedChanges = false
    private(set) var canSave = false
    @ObservationIgnored private let worker = DispatchQueue(label: "dev.frankencode.atlas-views", qos: .utility)
    @ObservationIgnored private let makeStore: @Sendable () throws -> AtlasFolderViewStore
    @ObservationIgnored private var active: Job?
    @ObservationIgnored private var pending: [Job] = []
    @ObservationIgnored private var owner = UUID()
    @ObservationIgnored private var context: AtlasFolderPersistenceContext?
    @ObservationIgnored private var revision: UUID?
    @ObservationIgnored private var confirmed: AtlasFolderNavigation.Snapshot?
    @ObservationIgnored private var desired: AtlasFolderNavigation.Snapshot?
    @ObservationIgnored private var accepted: (@MainActor (AtlasFolderNavigation.Snapshot?) -> Void)?

    init(makeStore: @escaping @Sendable () throws -> AtlasFolderViewStore = { try .applicationSupport() }) {
        self.makeStore = makeStore
    }

    func matches(root: String, policy: UInt64) -> Bool {
        guard let context else { return false }
        return context.policy == policy && context.root.utf8.elementsEqual(root.utf8)
    }

    func open(root: String, policy: UInt64, accepted: @escaping @MainActor (AtlasFolderNavigation.Snapshot?) -> Void) {
        close()
        do { context = try AtlasFolderPersistenceContext(root: root, policy: policy) }
        catch { notice = AtlasFolderPersistenceError.invalidState.message; return }
        self.accepted = accepted
        isLoading = true
        notice = "Restoring saved atlas views…"
        enqueue(Job(id: UUID(), owner: owner, request: .load(context!)))
    }

    func save(_ snapshot: AtlasFolderNavigation.Snapshot) {
        guard canSave, let context else { return }
        desired = snapshot
        hasUnsavedChanges = snapshot != confirmed
        let writeEntered = active.map { $0.owner == owner && !$0.request.isLoad } ?? false
        guard hasUnsavedChanges || writeEntered else { return }
        let job = Job(id: UUID(), owner: owner, request: .save(context, revision, snapshot))
        if let index = pending.firstIndex(where: { !$0.request.isLoad && $0.owner == owner }) {
            pending[index] = job
        } else {
            enqueue(job)
        }
        isSaving = true
        notice = "Saving atlas views…"
    }

    func retrySave() {
        guard let desired else { return }
        save(desired)
    }

    /// Does not cancel or relabel already entered writes. A new root waits
    /// behind a pending saved view; only obsolete load requests are coalesced.
    func close() {
        owner = UUID()
        context = nil; revision = nil; confirmed = nil; desired = nil; accepted = nil
        pending.removeAll(where: { $0.request.isLoad })
        isLoading = false; isSaving = false; hasUnsavedChanges = false; canSave = false
    }

    private func enqueue(_ job: Job) {
        if active == nil { start(job); return }
        // A new context cannot submit saves before its load is accepted, so the
        // only queued pair is an old context's save followed by the newest load.
        guard pending.count < 2 else {
            if job.owner == owner { notice = AtlasFolderPersistenceError.busy.message }
            return
        }
        pending.append(job)
    }

    private func start(_ job: Job) {
        active = job
        let makeStore = self.makeStore
        worker.async { [self] in
            let result: Result<AtlasFolderPersistenceReceipt, AtlasFolderPersistenceError>
            do {
                let store = try makeStore()
                switch job.request {
                case .load(let context): result = .success(try store.load(context))
                case .save(let context, let expected, let snapshot):
                    result = .success(try store.save(snapshot, context: context, expecting: expected))
                }
            } catch { result = .failure((error as? AtlasFolderPersistenceError) ?? .unavailable) }
            DispatchQueue.main.async { [self] in finish(job, result: result) }
        }
    }

    private func finish(_ job: Job, result: Result<AtlasFolderPersistenceReceipt, AtlasFolderPersistenceError>) {
        guard active?.id == job.id else { return }
        active = nil
        if !job.request.isLoad, case .failure = result {
            // Also retire queued writes after their owner has closed. A newer
            // root's load remains queued and may still restore its own record.
            pending.removeAll(where: { $0.owner == job.owner && !$0.request.isLoad })
        }
        if case let .save(savedContext, expected, _) = job.request,
           case .success(let receipt) = result {
            for index in pending.indices {
                if case let .save(nextContext, nextExpected, snapshot) = pending[index].request,
                   nextContext == savedContext, nextExpected == expected {
                    pending[index].request = .save(nextContext, receipt.revision, snapshot)
                }
            }
        }
        if job.owner == owner, job.request.context == context {
            switch (job.request, result) {
            case (.load, .success(let receipt)):
                isLoading = false; canSave = true; revision = receipt.revision
                confirmed = receipt.snapshot; desired = receipt.snapshot
                notice = receipt.snapshot == nil ? "" : "Saved atlas landmarks and folder history restored."
                let publish = accepted; accepted = nil
                publish?(receipt.snapshot)
            case (.load, .failure(let error)):
                isLoading = false; canSave = false
                notice = error.message
            case (.save, .success(let receipt)):
                revision = receipt.revision; confirmed = receipt.snapshot
                hasUnsavedChanges = desired != confirmed
                notice = receipt.synchronizationConfirmed
                    ? "Atlas landmarks and folder history saved on this Mac."
                    : "The atlas view was written, but storage synchronization could not be confirmed."
            case (.save, .failure(let error)):
                hasUnsavedChanges = true
                if error == .conflict || error == .corrupt || error == .unsafeLocation { canSave = false }
                notice = error.message
                // Do not replay stale writes repeatedly after one refusal.
                pending.removeAll(where: { $0.owner == owner && !$0.request.isLoad })
            }
            isSaving = (active.map { $0.owner == owner && !$0.request.isLoad } ?? false)
                || pending.contains(where: { $0.owner == owner && !$0.request.isLoad })
        }
        // The accepted load callback may synchronously request a first save.
        // In that case it already owns the worker and we must not start twice.
        if active == nil, !pending.isEmpty { start(pending.removeFirst()) }
    }
}
