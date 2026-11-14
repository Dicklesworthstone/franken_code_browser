import Foundation

private final class ChangeDriver: AtlasProjectChangeDriver, @unchecked Sendable {
    private struct Watch {
        let root: String
        let generation: UUID
        let emit: @Sendable (UUID, AtlasProjectChangeEvent) -> Void
    }
    private let lock = NSLock()
    private var watches: [Watch] = []
    private var stops = 0
    private var creations = 0
    var counts: (created: Int, watched: Int, stopped: Int) {
        lock.lock(); defer { lock.unlock() }
        return (creations, watches.count, stops)
    }
    func make() -> ChangeDriver {
        lock.lock(); creations += 1; lock.unlock()
        return self
    }
    func watch(root: String, generation: UUID, accessLease: AnyObject?,
               emit: @escaping @Sendable (UUID, AtlasProjectChangeEvent) -> Void) {
        lock.lock(); defer { lock.unlock() }
        watches.append(Watch(root: root, generation: generation, emit: emit))
    }
    func stop() { lock.lock(); stops += 1; lock.unlock() }
    func emit(_ event: AtlasProjectChangeEvent, watch: Int? = nil) {
        lock.lock()
        let value = watches[watch ?? watches.count - 1]
        lock.unlock()
        value.emit(value.generation, event)
    }
}

@MainActor private final class ChangeDelay {
    private var continuation: CheckedContinuation<Void, Never>?
    private(set) var entries = 0
    func wait() async {
        precondition(continuation == nil, "More than one coalescing wake entered")
        entries += 1
        await withCheckedContinuation { continuation = $0 }
    }
    func release() {
        let value = continuation
        continuation = nil
        value?.resume()
    }
}

/// Pure model and actual bounded mailbox. OS event delivery, sandbox scope
/// activation and native teardown are qualified separately on macOS.
@MainActor @main private enum AtlasProjectChangesTests {
    private static var checks = 0
    private static func check(_ value: @autoclosure () -> Bool, _ label: String) {
        precondition(value(), label); checks += 1
    }
    private static func until(_ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(5)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 1_000_000) }
        check(condition(), "Coalesced main-actor delivery did not arrive within the test bound")
    }
    private static func ready(_ generation: UUID = UUID(), hints: AtlasProjectChangeHints = .contentsChanged) -> AtlasProjectChangeState {
        var state = AtlasProjectChangeState()
        state.bind(generation)
        state.receive(AtlasProjectChangeDelivery(generation: generation, monitoring: true, hints: hints))
        return state
    }

    static func main() async throws {
        mailboxCoalescesAndRevokes()
        discoveryRequiresCompleteCoverage()
        discoveryCannotEraseNewerHints()
        rootAndAvailabilityStayExplicit()
        try await modelOwnershipAndRetry()
        try await pendingHintPreventsAcknowledgment()
        try await independentWindows()
        liveRefreshConsumesOnlyAdmittedEpochs()
        try await liveRefreshCoalescesWithoutStarvation()
        try await liveRefreshDefersAndRetainsRefusedAdmission()
        try await liveRefreshFencesCloseAndRebinding()
        try await automaticReceiptsRejectQueuedAndNewerHints()
        try await manualRefreshRebindConsumesTheNewGeneration()
        print("Project-change admission, bounded delivery and refresh acknowledgment: \(checks) checks passed")
    }

    private static func mailboxCoalescesAndRevokes() {
        let mailbox = AtlasProjectChangeMailbox(), first = UUID(), replacement = UUID()
        check(!mailbox.offer(.watching, generation: first), "An unbound mailbox admits no event")
        mailbox.bind(first)
        check(mailbox.offer(.changed(.contentsChanged), generation: first), "First hint requests one wake")
        for _ in 0..<10_000 {
            check(!mailbox.offer(.changed(.contentsChanged), generation: first), "A burst cannot enqueue more wakes")
        }
        check(!mailbox.offer(.changed(.permissionsChanged), generation: first), "Kinds coalesce into one delivery")
        check(mailbox.hasPendingChanges(in: first), "Unconsumed hints remain visible to scan admission")
        let delivery = mailbox.take()
        check(delivery?.hints == [.contentsChanged, .permissionsChanged], "No event list is needed to retain relevant categories")
        check(mailbox.take() == nil && !mailbox.hasPendingChanges(in: first), "Taking drains pending state")

        check(mailbox.offer(.unavailable, generation: first), "A coverage gap wakes once")
        check(!mailbox.offer(.watching, generation: first), "Recovery shares that wake")
        let recovered = mailbox.take()
        check(recovered?.monitoring == true && recovered?.hints.contains(.rescanRequired) == true,
              "Recovery cannot hide an earlier coverage gap in the same batch")

        check(mailbox.offer(.changed(.rootMoved), generation: first), "Old stream queues a wake")
        mailbox.bind(replacement)
        check(!mailbox.offer(.changed(.rootMoved), generation: first), "Old callbacks cannot pollute a new root")
        check(!mailbox.offer(.watching, generation: replacement), "Rebinding reuses an already queued wake")
        let rebound = mailbox.take()
        check(rebound?.generation == replacement && rebound?.hints.isEmpty == true, "Queued wake drains only the new generation")
        check(mailbox.offer(.changed(.volumeChanged), generation: replacement), "Delivery can schedule again after drain")
        mailbox.bind(nil)
        check(mailbox.take() == nil, "Close revokes queued publication")
    }

    private static func discoveryRequiresCompleteCoverage() {
        let project = UUID()
        var state = ready()
        state.beginDiscovery(project: project)
        state.reportDiscovery(project: project, complete: false, more: true, omissions: 0, pendingChanges: false)
        check(!state.hints.isEmpty, "First or paused pages cannot acknowledge root changes")
        state.reportDiscovery(project: project, complete: true, more: false, omissions: 0, pendingChanges: false)
        check(state.hints.isEmpty, "A fully covered successful scan acknowledges previous hints")

        for (complete, omissions) in [(false, UInt64(0)), (false, UInt64(1)), (true, UInt64(1))] {
            state = ready(); state.beginDiscovery(project: project)
            state.reportDiscovery(project: project, complete: complete, more: false, omissions: omissions, pendingChanges: false)
            check(!state.hints.isEmpty, "Partial discovery, omissions and invalid completion preserve uncertainty")
        }
        state = ready(); state.beginDiscovery(project: project); state.cancelDiscovery(project: project)
        state.reportDiscovery(project: project, complete: true, more: false, omissions: 0, pendingChanges: false)
        check(!state.hints.isEmpty, "Canceled discovery cannot acknowledge hints through a late response")

        let generation = UUID()
        state = AtlasProjectChangeState(); state.bind(generation)
        state.beginDiscovery(project: project)
        state.receive(AtlasProjectChangeDelivery(generation: generation, monitoring: true, hints: .contentsChanged))
        state.reportDiscovery(project: project, complete: true, more: false, omissions: 0, pendingChanges: false)
        check(!state.hints.isEmpty, "A watcher that started mid-scan cannot establish full coverage")
    }

    private static func discoveryCannotEraseNewerHints() {
        let project = UUID(), replacement = UUID(), generation = UUID()
        var state = ready(generation)
        state.beginDiscovery(project: project)
        state.receive(AtlasProjectChangeDelivery(generation: generation, hints: .contentsChanged))
        state.reportDiscovery(project: project, complete: true, more: false, omissions: 0, pendingChanges: false)
        check(!state.hints.isEmpty, "Even another hint of the same kind invalidates scan coverage")
        state.beginDiscovery(project: project)
        state.reportDiscovery(project: project, complete: true, more: false, omissions: 0, pendingChanges: true)
        check(!state.hints.isEmpty, "A not-yet-delivered callback prevents a false clean state")
        state.beginDiscovery(project: replacement)
        state.reportDiscovery(project: project, complete: true, more: false, omissions: 0, pendingChanges: false)
        check(!state.hints.isEmpty, "An old browser cannot acknowledge the replacement scan")
        state.reportDiscovery(project: replacement, complete: true, more: false, omissions: 0, pendingChanges: false)
        check(state.hints.isEmpty, "The current scan can acknowledge unchanged hints")
        state.receive(AtlasProjectChangeDelivery(generation: UUID(), monitoring: false, hints: .rootMoved))
        check(state.hints.isEmpty && state.monitoring == .active, "A stale stream cannot revoke current coverage")
    }

    private static func rootAndAvailabilityStayExplicit() {
        let generation = UUID(), project = UUID()
        var state = ready(generation, hints: .rootMoved)
        state.beginDiscovery(project: project)
        state.reportDiscovery(project: project, complete: true, more: false, omissions: 0, pendingChanges: false)
        check(state.hints.contains(.rootMoved) && state.notice?.contains("Choose it again") == true,
              "A scan cannot retarget a watcher after a root move; folder selection is explicit")
        state.bind(UUID())
        check(state.hints.isEmpty && state.monitoring == .starting, "Explicit rebinding starts a distinct root lifetime")

        state = ready(generation)
        state.beginDiscovery(project: project)
        state.receive(AtlasProjectChangeDelivery(generation: generation, monitoring: false, hints: .rescanRequired))
        state.reportDiscovery(project: project, complete: true, more: false, omissions: 0, pendingChanges: false)
        check(state.monitoring == .unavailable && !state.hints.isEmpty && state.notice?.contains("unavailable") == true,
              "Failed monitoring never becomes a successful absence assertion")
        for hint: AtlasProjectChangeHints in [.rescanRequired, .volumeChanged, .permissionsChanged, .contentsChanged] {
            state = ready(hints: hint)
            check(state.notice != nil, "Each important event class has a visible recovery notice")
        }
    }

    private static func modelOwnershipAndRetry() async throws {
        let driver = ChangeDriver(), model = AtlasProjectChanges(makeDriver: { driver.make() })
        let grant = NSObject(), replacementGrant = NSObject()
        check(driver.counts.created == 0 && model.notice == nil, "Construction performs no native work")
        model.bind(root: "/repo/cafe\u{301}", accessLease: grant)
        model.bind(root: "/repo/cafe\u{301}", accessLease: grant)
        check(driver.counts.created == 1 && driver.counts.watched == 1, "Refresh reuses the current root stream and grant")
        driver.emit(.watching); try await until { model.monitoring == .active }
        driver.emit(.changed(.contentsChanged)); try await until { model.notice != nil }
        let project = UUID(); model.beginDiscovery(project: project)
        model.reportDiscovery(project: project, complete: true, more: false, omissions: 0)
        check(model.notice == nil, "Covered successful discovery acknowledges the notice")

        model.bind(root: "/repo/caf\u{e9}", accessLease: grant)
        check(driver.counts.watched == 2, "Canonical-equivalent strings do not alias different root bytes")
        model.bind(root: "/repo/caf\u{e9}", accessLease: replacementGrant)
        check(driver.counts.watched == 3, "Replacing the grant revokes the former native binding")
        driver.emit(.changed(.rootMoved), watch: 0)
        driver.emit(.unavailable)
        try await until { model.monitoring == .unavailable }
        check(!model.hints.contains(.rootMoved) && model.needsFolderSelection, "Old root events cannot enter a new grant's notice")
        model.bind(root: "/repo/caf\u{e9}", accessLease: replacementGrant)
        check(driver.counts.watched == 4 && model.hints.contains(.rescanRequired), "Retrying unavailable monitoring preserves the coverage gap")
        driver.emit(.watching); try await until { model.monitoring == .active }
        check(model.notice != nil, "A successful restart alone cannot acknowledge missed changes")
        model.beginDiscovery(project: project)
        model.reportDiscovery(project: project, complete: true, more: false, omissions: 0)
        check(model.notice == nil, "A later complete covered refresh resolves the restart gap")
        model.close()
        driver.emit(.changed(.contentsChanged))
        check(model.monitoring == .idle && model.notice == nil && driver.counts.stopped == 1, "Close revokes delivery and requests native drain")
        model.bind(root: "/next", accessLease: nil)
        check(driver.counts.created == 1 && driver.counts.watched == 5, "A reopened view reuses the one bounded native worker")
        model.close()
    }

    private static func pendingHintPreventsAcknowledgment() async throws {
        let driver = ChangeDriver(), model = AtlasProjectChanges(makeDriver: { driver })
        model.bind(root: "/repo", accessLease: nil)
        driver.emit(.watching); try await until { model.monitoring == .active }
        driver.emit(.changed(.contentsChanged)); try await until { model.notice != nil }
        let project = UUID(); model.beginDiscovery(project: project)
        // No suspension before completion: this event is still in the mailbox.
        driver.emit(.changed(.permissionsChanged))
        model.reportDiscovery(project: project, complete: true, more: false, omissions: 0)
        check(model.notice != nil, "Queued but unconsumed events prevent acknowledgment")
        try await until { model.hints.contains(.permissionsChanged) }
        model.close()
    }

    private static func independentWindows() async throws {
        let firstDriver = ChangeDriver(), secondDriver = ChangeDriver()
        let first = AtlasProjectChanges(makeDriver: { firstDriver }), second = AtlasProjectChanges(makeDriver: { secondDriver })
        first.bind(root: "/shared", accessLease: nil); second.bind(root: "/shared", accessLease: nil)
        firstDriver.emit(.changed(.contentsChanged)); secondDriver.emit(.changed(.volumeChanged))
        try await until { first.notice != nil && second.notice != nil }
        first.close()
        check(second.hints == .volumeChanged && secondDriver.counts.stopped == 0,
              "Closing one window cannot revoke another's stream, hints or grant")
        second.close()
    }

    private static func liveInput(_ generation: UUID, _ change: UUID = UUID(), enabled: Bool = true,
                                  watching: Bool = true, available: Bool = true) -> AtlasProjectLiveRefreshInput {
        AtlasProjectLiveRefreshInput(generation: generation, change: change,
            enabled: enabled, watching: watching, canRefresh: available)
    }

    private static func liveRefreshConsumesOnlyAdmittedEpochs() {
        let generation = UUID(), first = UUID(), newer = UUID()
        var state = AtlasProjectLiveRefreshState()
        let input = liveInput(generation, first)
        state.update(input)
        check(state.ready == input.ticket, "A watched dirty epoch can request reconciliation")
        state.didStart(input.ticket!)
        for _ in 0..<100 {
            state.update(input)
            check(state.ready == nil && state.pending == nil,
                  "Failed, partial or canceled admitted work cannot retry the same unchanged hint")
        }
        state.update(liveInput(generation, newer, available: false))
        check(state.pending?.change == newer && state.ready == nil, "A change during work remains pending without parallel admission")
        state.update(liveInput(generation, newer))
        check(state.ready?.change == newer, "Draining the prior work exposes the latest dirty epoch")
        state.didStart(state.ready!)
        state.update(liveInput(generation, newer, enabled: false))
        state.update(liveInput(generation, newer))
        check(state.ready == nil, "Toggling does not turn retained uncertainty into a fresh filesystem event")

        state.update(liveInput(generation, UUID(), watching: false))
        check(state.pending == nil, "Moved roots and unavailable monitoring cannot automatically rebind by path")
        let replacement = liveInput(UUID(), first)
        state.update(replacement)
        state.didStart(input.ticket!)
        check(state.ready == replacement.ticket, "An old watcher cannot consume the replacement root's epoch")
        state.didStart(replacement.ticket!)
        check(state.ready == nil, "A manual admitted refresh also consumes its current epoch")
    }

    private static func liveRefreshCoalescesWithoutStarvation() async throws {
        let delay = ChangeDelay(), refresh = AtlasProjectLiveRefresh(wait: { await delay.wait() })
        let generation = UUID()
        var latest = liveInput(generation), calls: [AtlasProjectChangeTicket] = []
        refresh.update(latest) { calls.append($0); return true }
        try await until { delay.entries == 1 }
        for _ in 0..<10_000 {
            latest = liveInput(generation)
            refresh.update(latest) { calls.append($0); return true }
        }
        check(delay.entries == 1 && calls.isEmpty, "An event storm neither creates timer tasks nor starts scans inline")
        delay.release()
        try await until { calls.count == 1 }
        check(calls[0] == latest.ticket, "The original wake dispatches the latest epoch instead of being postponed by every save")
        refresh.update(latest) { calls.append($0); return true }
        check(delay.entries == 1, "Retained partial hints cannot schedule another timer")
        refresh.close()
    }

    private static func liveRefreshDefersAndRetainsRefusedAdmission() async throws {
        let delay = ChangeDelay(), refresh = AtlasProjectLiveRefresh(wait: { await delay.wait() })
        let generation = UUID(), change = UUID()
        var accepted = false, calls = 0
        let perform: @MainActor (AtlasProjectChangeTicket) -> Bool = { _ in calls += 1; return accepted }
        let available = liveInput(generation, change)
        let busy = liveInput(generation, change, available: false)
        refresh.update(available, action: perform)
        try await until { delay.entries == 1 }
        refresh.update(busy, action: perform)
        delay.release()
        try await until { !refresh.hasScheduledWake }
        check(calls == 0 && refresh.notice?.contains("waiting") == true,
              "A workflow opened during coalescing prevents admission without dropping the epoch")
        refresh.update(available, action: perform)
        try await until { delay.entries == 2 }
        delay.release()
        try await until { calls == 1 }
        for _ in 0..<10 { refresh.update(available, action: perform) }
        check(delay.entries == 2, "A pre-admission refusal cannot immediately loop")
        accepted = true
        refresh.update(busy, action: perform)
        refresh.update(available, action: perform)
        try await until { delay.entries == 3 }
        delay.release()
        try await until { calls == 2 }
        refresh.update(available, action: perform)
        check(delay.entries == 3, "The later successful admission consumes the still-pending epoch")
        refresh.close()
    }

    private static func liveRefreshFencesCloseAndRebinding() async throws {
        for transition in ["close", "rebind", "disable"] {
            let delay = ChangeDelay(), refresh = AtlasProjectLiveRefresh(wait: { await delay.wait() })
            var calls: [AtlasProjectChangeTicket] = []
            let old = liveInput(UUID())
            let replacement = transition == "disable" ? old : liveInput(UUID())
            refresh.update(old) { calls.append($0); return true }
            try await until { delay.entries == 1 }
            if transition == "close" { refresh.close() }
            if transition == "disable" {
                var disabled = old; disabled.enabled = false
                refresh.update(disabled) { calls.append($0); return true }
            }
            refresh.update(replacement) { calls.append($0); return true }
            check(delay.entries == 1, "A canceled but entered wake retains its one slot across rebinding or re-enabling")
            delay.release()
            try await until { delay.entries == 2 }
            check(calls.isEmpty, "A revoked timer cannot dispatch the replacement or re-enabled project")
            delay.release()
            try await until { calls.count == 1 }
            check(calls[0] == replacement.ticket, "The admitted binding receives its own coalescing interval")
            var disabled = replacement; disabled.enabled = false; disabled.change = UUID()
            refresh.update(disabled) { calls.append($0); return true }
            check(delay.entries == 2 && refresh.notice == nil, "Turning off live refresh leaves no pending automatic work")
            refresh.close()
        }
    }

    private static func automaticReceiptsRejectQueuedAndNewerHints() async throws {
        let driver = ChangeDriver(), changes = AtlasProjectChanges(makeDriver: { driver })
        changes.bind(root: "/repo", accessLease: nil)
        driver.emit(.watching)
        driver.emit(.changed(.contentsChanged))
        try await until { changes.allowsAutomaticRefresh && changes.refreshTicket != nil }
        let first = changes.refreshTicket!
        check(changes.acceptsAutomaticRefresh(first), "A matching watched dirty epoch permits automatic reconciliation")
        driver.emit(.changed(.contentsChanged))
        check(!changes.acceptsAutomaticRefresh(first), "Undelivered hints fence scan publication before the main-actor wake")
        try await until { changes.refreshTicket != first }
        let newer = changes.refreshTicket!
        check(!changes.acceptsAutomaticRefresh(first) && changes.acceptsAutomaticRefresh(newer),
              "Repeated same-kind hints still revoke the older scan's receipt")
        driver.emit(.changed(.rootMoved))
        try await until { !changes.allowsAutomaticRefresh }
        check(!changes.acceptsAutomaticRefresh(newer), "Automatic refresh never follows a moved root")
        changes.bind(root: "/repo", accessLease: NSObject())
        driver.emit(.watching); driver.emit(.changed(.contentsChanged))
        try await until { changes.allowsAutomaticRefresh && changes.refreshTicket != nil }
        check(!changes.acceptsAutomaticRefresh(newer), "A replacement grant invalidates the old refresh receipt")
        changes.close()
        check(!changes.acceptsAutomaticRefresh(changes.refreshTicket ?? newer), "Closing revokes scan publication")
    }

    private static func manualRefreshRebindConsumesTheNewGeneration() async throws {
        let driver = ChangeDriver(), changes = AtlasProjectChanges(makeDriver: { driver })
        let delay = ChangeDelay(), refresh = AtlasProjectLiveRefresh(wait: { await delay.wait() })
        var calls = 0
        func input(available: Bool) -> AtlasProjectLiveRefreshInput {
            AtlasProjectLiveRefreshInput(generation: changes.generation, change: changes.refreshTicket?.change,
                enabled: true, watching: changes.allowsAutomaticRefresh, canRefresh: available)
        }
        changes.bind(root: "/repo", accessLease: nil)
        driver.emit(.unavailable)
        try await until { changes.monitoring == .unavailable }
        refresh.update(input(available: true)) { _ in calls += 1; return true }
        let oldGeneration = changes.generation
        // Same-root manual refresh restarts monitoring before SwiftUI can
        // publish a new live-refresh input. Its first discovery stays partial.
        changes.bind(root: "/repo", accessLease: nil)
        let ticket = changes.refreshTicket!, project = UUID()
        check(changes.generation != oldGeneration, "The manual retry owns a new watcher lifetime")
        refresh.didStart(ticket, input: input(available: false))
        changes.beginDiscovery(project: project)
        driver.emit(.watching)
        try await until { changes.allowsAutomaticRefresh }
        changes.reportDiscovery(project: project, complete: false, more: true, omissions: 0)
        refresh.update(input(available: true)) { _ in calls += 1; return true }
        check(!refresh.hasScheduledWake && calls == 0 && changes.refreshTicket == ticket,
              "A partial manual refresh cannot enqueue a duplicate automatic scan after rebind")
        driver.emit(.changed(.contentsChanged))
        try await until { changes.refreshTicket != ticket }
        refresh.update(input(available: true)) { _ in calls += 1; return true }
        try await until { delay.entries == 1 }
        delay.release()
        try await until { calls == 1 }
        refresh.close(); changes.close()
    }
}
