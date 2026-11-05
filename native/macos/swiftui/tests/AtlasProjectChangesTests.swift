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
}
