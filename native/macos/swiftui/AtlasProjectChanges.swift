import Foundation
import Observation

/// Notifications are bounded hints about a root, never source contents or proof
/// that a member was deleted. The existing project browser owns discovery.
struct AtlasProjectChangeHints: OptionSet, Sendable {
    let rawValue: UInt8
    static let contentsChanged = Self(rawValue: 1 << 0)
    static let rescanRequired = Self(rawValue: 1 << 1)
    static let rootMoved = Self(rawValue: 1 << 2)
    static let volumeChanged = Self(rawValue: 1 << 3)
    static let permissionsChanged = Self(rawValue: 1 << 4)
}

enum AtlasProjectChangeEvent: Sendable {
    case watching
    case unavailable
    case changed(AtlasProjectChangeHints)
}

struct AtlasProjectChangeDelivery: Sendable {
    let generation: UUID
    var monitoring: Bool? = nil
    var hints: AtlasProjectChangeHints = []
}

/// One observed dirty epoch within one root/grant watcher lifetime. Equal hint
/// kinds can still carry a new epoch; retained partial coverage is not a new
/// filesystem event and must not create a refresh loop.
struct AtlasProjectChangeTicket: Equatable, Sendable {
    let generation: UUID
    let change: UUID
}

/// One delivery and at most one queued main-actor wake, regardless of event
/// volume. No filenames, event arrays, or unbounded Task-per-event backlog.
final class AtlasProjectChangeMailbox: @unchecked Sendable {
    private let lock = NSLock()
    private var generation: UUID?
    private var pending: AtlasProjectChangeDelivery?
    private var wakeScheduled = false

    func bind(_ generation: UUID?) {
        lock.lock(); defer { lock.unlock() }
        self.generation = generation
        pending = nil
        // A wake already queued for the old binding will drain the latest one.
        // Resetting this flag here would permit an unbounded rebind backlog.
    }

    /// True only when the caller must schedule the single consumer wake.
    func offer(_ event: AtlasProjectChangeEvent, generation: UUID) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard self.generation == generation else { return false }
        var delivery = pending ?? AtlasProjectChangeDelivery(generation: generation)
        switch event {
        case .watching: delivery.monitoring = true
        case .unavailable:
            delivery.monitoring = false
            // A later successful start must not erase an intervening gap.
            delivery.hints.insert(.rescanRequired)
        case .changed(let hints): delivery.hints.formUnion(hints)
        }
        pending = delivery
        guard !wakeScheduled else { return false }
        wakeScheduled = true
        return true
    }

    /// Only the scheduled consumer may take a delivery.
    func take() -> AtlasProjectChangeDelivery? {
        lock.lock(); defer { lock.unlock() }
        let delivery = pending
        pending = nil; wakeScheduled = false
        return delivery
    }

    func hasPendingChanges(in generation: UUID) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard let pending, pending.generation == generation else { return false }
        return !pending.hints.isEmpty || pending.monitoring == false
    }
}

/// Pure admission state. A completed scan can acknowledge previously observed
/// hints only when the same stream covered that entire scan without new hints.
/// This does not assert current byte identity or reconcile retained captures.
struct AtlasProjectChangeState {
    enum Monitoring: Equatable { case idle, starting, active, unavailable }
    private struct Discovery {
        let project: UUID
        let generation: UUID
        let changeStamp: UUID
        let monitoredAtStart: Bool
    }
    private(set) var generation: UUID?
    private(set) var monitoring = Monitoring.idle
    private(set) var hints: AtlasProjectChangeHints = []
    private var changeStamp = UUID()
    private var discovery: Discovery?

    var refreshTicket: AtlasProjectChangeTicket? {
        guard let generation, !hints.isEmpty else { return nil }
        return AtlasProjectChangeTicket(generation: generation, change: changeStamp)
    }

    mutating func bind(_ generation: UUID, preserving previousHints: AtlasProjectChangeHints = []) {
        self = Self()
        self.generation = generation
        hints = previousHints
        monitoring = .starting
    }

    mutating func receive(_ delivery: AtlasProjectChangeDelivery) {
        guard generation == delivery.generation else { return }
        if let available = delivery.monitoring { monitoring = available ? .active : .unavailable }
        if !delivery.hints.isEmpty {
            hints.formUnion(delivery.hints)
            changeStamp = UUID()
        }
    }

    mutating func beginDiscovery(project: UUID) {
        guard let generation else { return }
        discovery = Discovery(project: project, generation: generation,
            changeStamp: changeStamp, monitoredAtStart: monitoring == .active)
    }

    mutating func reportDiscovery(project: UUID, complete: Bool, more: Bool,
                                  omissions: UInt64, pendingChanges: Bool) {
        guard let discovery, discovery.project == project,
              discovery.generation == generation else { return }
        // Paused/progressive pages are not a complete membership check.
        guard !more else { return }
        self.discovery = nil
        guard complete, omissions == 0, discovery.monitoredAtStart,
              monitoring == .active, discovery.changeStamp == changeStamp,
              !pendingChanges, !hints.contains(.rootMoved) else { return }
        hints = []
    }

    mutating func cancelDiscovery(project: UUID) {
        if discovery?.project == project { discovery = nil }
    }

    var notice: String? {
        if hints.contains(.rootMoved) {
            return "The project folder moved or changed. Choose it again to restore change monitoring."
        }
        if monitoring == .unavailable {
            return "Live change monitoring is unavailable. Refresh the project to check its files, or choose the folder again."
        }
        if hints.contains(.rescanRequired) {
            return "Some filesystem changes could not be tracked. Refresh the project and finish discovery to check current files."
        }
        if hints.contains(.volumeChanged) {
            return "A volume change may affect this project. Refresh the project to check availability."
        }
        if hints.contains(.permissionsChanged) {
            return "File metadata or permissions changed. Refresh the project to check accessible files."
        }
        if hints.contains(.contentsChanged) {
            return "Project files changed. Refresh the project to check current files."
        }
        return nil
    }
}

/// Native setup/stop must be asynchronous and bounded. The grant is a retained
/// lifetime token, not permission to resolve another root or follow a rename.
protocol AtlasProjectChangeDriver: AnyObject, Sendable {
    func watch(root: String, generation: UUID, accessLease: AnyObject?,
               emit: @escaping @Sendable (UUID, AtlasProjectChangeEvent) -> Void)
    func stop()
}

@Observable @MainActor final class AtlasProjectChanges {
    private var state = AtlasProjectChangeState()
    @ObservationIgnored private let makeDriver: @Sendable () -> any AtlasProjectChangeDriver
    @ObservationIgnored private var driver: (any AtlasProjectChangeDriver)?
    @ObservationIgnored private let mailbox = AtlasProjectChangeMailbox()
    @ObservationIgnored private var root = ""
    @ObservationIgnored private var accessIdentity: ObjectIdentifier?

    init(makeDriver: @escaping @Sendable () -> any AtlasProjectChangeDriver) {
        self.makeDriver = makeDriver
    }
    deinit { driver?.stop() }

    var notice: String? { state.notice }
    var hints: AtlasProjectChangeHints { state.hints }
    var monitoring: AtlasProjectChangeState.Monitoring { state.monitoring }
    var needsFolderSelection: Bool { state.hints.contains(.rootMoved) || state.monitoring == .unavailable }
    var generation: UUID? { state.generation }
    var refreshTicket: AtlasProjectChangeTicket? { state.refreshTicket }
    var allowsAutomaticRefresh: Bool { state.monitoring == .active && !state.hints.contains(.rootMoved) }

    /// Recheck both delivered and queued hints at admission and publication.
    /// A newer event cannot lend an older automatic scan deletion authority.
    func acceptsAutomaticRefresh(_ ticket: AtlasProjectChangeTicket) -> Bool {
        allowsAutomaticRefresh && state.refreshTicket == ticket
            && !mailbox.hasPendingChanges(in: ticket.generation)
    }

    func bind(root: String, accessLease: AnyObject?) {
        guard root.hasPrefix("/"), root.utf8.count <= 16_384, !root.utf8.contains(0) else {
            close(); return
        }
        let identity = accessLease.map(ObjectIdentifier.init)
        if self.root.utf8.elementsEqual(root.utf8), identity == accessIdentity,
           state.monitoring == .active || state.monitoring == .starting { return }
        let previousHints: AtlasProjectChangeHints = self.root.utf8.elementsEqual(root.utf8)
            && identity == accessIdentity ? state.hints : []
        self.root = root; accessIdentity = identity
        let generation = UUID()
        state.bind(generation, preserving: previousHints); mailbox.bind(generation)
        if driver == nil { driver = makeDriver() }
        let mailbox = mailbox
        driver?.watch(root: root, generation: generation, accessLease: accessLease) { [weak self] generation, event in
            guard mailbox.offer(event, generation: generation) else { return }
            Task { @MainActor [weak self] in
                guard let delivery = mailbox.take() else { return }
                self?.state.receive(delivery)
            }
        }
    }

    func beginDiscovery(project: UUID) { state.beginDiscovery(project: project) }
    func reportDiscovery(project: UUID, complete: Bool, more: Bool, omissions: UInt64) {
        let pending = state.generation.map { mailbox.hasPendingChanges(in: $0) } ?? false
        state.reportDiscovery(project: project, complete: complete, more: more,
            omissions: omissions, pendingChanges: pending)
    }
    func cancelDiscovery(project: UUID) { state.cancelDiscovery(project: project) }

    func close() {
        mailbox.bind(nil)
        state = AtlasProjectChangeState()
        root = ""; accessIdentity = nil
        // Keep the reusable driver: close/rebind coalesces into the same worker
        // instead of allocating one native queue per rapid folder selection.
        driver?.stop()
    }
}

struct AtlasProjectLiveRefreshInput: Equatable {
    var generation: UUID?
    var change: UUID?
    var enabled: Bool
    var watching: Bool
    var canRefresh: Bool

    static let idle = Self(generation: nil, change: nil, enabled: false, watching: false, canRefresh: false)

    var ticket: AtlasProjectChangeTicket? {
        guard let generation, let change else { return nil }
        return AtlasProjectChangeTicket(generation: generation, change: change)
    }
    func admits(_ ticket: AtlasProjectChangeTicket) -> Bool {
        enabled && watching && canRefresh && self.ticket == ticket
    }
}

/// No timers, I/O, or event list. An admitted refresh consumes one dirty epoch;
/// a partial/failed scan cannot repeat it. New events while busy retain only the
/// latest epoch. A refusal before admission waits for a changed availability
/// input, instead of losing the epoch or retrying in a tight loop.
struct AtlasProjectLiveRefreshState {
    private(set) var input = AtlasProjectLiveRefreshInput.idle
    private var attempted: AtlasProjectChangeTicket?
    private var refusedInput: AtlasProjectLiveRefreshInput?

    mutating func update(_ input: AtlasProjectLiveRefreshInput) {
        if self.input.generation != input.generation { attempted = nil }
        if self.input != input { refusedInput = nil }
        self.input = input
    }
    var pending: AtlasProjectChangeTicket? {
        guard input.enabled, input.watching, let ticket = input.ticket,
              ticket != attempted else { return nil }
        return ticket
    }
    var ready: AtlasProjectChangeTicket? {
        guard input.canRefresh, refusedInput != input else { return nil }
        return pending
    }
    var waitingForAdmission: Bool { pending != nil && refusedInput == input }
    mutating func didStart(_ ticket: AtlasProjectChangeTicket) {
        guard ticket.generation == input.generation else { return }
        attempted = ticket
        refusedInput = nil
    }
    mutating func refused() { refusedInput = input }
}

/// One short coalescing wake per window, not one task per native event. Later
/// hints replace the pending epoch without restarting the timer, so sustained
/// edits cannot indefinitely postpone an otherwise available refresh. The
/// existing transaction owns all native work, cancellation and entered drains.
@Observable @MainActor final class AtlasProjectLiveRefresh {
    private var state = AtlasProjectLiveRefreshState()
    @ObservationIgnored private let wait: @Sendable () async -> Void
    @ObservationIgnored private var action: (@MainActor (AtlasProjectChangeTicket) -> Bool)?
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var wake: UUID?

    init(wait: @escaping @Sendable () async -> Void = {
        try? await Task.sleep(nanoseconds: 750_000_000)
    }) { self.wait = wait }

    var hasScheduledWake: Bool { wake != nil }

    var notice: String? {
        guard state.input.enabled else { return nil }
        guard state.input.watching else { return "Live refresh is waiting for change monitoring." }
        if state.waitingForAdmission {
            return "Live refresh is waiting for project availability. Use Refresh project to retry explicitly."
        }
        if state.pending != nil {
            return state.input.canRefresh
                ? "Live refresh is scheduled for the latest observed changes."
                : "Live refresh is waiting for current work or interaction to finish."
        }
        if state.input.change != nil {
            return "These changes have had one refresh attempt. Use Refresh project to retry or continue discovery."
        }
        return nil
    }

    func update(_ input: AtlasProjectLiveRefreshInput,
                action: @escaping @MainActor (AtlasProjectChangeTicket) -> Bool) {
        receive(input)
        self.action = action
        schedule()
    }

    /// Manual refreshes also cover their current dirty epoch; do not enqueue a
    /// duplicate automatic scan merely because the retained page is partial.
    func didStart(_ ticket: AtlasProjectChangeTicket?, input: AtlasProjectLiveRefreshInput) {
        // A manual refresh can restart unavailable monitoring synchronously.
        // Consume that new generation before SwiftUI's next observation turn.
        receive(input)
        if let ticket { state.didStart(ticket) }
    }

    func close() {
        state = AtlasProjectLiveRefreshState()
        action = nil
        // The single slot remains occupied until its canceled wake drains.
        // Rapid disappear/reappear cannot accumulate pending timer tasks.
        task?.cancel()
    }

    private func schedule() {
        guard wake == nil, state.ready != nil, action != nil else { return }
        let ticket = UUID(), wait = wait
        wake = ticket
        task = Task { [weak self] in
            await wait()
            self?.deliver(ticket)
        }
    }

    private func receive(_ input: AtlasProjectLiveRefreshInput) {
        if state.input.generation != input.generation || !input.enabled || !input.watching {
            task?.cancel()
        }
        state.update(input)
    }

    private func deliver(_ ticket: UUID) {
        guard wake == ticket else { return }
        wake = nil; task = nil
        // A close followed by a new binding schedules a fresh coalescing
        // interval. A canceled old wake cannot dispatch the replacement root.
        if Task.isCancelled { schedule(); return }
        guard let change = state.ready, let action else { return }
        if action(change) { state.didStart(change) }
        else { state.refused() }
    }
}
