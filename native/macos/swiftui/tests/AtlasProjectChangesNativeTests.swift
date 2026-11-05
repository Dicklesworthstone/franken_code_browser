import Foundation
import CoreServices

private final class NativeChangeProbe: @unchecked Sendable {
    private let lock = NSLock()
    private var watching = false
    private var failed = false
    private var hints: AtlasProjectChangeHints = []
    private var releases = 0
    private var changes = 0
    var state: (watching: Bool, failed: Bool, hints: AtlasProjectChangeHints, releases: Int, changes: Int) {
        lock.lock(); defer { lock.unlock() }
        return (watching, failed, hints, releases, changes)
    }
    func receive(_ event: AtlasProjectChangeEvent) {
        lock.lock(); defer { lock.unlock() }
        switch event {
        case .watching: watching = true
        case .unavailable: failed = true
        case .changed(let incoming): hints.formUnion(incoming); changes += 1
        }
    }
    func released() { lock.lock(); releases += 1; lock.unlock() }
}
private final class NativeChangeLease {
    let probe: NativeChangeProbe
    init(_ probe: NativeChangeProbe) { self.probe = probe }
    deinit { probe.released() }
}

/// Real FSEvents delivery and stream-context lifetime on macOS. This process is
/// not the sandboxed application, so it cannot qualify security-scoped access.
@MainActor @main private enum AtlasProjectChangesNativeTests {
    static func until(_ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(10)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 10_000_000) }
        precondition(condition(), "Native change observation did not arrive within the test bound")
    }
    static func collect(_ flags: [FSEventStreamEventFlags]) -> AtlasProjectChangeHints {
        flags.withUnsafeBufferPointer { AtlasNativeProjectChangeDriver.collect(flags: $0.baseAddress!, count: $0.count) }
    }
    static func main() async throws {
        precondition(collect([FSEventStreamEventFlags(kFSEventStreamEventFlagHistoryDone)]).isEmpty)
        let flags = [FSEventStreamEventFlags(kFSEventStreamEventFlagRootChanged),
            FSEventStreamEventFlags(kFSEventStreamEventFlagKernelDropped),
            FSEventStreamEventFlags(kFSEventStreamEventFlagItemChangeOwner),
            FSEventStreamEventFlags(kFSEventStreamEventFlagUnmount)]
        precondition(collect(flags) == [.contentsChanged, .rescanRequired, .rootMoved, .permissionsChanged, .volumeChanged])
        let burst = Array(repeating: FSEventStreamEventFlags(kFSEventStreamEventFlagItemModified), count: 4_097)
        precondition(collect(burst) == [.contentsChanged, .rescanRequired])

        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("fcb-native-changes-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        let probe = NativeChangeProbe(), driver = AtlasNativeProjectChangeDriver(), generation = UUID()
        var lease: NativeChangeLease? = NativeChangeLease(probe)
        driver.watch(root: directory.path, generation: generation, accessLease: lease) { receivedGeneration, event in
            precondition(receivedGeneration == generation)
            probe.receive(event)
        }
        try await until { probe.state.watching || probe.state.failed }
        precondition(probe.state.watching && !probe.state.failed, "FSEvents must actually start for native qualification")
        lease = nil
        precondition(probe.state.releases == 0, "The native context must own the root lease after the caller releases it")

        // Same-process exports and checkpoints inside a root must also make
        // project membership stale. IgnoreSelf would hide this first write.
        try Data("kept source export\n".utf8).write(to: directory.appendingPathComponent("export.txt"), options: .withoutOverwriting)
        try await until { probe.state.hints.contains(.contentsChanged) }
        let beforeExternalWrite = probe.state.changes

        // A child process supplies a subsequent real external edit without
        // shell interpolation or source execution. Retain files for diagnosis.
        let touch = Process()
        touch.executableURL = URL(fileURLWithPath: "/usr/bin/touch")
        touch.arguments = [directory.appendingPathComponent("changed.txt").path]
        try touch.run(); touch.waitUntilExit()
        precondition(touch.terminationStatus == 0)
        try await until { probe.state.changes > beforeExternalWrite }

        let moved = directory.deletingLastPathComponent().appendingPathComponent(directory.lastPathComponent + "-moved", isDirectory: true)
        let move = Process()
        move.executableURL = URL(fileURLWithPath: "/bin/mv")
        move.arguments = [directory.path, moved.path]
        try move.run(); move.waitUntilExit()
        precondition(move.terminationStatus == 0)
        try await until { probe.state.hints.contains(.rootMoved) }
        driver.stop()
        try await until { probe.state.releases == 1 }
        precondition(probe.state.releases == 1, "Stream stop/invalidate/release must balance its context exactly once")
        print("Native FSEvents same-process write, external edit, root move and retained lease drain observed; retained fixture: \(moved.path)")
    }
}
