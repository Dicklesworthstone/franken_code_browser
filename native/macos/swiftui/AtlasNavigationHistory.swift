import Foundation

/// Bounded, transactional navigation shared by source/Markdown readers and the
/// atlas camera. Proposing a move does not mutate history: failed/canceled I/O leaves
/// both directions intact. Only a successfully published destination is recorded.
/// Owners store source/layout-relative anchors, never handles, pages or source payload.
struct AtlasNavigationHistory<Position: Equatable> {
    enum Direction { case visit, back, forward }
    struct Move {
        let target: Position
        fileprivate let revision: UUID
        fileprivate let direction: Direction
    }

    private(set) var current: Position?
    private var previous: [Position] = []
    private var following: [Position] = []
    private var revision = UUID()
    let capacity: Int
    var retainedCount: Int { previous.count + following.count }
    var canBack: Bool { !previous.isEmpty }
    var canForward: Bool { !following.isEmpty }

    init(capacity: Int = 128) { self.capacity = min(128, max(0, capacity)) }

    func visit(_ position: Position) -> Move {
        Move(target: position, revision: revision, direction: .visit)
    }
    func back() -> Move? {
        previous.last.map { Move(target: $0, revision: revision, direction: .back) }
    }
    func forward() -> Move? {
        following.last.map { Move(target: $0, revision: revision, direction: .forward) }
    }

    /// Refresh an existing location's presentation state without adding a
    /// navigation step or discarding Forward. The camera uses this only at an
    /// explicit navigation action, after direct pan/zoom has changed its view.
    mutating func replaceCurrent(_ position: Position) {
        guard current != position else { return }
        current = position
        revision = UUID()
    }

    /// `resolved` can differ from the request: a search/symbol activation must be
    /// reduced to its accepted plain source window before entering history. It
    /// cannot resurrect an expired query's exact-hit/clipboard authority later.
    @discardableResult mutating func commit(_ move: Move, resolved: Position) -> Bool {
        guard move.revision == revision else { return false }
        switch move.direction {
        case .visit:
            if let current, current != resolved {
                previous.append(current)
                following.removeAll(keepingCapacity: true)
            }
        case .back:
            guard let current, previous.last == move.target else { return false }
            previous.removeLast()
            following.append(current)
        case .forward:
            guard let current, following.last == move.target else { return false }
            following.removeLast()
            previous.append(current)
        }
        current = resolved
        // Back/Forward move an existing entry, so only a visit can increase the
        // total. Keep one shared bound for both directions, excluding current.
        let excess = retainedCount - capacity
        if excess > 0 { previous.removeFirst(excess) }
        revision = UUID()
        return true
    }

    /// Capture or document-generation replacement retires every old proposal,
    /// even if the new capture happens to have identical-looking coordinates.
    mutating func reset(to position: Position? = nil) {
        current = position
        previous.removeAll(keepingCapacity: true)
        following.removeAll(keepingCapacity: true)
        revision = UUID()
    }
}
