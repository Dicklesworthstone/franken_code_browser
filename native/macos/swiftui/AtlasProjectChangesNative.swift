import Foundation
import CoreServices

/// FSEvents is only the Apple host's change-hint adapter. Discovery, exclusions,
/// source identity and reconciliation remain with the existing Rust services.
/// Native setup/teardown runs on one utility queue; rapid root switches retain
/// at most the running request and one latest pending request.
final class AtlasNativeProjectChangeDriver: AtlasProjectChangeDriver, @unchecked Sendable {
    /// accessLease is retained only for lifetime. It is never invoked here.
    private final class Request: @unchecked Sendable {
        let root: String
        let generation: UUID
        let accessLease: AnyObject?
        let emit: @Sendable (UUID, AtlasProjectChangeEvent) -> Void
        init(root: String, generation: UUID, accessLease: AnyObject?,
             emit: @escaping @Sendable (UUID, AtlasProjectChangeEvent) -> Void) {
            self.root = root; self.generation = generation
            self.accessLease = accessLease; self.emit = emit
        }
        func send(_ event: AtlasProjectChangeEvent) { emit(generation, event) }
    }

    private let queue = DispatchQueue(label: "dev.frankencode.project-changes", qos: .utility)
    private let lock = NSLock()
    private var requested: Request?
    private var version = UUID()
    private var scheduled = false
    // Only the serial native queue may access an active stream. A stream is
    // assigned here only after Start succeeds, so retirement can always Stop.
    private var active: FSEventStreamRef?

    func watch(root: String, generation: UUID, accessLease: AnyObject?,
               emit: @escaping @Sendable (UUID, AtlasProjectChangeEvent) -> Void) {
        let request = Request(root: root, generation: generation, accessLease: accessLease, emit: emit)
        submit(request)
    }
    func stop() { submit(nil) }

    private func submit(_ request: Request?) {
        lock.lock()
        requested = request; version = UUID()
        let enqueue = !scheduled
        scheduled = true
        lock.unlock()
        if enqueue { queue.async { self.drain() } }
    }

    private func current(_ version: UUID) -> Bool {
        lock.lock(); defer { lock.unlock() }
        return self.version == version
    }

    private func drain() {
        while true {
            lock.lock()
            let request = requested, requestVersion = version
            lock.unlock()
            retire()
            if let request, current(requestVersion) { start(request, version: requestVersion) }
            lock.lock()
            if version == requestVersion {
                // The active stream's context owns its request/grant. Do not
                // retain a second stopped grant in the configuration slot.
                requested = nil
                scheduled = false
                lock.unlock()
                return
            }
            lock.unlock()
        }
    }

    private func retire() {
        guard let stream = active else { return }
        active = nil
        FSEventStreamStop(stream)
        // Invalidate unschedules the dispatch queue; setting that queue to nil
        // first would violate FSEvents' required invalidation lifecycle.
        FSEventStreamInvalidate(stream)
        FSEventStreamRelease(stream)
    }

    private func start(_ request: Request, version: UUID) {
        guard request.root.hasPrefix("/"), request.root.utf8.count <= 16_384,
              !request.root.utf8.contains(0) else {
            if current(version) { request.send(.unavailable) }
            return
        }
        // FSEvents copies this context and retains its info through these
        // callbacks. Request stays alive across Create even if it fails; no
        // manual passRetained ownership is guessed on the failure path.
        var context = FSEventStreamContext(version: 0,
            info: Unmanaged.passUnretained(request).toOpaque(),
            retain: { pointer in
                guard let pointer else { return nil }
                _ = Unmanaged<Request>.fromOpaque(pointer).retain()
                return pointer
            }, release: { pointer in
                guard let pointer else { return }
                Unmanaged<Request>.fromOpaque(pointer).release()
            }, copyDescription: nil)
        // Explicit exports/checkpoints may be inside the watched project. Do
        // not IgnoreSelf: those app-originated writes also change membership.
        // This observer never writes or rebuilds anything in response to hints.
        let flags = FSEventStreamCreateFlags(kFSEventStreamCreateFlagWatchRoot
            | kFSEventStreamCreateFlagFileEvents)
        let paths = [request.root] as CFArray
        guard let stream = withExtendedLifetime(request, {
            FSEventStreamCreate(kCFAllocatorDefault, Self.callback, &context, paths,
                FSEventStreamEventId(kFSEventStreamEventIdSinceNow), 0.35, flags)
        }) else {
            if current(version) { request.send(.unavailable) }
            return
        }
        FSEventStreamSetDispatchQueue(stream, queue)
        guard FSEventStreamStart(stream) else {
            // A stream that did not Start must not be passed to Stop.
            FSEventStreamInvalidate(stream)
            FSEventStreamRelease(stream)
            if current(version) { request.send(.unavailable) }
            return
        }
        active = stream
        if current(version) { request.send(.watching) }
        // A superseded start is retired in drain before the latest request is
        // started. Its old-generation callbacks cannot publish into the UI.
    }

    private static let callback: FSEventStreamCallback = { _, info, count, _, flags, _ in
        guard let info, count > 0 else { return }
        let request = Unmanaged<Request>.fromOpaque(info).takeUnretainedValue()
        let hints = AtlasNativeProjectChangeDriver.collect(flags: flags, count: count)
        if count > AtlasNativeProjectChangeDriver.maximumEvents {
            // The unread tail could include a root-change flag. Require a new
            // stream on explicit refresh instead of claiming continued coverage
            // for a possibly replaced root from a deliberately truncated batch.
            request.send(.unavailable)
        }
        if !hints.isEmpty { request.send(.changed(hints)) }
    }

    private static let maximumEvents = 4_096
    static func collect(flags: UnsafePointer<FSEventStreamEventFlags>, count: Int) -> AtlasProjectChangeHints {
        guard count > 0 else { return [] }
        var hints: AtlasProjectChangeHints = []
        // A large native batch is itself a reason to rescan. Reading a bounded
        // prefix keeps this host callback O(1) in retained space and bounded in
        // work; no paths are decoded and dropped detail never means no change.
        if count > maximumEvents { hints.insert(.rescanRequired) }
        let dropped = FSEventStreamEventFlags(kFSEventStreamEventFlagMustScanSubDirs
            | kFSEventStreamEventFlagUserDropped | kFSEventStreamEventFlagKernelDropped
            | kFSEventStreamEventFlagEventIdsWrapped)
        let volume = FSEventStreamEventFlags(kFSEventStreamEventFlagMount | kFSEventStreamEventFlagUnmount)
        let metadata = FSEventStreamEventFlags(kFSEventStreamEventFlagItemInodeMetaMod
            | kFSEventStreamEventFlagItemChangeOwner)
        for index in 0..<min(count, maximumEvents) {
            let event = flags[index]
            if event == FSEventStreamEventFlags(kFSEventStreamEventFlagHistoryDone) { continue }
            hints.insert(.contentsChanged)
            if event & dropped != 0 { hints.insert(.rescanRequired) }
            if event & FSEventStreamEventFlags(kFSEventStreamEventFlagRootChanged) != 0 { hints.insert(.rootMoved) }
            if event & volume != 0 { hints.insert(.volumeChanged) }
            if event & metadata != 0 { hints.insert(.permissionsChanged) }
        }
        return hints
    }
}
