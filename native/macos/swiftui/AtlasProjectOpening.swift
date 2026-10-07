import Foundation

/// Project membership becomes usable independently of optional text-atlas work.
/// Tickets distinguish project replacement from a canceled/retried preview;
/// stopping enrichment never revokes an already opened reader's source origin.
/// Pure admission state: no filesystem, capture, cache, worker or native object.
struct AtlasProjectOpening {
    struct Ticket: Equatable, Sendable {
        let project: UUID
        fileprivate let attempt: UUID
    }
    private enum Phase { case idle, discovering, ready, preparing }
    private var phase = Phase.idle
    private var active: Ticket?
    private var hasOpenableFiles = false
    private(set) var projectID = UUID()
    private(set) var catalog: AtlasProjectCatalog?
    private(set) var hasAtlas = false

    var isDiscovering: Bool { phase == .discovering }
    var isPreparing: Bool { phase == .preparing }
    var canBrowse: Bool { catalog != nil }
    var canPrepare: Bool { phase == .ready && hasOpenableFiles && !hasAtlas }

    mutating func begin() -> Ticket {
        close()
        let ticket = Ticket(project: projectID, attempt: UUID())
        active = ticket; phase = .discovering
        return ticket
    }
    func accepts(_ ticket: Ticket) -> Bool { active == ticket && ticket.project == projectID }
    @discardableResult mutating func acceptCatalog(_ report: AtlasProjectCatalog, for ticket: Ticket) -> Bool {
        guard accepts(ticket), phase == .discovering else { return false }
        catalog = report
        hasOpenableFiles = report.entries.contains { $0.sourcePath != nil }
        active = nil; phase = .ready
        return true
    }
    mutating func beginPreviews() -> Ticket? {
        guard canPrepare else { return nil }
        let ticket = Ticket(project: projectID, attempt: UUID())
        active = ticket; phase = .preparing
        return ticket
    }
    @discardableResult mutating func finishPreviews(_ ticket: Ticket, available: Bool) -> Bool {
        guard accepts(ticket), phase == .preparing else { return false }
        hasAtlas = available
        active = nil; phase = .ready
        return true
    }
    @discardableResult mutating func fail(_ ticket: Ticket) -> Bool {
        guard accepts(ticket) else { return false }
        cancel()
        return true
    }
    mutating func cancel() {
        active = nil
        phase = catalog == nil ? .idle : .ready
    }
    mutating func close() {
        projectID = UUID()
        active = nil; phase = .idle; catalog = nil
        hasAtlas = false; hasOpenableFiles = false
    }
    /// Resolve the selected member of this exact catalog, never the same row
    /// number in a replacement project. Non-UTF-8 members remain known entries;
    /// callers must still require sourcePath before using the UTF-8 opener.
    func file(_ id: UInt64, in project: UUID) -> AtlasProjectCatalog.Entry? {
        guard project == projectID else { return nil }
        return catalog?.entries.first { $0.id == id }
    }
}

extension AtlasSearchContext {
    /// Content queries do not depend on an optional preview's geometry. Admit
    /// source results across preview publication ONLY under the same project,
    /// query and display filter; overlay proof must then be rebuilt against the
    /// current atlas. Existing matches() remains strict for row/frame actions.
    func matchesSourceRequest(_ other: Self) -> Bool {
        loadGeneration == other.loadGeneration
            && root.utf8.elementsEqual(other.root.utf8)
            && query.utf8.elementsEqual(other.query.utf8)
            && scope.utf8.elementsEqual(other.scope.utf8)
            && customExtensions.utf8.elementsEqual(other.customExtensions.utf8)
    }
}
