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
    private var progressiveReady = false
    private(set) var projectID = UUID()
    private(set) var catalog: AtlasProjectCatalog?
    private(set) var hasAtlas = false

    var isDiscovering: Bool { phase == .discovering }
    var isPreparing: Bool { phase == .preparing }
    var canBrowse: Bool { progressiveReady || catalog != nil }
    var canPrepare: Bool { phase == .ready && !hasAtlas && (catalog == nil ? progressiveReady : hasOpenableFiles) }

    mutating func begin() -> Ticket {
        close()
        let ticket = Ticket(project: projectID, attempt: UUID())
        active = ticket; phase = .discovering
        return ticket
    }
    func accepts(_ ticket: Ticket) -> Bool { active == ticket && ticket.project == projectID }
    /// First-page discovery makes browsing/search usable even when that batch
    /// contains only directories. Optional atlas membership is collected later.
    @discardableResult mutating func acceptProgressive(for ticket: Ticket) -> Bool {
        guard accepts(ticket), phase == .discovering else { return false }
        progressiveReady = true; active = nil; phase = .ready
        return true
    }
    /// A consumed initial page may be recovered only after the opening attempt
    /// failed. The host also binds the current browser instance and raw root.
    @discardableResult mutating func recoverProgressive(in project: UUID) -> Bool {
        guard project == projectID, phase == .idle, !canBrowse else { return false }
        progressiveReady = true; phase = .ready
        return true
    }
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
    @discardableResult mutating func acceptPreviewCatalog(_ report: AtlasProjectCatalog, for ticket: Ticket) -> Bool {
        guard accepts(ticket), phase == .preparing, progressiveReady, !hasAtlas else { return false }
        catalog = report; hasOpenableFiles = report.entries.contains { $0.sourcePath != nil }
        return true
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
        phase = canBrowse ? .ready : .idle
    }
    mutating func close() {
        projectID = UUID()
        active = nil; phase = .idle; catalog = nil
        hasAtlas = false; hasOpenableFiles = false; progressiveReady = false
    }
    /// Resolve the selected member of this exact catalog, never the same row
    /// number in a replacement project. Non-UTF-8 members remain known entries;
    /// callers must still require sourcePath before using the UTF-8 opener.
    func file(_ id: UInt64, in project: UUID) -> AtlasProjectCatalog.Entry? {
        guard project == projectID else { return nil }
        return catalog?.entries.first { $0.id == id }
    }
}

/// Resolve a window's initial project exactly once. Later picker/recents changes
/// and refreshes own their root independently of the process's launch arguments.
/// This is lexical admission only: existence, file kind and grants are checked by
/// the asynchronous project opener, never by filesystem I/O on the main actor.
struct AtlasProjectStartup {
    enum Selection {
        case open(root: String, remember: Bool)
        case chooseProject
        case invalidRoot
    }

    private(set) var consumed = false

    /// Named -fcb.root overrides are the native_launch.rs protocol and remain
    /// per-launch (NSArgumentDomain), without changing persistent preferences.
    /// The legacy first positional project retains its existing remember policy.
    mutating func consume(arguments: [String], restoredRoot: String) -> Selection? {
        guard !consumed else { return nil }
        consumed = true
        guard arguments.count <= 64 else { return .invalidRoot }
        var total = 0
        for argument in arguments.dropFirst() {
            let count = argument.utf8.count
            guard count <= 32_768, count <= 65_536 - total else { return .invalidRoot }
            total += count
        }

        var namedRoot: String?
        var index = 1
        while index < arguments.count {
            let argument = arguments[index]; index += 1
            if argument == "-fcb.root" {
                guard namedRoot == nil, index < arguments.count else { return .invalidRoot }
                let value = arguments[index]; index += 1
                guard Self.validRoot(value) else { return .invalidRoot }
                namedRoot = value
            } else if argument == "-fcb.file-hex" {
                // Filename decoding and capture authority belong exclusively to
                // AtlasStartupFileRequest. Its value is never a project path.
                guard index < arguments.count else { return .invalidRoot }
                index += 1
            }
        }
        if let namedRoot { return .open(root: namedRoot, remember: false) }
        if let first = arguments.dropFirst().first, !first.hasPrefix("-") {
            guard Self.validRoot(first) else { return .invalidRoot }
            return .open(root: first, remember: true)
        }
        // Unknown options and their values are not positional project roots.
        guard !restoredRoot.isEmpty else { return .chooseProject }
        guard Self.validRoot(restoredRoot) else { return .invalidRoot }
        return .open(root: restoredRoot, remember: false)
    }

    private static func validRoot(_ root: String) -> Bool {
        root.hasPrefix("/") && root.utf8.count <= 16_384 && !root.utf8.contains(0)
    }
}

extension AtlasSearchContext {
    /// Content queries do not depend on an optional preview's geometry. Admit
    /// source results across preview publication ONLY under the same project,
    /// query and display filter; overlay proof must then be rebuilt against the
    /// current atlas. Existing matches() remains strict for row/frame actions.
    func matchesSourceRequest(_ other: Self) -> Bool {
        loadGeneration == other.loadGeneration
            && mode == other.mode && nameMode == other.nameMode
            && root.utf8.elementsEqual(other.root.utf8)
            && query.utf8.elementsEqual(other.query.utf8)
            && scope.utf8.elementsEqual(other.scope.utf8)
            && customExtensions.utf8.elementsEqual(other.customExtensions.utf8)
            && pathKind.utf8.elementsEqual(other.pathKind.utf8)
            && path.utf8.elementsEqual(other.path.utf8)
    }
}
