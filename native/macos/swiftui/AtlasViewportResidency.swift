import Foundation
import CoreGraphics
import Observation
import Dispatch

/// One process-wide native signal, shared by all atlas windows. Recovery uses
/// monotonic deadlines and never grants a new per-window memory allowance.
/// Tests inject the same signal path without subscribing to the operating system.
@MainActor final class AtlasViewportMemoryPressure {
    enum Phase: Equatable, Sendable { case normal, constrained, critical, recovery }
    struct State: Equatable, Sendable {
        let phase: Phase
        let selectedOnly: Bool
        static let normal = State(phase: .normal, selectedOnly: false)
    }
    static let shared = AtlasViewportMemoryPressure(observingSystem: true)
    static let quietInterval: UInt64 = 5_000_000_000
    private(set) var state = State.normal
    private var recoveryDeadline: UInt64?
    private var source: DispatchSourceMemoryPressure?
    private var timer: DispatchSourceTimer?
    private let observingSystem: Bool
    private final class Observer {
        weak var value: AtlasViewportResidency?
        init(_ value: AtlasViewportResidency) { self.value = value }
    }
    private var observers: [Observer] = []

    init(observingSystem: Bool = false) {
        self.observingSystem = observingSystem
        guard observingSystem else { return }
        let source = DispatchSource.makeMemoryPressureSource(eventMask: [.normal, .warning, .critical], queue: .main)
        self.source = source
        source.setEventHandler { [weak self] in
            // The dispatch source is explicitly delivered on the main queue.
            MainActor.assumeIsolated {
                guard let self, let events = self.source?.data else { return }
                self.receive(events, at: DispatchTime.now().uptimeNanoseconds)
            }
        }
        source.resume()
    }
    deinit { source?.cancel(); timer?.cancel() }

    fileprivate func attach(_ value: AtlasViewportResidency) {
        observers.removeAll { $0.value == nil || $0.value === value }
        observers.append(Observer(value))
        value.applyMemoryPressure(state)
    }
    fileprivate func detach(_ value: AtlasViewportResidency) {
        observers.removeAll { $0.value == nil || $0.value === value }
    }

    /// Coalesced native flags prefer the strongest pressure. Repeated normal
    /// notifications cannot bypass or restart a recovery interval.
    func receive(_ events: DispatchSource.MemoryPressureEvent, at now: UInt64) {
        let previous = state
        if events.contains(.critical) {
            state = State(phase: .critical, selectedOnly: true); recoveryDeadline = nil
        } else if events.contains(.warning) {
            state = State(phase: .constrained, selectedOnly: false); recoveryDeadline = nil
        } else if events.contains(.normal), state.phase != .normal, state.phase != .recovery {
            state = State(phase: .recovery, selectedOnly: state.selectedOnly)
            recoveryDeadline = deadline(after: now)
        } else { return }
        if previous != state { publish() }
        scheduleRecovery(from: now)
    }

    /// A critical recovery first keeps selected-only admission for five quiet
    /// seconds, then allows a small visible set for another five. Warning
    /// recovery keeps its reduced budget for one interval. An old timer checks
    /// the current deadline, so it cannot undo a renewed warning or critical event.
    func advanceRecovery(at now: UInt64) {
        guard state.phase == .recovery, let deadline = recoveryDeadline, now >= deadline else { return }
        if state.selectedOnly {
            state = State(phase: .recovery, selectedOnly: false)
            recoveryDeadline = self.deadline(after: now)
        } else {
            state = .normal; recoveryDeadline = nil
        }
        publish()
        scheduleRecovery(from: now)
    }

    private func deadline(after now: UInt64) -> UInt64? {
        let (value, overflow) = now.addingReportingOverflow(Self.quietInterval)
        // An exhausted clock stays conservative rather than wrapping into an
        // immediately expired deadline.
        return overflow ? nil : value
    }
    private func scheduleRecovery(from now: UInt64) {
        timer?.cancel(); timer = nil
        guard observingSystem, let deadline = recoveryDeadline, deadline > now else { return }
        let timer = DispatchSource.makeTimerSource(queue: .main)
        self.timer = timer
        timer.schedule(deadline: .now() + .nanoseconds(Int(min(deadline - now, UInt64(Int.max)))))
        timer.setEventHandler { [weak self] in
            MainActor.assumeIsolated { self?.advanceRecovery(at: DispatchTime.now().uptimeNanoseconds) }
        }
        timer.resume()
    }
    private func publish() {
        observers.removeAll { $0.value == nil }
        for observer in observers { observer.value?.applyMemoryPressure(state) }
    }
}

/// A hierarchy over immutable metadata. Exact viewport culling happens before
/// leaf candidates consume the bounded query allowance, so a late small parcel
/// in a dense region remains reachable when the camera zooms into that region.
fileprivate struct AtlasViewportIndex: Sendable {
    private struct Leaf: Sendable {
        let id: UInt64
        let ordinal: Int  // Paint order is independent of response-local file IDs.
        let rect: CGRect
    }
    private struct Node: Sendable {
        let bounds: CGRect
        let first, last: Int
        let children: [Int]
        let leaves: [Leaf]
    }
    private let nodes: [Node]
    private let previewRoot, readerOnlyRoot: Int?

    init(entries: [AtlasProjectCatalog.Entry], readerOnly: Set<UInt64> = []) {
        var result: [Node] = []
        result.reserveCapacity(entries.count * 2)
        func build(_ members: [Leaf]) -> Int {
            // Roots are nonempty; internal nodes split at least nine leaves.
            let first = members[0]
            var bounds = first.rect, lowest = first.ordinal, highest = first.ordinal
            for leaf in members.dropFirst() {
                bounds = bounds.union(leaf.rect)
                lowest = min(lowest, leaf.ordinal); highest = max(highest, leaf.ordinal)
            }
            let children: [Int], leaves: [Leaf]
            if members.count <= 8 {
                children = []; leaves = members
            } else {
                let horizontal = bounds.width >= bounds.height
                let ordered = members.sorted {
                    let left = horizontal ? $0.rect.midX : $0.rect.midY
                    let right = horizontal ? $1.rect.midX : $1.rect.midY
                    return left == right ? $0.id < $1.id : left < right
                }
                let middle = ordered.count / 2
                children = [build(Array(ordered[..<middle])), build(Array(ordered[middle...]))]
                leaves = []
            }
            result.append(Node(bounds: bounds, first: lowest, last: highest, children: children, leaves: leaves))
            return result.count - 1
        }
        var preview: [Leaf] = [], metadataOnly: [Leaf] = []
        for (ordinal, entry) in entries.enumerated() {
            let leaf = Leaf(id: entry.id, ordinal: ordinal, rect: Self.rect(entry))
            if readerOnly.contains(entry.id) { metadataOnly.append(leaf) }
            else { preview.append(leaf) }
        }
        previewRoot = preview.isEmpty ? nil : build(preview)
        readerOnlyRoot = metadataOnly.isEmpty ? nil : build(metadataOnly)
        nodes = result
    }
    static func rect(_ entry: AtlasProjectCatalog.Entry) -> CGRect {
        CGRect(x: entry.x, y: entry.y, width: entry.width, height: entry.height)
    }
    /// Both queries use metadata even before source previews load. Taps select
    /// the last painted parcel containing the point. A bookmark in a gutter
    /// chooses the nearest closed rectangle, with the first ordinal breaking ties.
    enum PointQuery { case hit, nearest }
    func file(at point: CGPoint, query: PointQuery) -> UInt64? {
        guard point.x.isFinite, point.y.isFinite else { return nil }
        typealias Score = (CGFloat, Int)
        var best: Score = (.infinity, .max)
        var file: UInt64?
        func distance(to rect: CGRect) -> CGFloat {
            if query == .hit { return rect.contains(point) ? 0 : .infinity }
            let dx = max(0, max(rect.minX - point.x, point.x - rect.maxX))
            let dy = max(0, max(rect.minY - point.y, point.y - rect.maxY))
            return hypot(dx, dy)
        }
        func lowerBound(_ node: Node) -> Score {
            (distance(to: node.bounds), query == .hit ? -node.last : node.first)
        }
        func visit(_ index: Int) {
            let node = nodes[index], bound = lowerBound(node)
            guard bound.0.isFinite, bound < best else { return }
            if node.children.isEmpty {
                for leaf in node.leaves {
                    let score = (distance(to: leaf.rect), query == .hit ? -leaf.ordinal : leaf.ordinal)
                    if score.0.isFinite, score < best { best = score; file = leaf.id }
                }
            } else {
                let a = node.children[0], b = node.children[1]
                if lowerBound(nodes[a]) < lowerBound(nodes[b]) { visit(a); visit(b) }
                else { visit(b); visit(a) }
            }
        }
        if let previewRoot { visit(previewRoot) }
        if let readerOnlyRoot { visit(readerOnlyRoot) }
        return file
    }
    func candidates(in viewport: CGRect, scale: Double, eligible: (UInt64) -> Bool) -> [UInt64] {
        guard let previewRoot else { return [] }
        var stack = [previewRoot], result: [UInt64] = [], visits = 0
        while let index = stack.popLast(), visits < 16_384, result.count < 4096 {
            visits += 1
            let node = nodes[index], bounds = node.bounds
            guard bounds.intersects(viewport), bounds.width * scale >= 48, bounds.height * scale >= 32,
                  bounds.width * scale * bounds.height * scale >= 4096 else { continue }
            if node.children.isEmpty {
                for leaf in node.leaves {
                    guard result.count < 4096 else { break }
                    let rect = leaf.rect
                    guard rect.intersects(viewport), rect.width * scale >= 48, rect.height * scale >= 32,
                          rect.width * scale * rect.height * scale >= 4096 else { continue }
                    if eligible(leaf.id) { result.append(leaf.id) }
                }
            }
            else {
                // Nearby branches reach admission first in a broad view; at
                // close zoom unrelated branches are rejected geometrically.
                let ordered = node.children.sorted {
                    let a = nodes[$0].bounds, b = nodes[$1].bounds
                    let left = pow(a.midX - viewport.midX, 2) + pow(a.midY - viewport.midY, 2)
                    let right = pow(b.midX - viewport.midX, 2) + pow(b.midY - viewport.midY, 2)
                    return left > right
                }
                stack.append(contentsOf: ordered)
            }
        }
        return result
    }
}

/// Immutable admission and spatial-index work can be prepared on the bounded
/// project worker. Publication then swaps this admitted value without scanning
/// or sorting the project on the interaction thread.
struct AtlasViewportLayout: Sendable {
    let root: String
    let revision: UUID
    fileprivate let entries: [UInt64: AtlasProjectCatalog.Entry]
    fileprivate let rawIDs: [[UInt8]: UInt64]
    fileprivate let index: AtlasViewportIndex
    fileprivate let previewStates: [UInt64: AtlasViewportPreviewState]

    static func prepare(root: String, entries next: [AtlasProjectCatalog.Entry], revision: UUID) -> Self? {
        guard root.hasPrefix("/"), root.utf8.count <= 16_384, !root.utf8.contains(0),
              next.count <= AtlasProjectCatalog.maximumFiles else { return nil }
        var entries: [UInt64: AtlasProjectCatalog.Entry] = [:], rawIDs: [[UInt8]: UInt64] = [:]
        var spellings: [String: (UInt64, [UInt8])] = [:], ambiguous = Set<UInt64>()
        for entry in next {
            let raw = entry.rawPath
            guard entry.id > 0, entries[entry.id] == nil, rawIDs[raw] == nil,
                  !raw.isEmpty, raw.count <= 16_384, raw.first != 47, !raw.contains(0),
                  !raw.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                      $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
                  }), [entry.x, entry.y, entry.width, entry.height, entry.x + entry.width, entry.y + entry.height]
                    .allSatisfy({ $0.isFinite && abs($0) <= 1e12 }),
                  entry.width > 0, entry.height > 0,
                  entry.sourcePath.map({ $0.utf8.elementsEqual(raw) }) ?? (String(bytes: raw, encoding: .utf8) == nil) else {
                return nil
            }
            entries[entry.id] = entry; rawIDs[raw] = entry.id
            if let path = entry.sourcePath {
                if let previous = spellings[path], previous.1 != raw { ambiguous.insert(previous.0); ambiguous.insert(entry.id) }
                else { spellings[path] = (entry.id, raw) }
            }
        }
        var states: [UInt64: AtlasViewportPreviewState] = [:]
        for entry in next where entry.sourcePath == nil || ambiguous.contains(entry.id)
            || entry.observedBytes > UInt64(AtlasViewportLimits.sourceFileBytes) {
            states[entry.id] = .readerOnly
        }
        return Self(root: root, revision: revision, entries: entries, rawIDs: rawIDs,
            index: AtlasViewportIndex(entries: next, readerOnly: Set(states.keys)), previewStates: states)
    }
}

/// Fixed metadata parcels and evictable source are separate lifetimes. Only
/// install changes the layout domain; viewport arrivals change contentRevision
/// and cannot move neighboring parcels, reset the camera or authorize a reader.
@Observable @MainActor final class AtlasViewportResidency {
    private(set) var root = ""
    private(set) var layoutRevision = UUID()
    private(set) var isInstalled = false
    private(set) var isLoading = false
    private(set) var isSuspended = false
    private(set) var markdownPreviewEnabled = false
    private(set) var documents: [String: AtlasDocument] = [:]
    private(set) var tiles: [AtlasTextTile] = []
    private(set) var contentRevision = UUID()
    private(set) var previewStates: [UInt64: AtlasViewportPreviewState] = [:]
    private(set) var residentCost = AtlasViewportCost()
    private(set) var residentCount = 0
    private(set) var pendingCount = 0
    private(set) var memoryPressure = AtlasViewportMemoryPressure.State.normal
    private var readerOnlyCount = 0
    private var unavailableCount = 0
    private var capacityLimitedCount = 0

    @ObservationIgnored private let limits: AtlasViewportLimits
    @ObservationIgnored private let worker: AtlasViewportWorker
    @ObservationIgnored private let retirement: AtlasNativeRetirement
    @ObservationIgnored private let pressureMonitor: AtlasViewportMemoryPressure
    @ObservationIgnored private var entries: [UInt64: AtlasProjectCatalog.Entry] = [:]
    @ObservationIgnored private var index = AtlasViewportIndex(entries: [])
    @ObservationIgnored private var rawIDs: [[UInt8]: UInt64] = [:]
    @ObservationIgnored private var residents: [UInt64: Resident] = [:]
    @ObservationIgnored private var wanted: [UInt64] = []
    // Ranked alternates from the last accepted viewport. Final file refusals
    // can fill a vacant slot without another camera event or spatial traversal.
    @ObservationIgnored private var remainingDemand: ArraySlice<UInt64> = []
    @ObservationIgnored private var blocked = Set<UInt64>()
    @ObservationIgnored private var selectedID: UInt64?
    @ObservationIgnored private var active: AtlasViewportWork?
    @ObservationIgnored private var accessLease: AnyObject?
    @ObservationIgnored private var cacheLease: AnyObject?
    @ObservationIgnored private var sourceCacheHandle: UInt64 = 0
    @ObservationIgnored private var serial: UInt64 = 0
    @ObservationIgnored private var lastViewport: CGRect?
    @ObservationIgnored private var lastScale = 0.0
    @ObservationIgnored private var lastSelected: [UInt8]?

    private struct Resident {
        let prepared: AtlasViewportPreparedSource
        let layoutRevision: UUID
        var used: UInt64
    }

    init(limits: AtlasViewportLimits = AtlasViewportLimits(), transport: AtlasViewportSourceTransport = .live,
         markdown: AtlasViewportMarkdownTransport? = nil, memoryPressure: AtlasViewportMemoryPressure? = nil) {
        precondition(limits.valid)
        self.limits = limits; worker = AtlasViewportWorker(transport: transport, markdown: markdown)
        pressureMonitor = memoryPressure ?? .shared
        self.memoryPressure = pressureMonitor.state
        // Source rows, cached Markdown and current placement can have separate
        // retained lifetimes. Admit all three bounded sets plus preparation. Older
        // accepted/flight frames consume this same finite pool and apply
        // backpressure until their completion releases its lifetime credits.
        retirement = AtlasNativeRetirement(maximumSlots: min(520, limits.documents * 3 + 8),
            maximumBytes: limits.estimatedBytes * 3 + AtlasViewportLimits.preparationBytes)
        retirement.onAvailability { [weak self] in DispatchQueue.main.async { [weak self] in self?.pump() } }
    }

    var retainedNativeBytes: Int { retirement.reservedBytes }
    var retainedNativeSlots: Int { retirement.reservedSlots }

    /// An explicit window preference changes only admitted visible Markdown
    /// demand. Existing captured pixels stay readable until a complete worker
    /// replacement is accepted; camera/layout/source authority does not change.
    func setMarkdownPreviewEnabled(_ enabled: Bool) {
        guard markdownPreviewEnabled != enabled else { return }
        markdownPreviewEnabled = enabled
        if let active, active.markdownRequested != wantsMarkdown(active.entry) {
            active.cancellation.cancel()
        }
        for id in wanted {
            guard let entry = entries[id], AtlasViewportMarkdownTransport.supports(entry.rawPath) else { continue }
            blocked.remove(id)
            if current(id), let resident = residents[id] { setState(id, publishedState(resident.prepared)) }
            else if previewStates[id] != .readerOnly { setState(id, .queued) }
        }
        pump()
    }

    private func wantsMarkdown(_ entry: AtlasProjectCatalog.Entry) -> Bool {
        markdownPreviewEnabled && AtlasViewportMarkdownTransport.supports(entry.rawPath)
    }
    private func current(_ id: UInt64) -> Bool {
        guard let resident = residents[id], let entry = entries[id] else { return false }
        return resident.layoutRevision == layoutRevision
            && resident.prepared.markdownRequested == wantsMarkdown(entry)
    }
    private func publishedState(_ prepared: AtlasViewportPreparedSource) -> AtlasViewportPreviewState {
        if prepared.document.source.text.isEmpty { return .empty }
        if prepared.document.presentation == .markdown { return .markdown }
        return prepared.markdownRequested ? .sourceFallback : .ready
    }

    /// Navigation can reveal a matching metadata atlas before any source is
    /// resident. Tying this to tiles would prevent the first visible frame
    /// from requesting source, and would hide empty or reader-only scopes.
    /// This is layout availability, not proof of presented pixels or text.
    func hasInstalledLayout(root: String, layoutRevision: UUID) -> Bool {
        isInstalled && self.layoutRevision == layoutRevision
            && self.root.utf8.elementsEqual(root.utf8)
    }

    func file(at point: CGPoint, nearest: Bool = false, revision: UUID) -> AtlasProjectCatalog.Entry? {
        guard isInstalled, revision == layoutRevision,
              let id = index.file(at: point, query: nearest ? .nearest : .hit) else { return nil }
        return entries[id]
    }

    func file(rawPath: [UInt8], revision: UUID) -> AtlasProjectCatalog.Entry? {
        guard isInstalled, revision == layoutRevision, let id = rawIDs[rawPath] else { return nil }
        return entries[id]
    }

    var summary: String {
        guard isInstalled else { return "Source previews are not prepared." }
        var result = "\(documents.count) resident source previews"
        if isLoading || pendingCount > 0 { result += " · \(pendingCount + (isLoading ? 1 : 0)) loading or queued" }
        if readerOnlyCount > 0 { result += " · \(readerOnlyCount) reader-only" }
        if unavailableCount > 0 { result += " · \(unavailableCount) previews unavailable" }
        if capacityLimitedCount > 0 { result += " · \(capacityLimitedCount) waiting for preview memory" }
        switch memoryPressure.phase {
        case .normal: break
        case .constrained: result += " · low memory: fewer source previews"
        case .critical: result += " · memory pressure: selected preview only"
        case .recovery: result += " · restoring previews gradually"
        }
        return result + ". Zoom to load source; every parcel remains selectable."
    }

    /// Metadata admission is all-or-nothing. `preservingCaptures` is reserved
    /// for an explicit scope repack of this exact project; refresh defaults to
    /// a new source observation domain even when its root spelling is unchanged.
    @discardableResult func install(root nextRoot: String, entries next: [AtlasProjectCatalog.Entry],
        layoutRevision nextRevision: UUID, accessLease: AnyObject?, sourceCacheHandle: UInt64 = 0,
        cacheLease: AnyObject? = nil, preservingCaptures: Bool = false) -> Bool {
        guard let layout = AtlasViewportLayout.prepare(root: nextRoot, entries: next, revision: nextRevision) else { return false }
        return install(layout, accessLease: accessLease, sourceCacheHandle: sourceCacheHandle,
            cacheLease: cacheLease, preservingCaptures: preservingCaptures)
    }

    @discardableResult func install(_ layout: AtlasViewportLayout, accessLease: AnyObject?,
        sourceCacheHandle: UInt64 = 0, cacheLease: AnyObject? = nil, preservingCaptures: Bool = false) -> Bool {
        guard sourceCacheHandle == 0 || cacheLease != nil else { return false }
        let preserve = preservingCaptures && isInstalled && root.utf8.elementsEqual(layout.root.utf8)
        active?.cancellation.cancel()
        if !preserve {
            residents = [:]
        }
        root = layout.root; layoutRevision = layout.revision
        entries = layout.entries; rawIDs = layout.rawIDs; index = layout.index
        self.accessLease = accessLease; self.cacheLease = cacheLease; self.sourceCacheHandle = sourceCacheHandle
        isInstalled = true; isSuspended = false; selectedID = nil
        wanted = []; remainingDemand = []; blocked = []; pendingCount = 0; lastViewport = nil; lastSelected = nil
        previewStates = layout.previewStates; readerOnlyCount = layout.previewStates.count
        unavailableCount = 0; capacityLimitedCount = 0
        publish()
        isLoading = active != nil
        pressureMonitor.attach(self)
        return true
    }

    /// Called for an accepted displayed frame. Geometry-only work is bounded;
    /// tiny overview parcels do not trigger all-project source preparation.
    func updateViewport(_ worldRect: CGRect, scale: Double, selectedPath: String?) {
        guard isInstalled, [worldRect.minX, worldRect.minY, worldRect.width, worldRect.height, scale].allSatisfy(\.isFinite),
              worldRect.width > 0, worldRect.height > 0, scale > 0, scale <= 1e12,
              abs(worldRect.minX) <= 1e12, abs(worldRect.minY) <= 1e12,
              worldRect.width <= 1e12, worldRect.height <= 1e12 else { return }
        let selected = selectedPath.map { Array($0.utf8) }
        guard isSuspended || lastViewport != worldRect || lastScale != scale || lastSelected != selected else { return }
        isSuspended = false
        lastViewport = worldRect; lastScale = scale; lastSelected = selected
        let previousSelection = selectedID
        selectedID = selected.flatMap { rawIDs[$0] }
        if previousSelection != selectedID, let selectedID, blocked.remove(selectedID) != nil,
           previewStates[selectedID] == .capacityLimited {
            setState(selectedID, .cold)
        }
        var candidates: [(id: UInt64, area: Double, distance: Double)] = []
        let eligible = index.candidates(in: worldRect, scale: scale) { id in
            previewStates[id] != .readerOnly && previewStates[id] != .unavailable
        }
        for id in eligible {
            guard let entry = entries[id] else { continue }
            let rect = AtlasViewportIndex.rect(entry), visible = rect.intersection(worldRect)
            guard !visible.isNull, rect.width * scale >= 48, rect.height * scale >= 32,
                  rect.width * scale * rect.height * scale >= 4096 else { continue }
            let area = visible.width * scale * visible.height * scale
            let distance = pow(rect.midX - worldRect.midX, 2) + pow(rect.midY - worldRect.midY, 2)
            candidates.append((id, area, distance))
        }
        candidates.sort {
            if $0.area != $1.area { return $0.area > $1.area }
            if $0.distance != $1.distance { return $0.distance < $1.distance }
            return $0.id < $1.id
        }
        var ranked: [UInt64] = []
        if let selectedID, previewStates[selectedID] != .readerOnly, previewStates[selectedID] != .unavailable {
            ranked.append(selectedID)
        }
        for candidate in candidates where candidate.id != selectedID { ranked.append(candidate.id) }
        if memoryPressure.selectedOnly { ranked = ranked.filter { $0 == selectedID } }
        let next = ranked.prefix(effectiveLimits.maximumDemand)
        remainingDemand = ranked.dropFirst(next.count)
        setDemand(Array(next))
    }

    func suspend() {
        isSuspended = true; active?.cancellation.cancel()
        for id in wanted where previewStates[id] == .queued || previewStates[id] == .capacityLimited { setState(id, .cold) }
        wanted = []; remainingDemand = []; pendingCount = 0; lastViewport = nil
    }

    func close() {
        pressureMonitor.detach(self)
        active?.cancellation.cancel()
        isInstalled = false; isSuspended = true; root = ""; layoutRevision = UUID()
        entries = [:]; rawIDs = [:]; index = AtlasViewportIndex(entries: [])
        accessLease = nil; cacheLease = nil; sourceCacheHandle = 0
        wanted = []; remainingDemand = []; blocked = []; selectedID = nil; previewStates = [:]
        readerOnlyCount = 0; unavailableCount = 0; capacityLimitedCount = 0
        lastViewport = nil; lastSelected = nil; pendingCount = 0
        residents = [:]
        publish()
        isLoading = active != nil
    }

    func retryUnavailable() {
        guard isInstalled else { return }
        for (id, state) in previewStates where state == .unavailable || state == .capacityLimited {
            setState(id, .cold)
        }
        blocked = []
        if !isSuspended, let viewport = lastViewport {
            lastViewport = nil
            updateViewport(viewport, scale: lastScale, selectedPath: lastSelected.flatMap { String(bytes: $0, encoding: .utf8) })
        } else {
            pump()
        }
    }

    func document(forRawPath raw: [UInt8]) -> AtlasDocument? {
        guard let id = rawIDs[raw], let resident = residents[id], resident.layoutRevision == layoutRevision,
              resident.prepared.rawPath == raw else { return nil }
        return resident.prepared.document
    }

    func preparation(forRawPath raw: [UInt8]) -> AtlasViewportPreparation? {
        guard let id = rawIDs[raw], let resident = residents[id], resident.layoutRevision == layoutRevision,
              resident.prepared.rawPath == raw else { return nil }
        return resident.prepared.preparation
    }

    /// These are soft residency targets within the existing hard limits, not
    /// a process-footprint promise. In-flight frames and entered preparation
    /// keep their existing credits until their actual final owner releases them.
    private var effectiveLimits: AtlasViewportLimits {
        guard memoryPressure.phase != .normal else { return limits }
        var result = limits
        result.documents = min(result.documents, memoryPressure.selectedOnly ? 1 : 8)
        result.maximumDemand = min(result.maximumDemand, memoryPressure.selectedOnly ? 1 : 8)
        result.sourceBytes = max(1, result.sourceBytes / 2)
        result.glyphs = max(1, result.glyphs / 2)
        result.runs = max(1, result.runs / 2)
        result.tiles = max(1, result.tiles / 2)
        result.pixels = max(1, result.pixels / 2)
        result.estimatedBytes = max(1, result.estimatedBytes / 2)
        return result
    }

    fileprivate func applyMemoryPressure(_ state: AtlasViewportMemoryPressure.State) {
        let admissionChanged = memoryPressure.selectedOnly != state.selectedOnly
            || (memoryPressure.phase == .normal) != (state.phase == .normal)
        memoryPressure = state
        guard isInstalled else { return }
        // Retry capacity refusals when the admission band changes, including
        // identical demand IDs after recovery. Starting a quiet interval alone
        // changes no budget and must not recapture an already refused preview.
        if admissionChanged {
            for id in blocked where previewStates[id] == .capacityLimited { setState(id, .cold) }
            blocked = []
        }
        if !isSuspended, let viewport = lastViewport {
            lastViewport = nil
            updateViewport(viewport, scale: lastScale,
                selectedPath: lastSelected.flatMap { String(bytes: $0, encoding: .utf8) })
        } else {
            trimForMemoryPressure()
            // An idle/hidden view must not resume discovery or source work.
            if !isSuspended { pump() }
        }
    }

    private func trimForMemoryPressure() {
        guard memoryPressure.phase != .normal else { return }
        let target = effectiveLimits
        var changed = false
        // At most the configured resident count is visited. Remove cold
        // entries before considering useful visible previews; selected source
        // is protected even when it alone exceeds a reduced soft target.
        for id in residents.keys.filter({ $0 != selectedID && !wanted.contains($0) }) {
            residents.removeValue(forKey: id)
            if entries[id] != nil { setState(id, .cold) }
            changed = true
        }
        var cost = residents.values.reduce(AtlasViewportCost()) { $0 + $1.prepared.cost }
        let victims = residents.keys.filter { $0 != selectedID }.sorted {
            let left = wanted.firstIndex(of: $0) ?? Int.max
            let right = wanted.firstIndex(of: $1) ?? Int.max
            if left != right { return left > right }
            return $0 > $1
        }
        for id in victims {
            if residents.count <= target.documents, cost.fits(target) { break }
            residents.removeValue(forKey: id)
            if entries[id] != nil {
                setState(id, .capacityLimited)
                blocked.insert(id)
            }
            changed = true
            cost = residents.values.reduce(AtlasViewportCost()) { $0 + $1.prepared.cost }
        }
        // Normal owner release preserves independently pinned readers and
        // displayed/in-flight tiles. Their retirement leases remain charged.
        if changed { publish() }
    }

    private func backfillRefused(_ id: UInt64) {
        guard let slot = wanted.firstIndex(of: id) else { return }
        wanted.remove(at: slot)
        while let next = remainingDemand.popFirst() {
            guard previewStates[next] != .readerOnly, previewStates[next] != .unavailable else { continue }
            wanted.append(next)
            if previewStates[next] == nil || previewStates[next] == .cold { setState(next, .queued) }
            break
        }
    }

    private func setDemand(_ next: [UInt64]) {
        if wanted != next {
            for id in wanted where previewStates[id] == .queued || previewStates[id] == .capacityLimited { setState(id, .cold) }
            for id in blocked where previewStates[id] == .capacityLimited { setState(id, .cold) }
            blocked = []; wanted = next
        }
        serial &+= 1
        for id in wanted {
            if var resident = residents[id] { resident.used = serial; residents[id] = resident }
            if previewStates[id] == nil || previewStates[id] == .cold { setState(id, .queued) }
        }
        if let active, !wanted.contains(active.entry.id) { active.cancellation.cancel() }
        trimForMemoryPressure()
        pump()
    }

    private func pump() {
        pendingCount = wanted.filter { id in
            !current(id) && previewStates[id] != .readerOnly
                && previewStates[id] != .unavailable && !blocked.contains(id) && active?.entry.id != id
        }.count
        guard isInstalled, !isSuspended, active == nil else { return }
        guard let id = wanted.first(where: {
            !current($0) && previewStates[$0] != .readerOnly
                && previewStates[$0] != .unavailable && !blocked.contains($0)
        }), let entry = entries[id] else { isLoading = false; pendingCount = 0; return }
        guard let credit = retirement.reserve(bytes: AtlasViewportLimits.preparationBytes) else {
            // This is temporary backpressure, never a terminal file refusal.
            // A coalesced retirement wakeup resumes the latest demand only.
            setState(id, .capacityLimited); isLoading = false
            return
        }
        let reused = residents[id].flatMap { $0.prepared.rawPath == entry.rawPath ? $0.prepared : nil }
        let work = AtlasViewportWork(root: root, layoutRevision: layoutRevision, entry: entry,
            sourceCacheHandle: sourceCacheHandle, limits: limits, accessLease: accessLease,
            cacheLease: cacheLease, reuse: reused, retirementLease: credit,
            markdownRequested: wantsMarkdown(entry))
        active = work; isLoading = true; pendingCount = max(0, pendingCount - 1); setState(id, .loading)
        worker.submit(work) { [weak self, work] result in self?.finish(work, result: result) }
    }

    private func finish(_ work: AtlasViewportWork, result: AtlasViewportWorkResult) {
        guard active?.id == work.id else { return }
        active = nil; isLoading = false
        guard isInstalled, !isSuspended, layoutRevision == work.layoutRevision,
              root.utf8.elementsEqual(work.root.utf8), let entry = entries[work.entry.id],
              entry.rawPath == work.entry.rawPath, wanted.contains(entry.id), !work.cancellation.isCanceled,
              work.markdownRequested == wantsMarkdown(entry) else {
            if layoutRevision == work.layoutRevision, previewStates[work.entry.id] == .loading {
                if current(work.entry.id), let resident = residents[work.entry.id] {
                    setState(work.entry.id, publishedState(resident.prepared))
                } else { setState(work.entry.id, .cold) }
            }
            pump(); return
        }
        switch result {
        case .canceled:
            setState(entry.id, .cold)
        case .refused(let state):
            setState(entry.id, state)
            if state == .capacityLimited { blocked.insert(entry.id) }
            if state == .readerOnly || state == .unavailable { backfillRefused(entry.id) }
        case .prepared(let prepared):
            guard prepared.rawPath == entry.rawPath,
                  prepared.document.source.path.utf8.elementsEqual(entry.rawPath),
                  prepared.document.renderTiles.allSatisfy({
                      $0.path.utf8.elementsEqual(entry.rawPath) && $0.parcelRect == AtlasViewportIndex.rect(entry)
                  }) else { setState(entry.id, .unavailable); backfillRefused(entry.id); pump(); return }
            if admit(prepared, for: entry.id) {
                setState(entry.id, publishedState(prepared))
                publish()
            } else {
                setState(entry.id, .capacityLimited); blocked.insert(entry.id)
            }
        }
        pump()
    }

    private func admit(_ prepared: AtlasViewportPreparedSource, for id: UInt64) -> Bool {
        guard prepared.cost.fits(limits) else { return false }
        var limits = effectiveLimits
        if id == selectedID {
            // One selected preview may retain its original admitted quality.
            // All optional neighbors still compete within the reduced target.
            limits.sourceBytes = max(limits.sourceBytes, prepared.cost.sourceBytes)
            limits.glyphs = max(limits.glyphs, prepared.cost.glyphs)
            limits.runs = max(limits.runs, prepared.cost.runs)
            limits.tiles = max(limits.tiles, prepared.cost.tiles)
            limits.pixels = max(limits.pixels, prepared.cost.pixels)
            limits.estimatedBytes = max(limits.estimatedBytes, prepared.cost.estimatedBytes)
        }
        guard prepared.cost.fits(limits) else { return false }
        let priority = wanted.firstIndex(of: id) ?? Int.max
        var count = residents.count - (residents[id] == nil ? 0 : 1)
        var cost = residents.filter { $0.key != id }.values.reduce(AtlasViewportCost()) { $0 + $1.prepared.cost }
        let candidates = residents.filter { key, _ in
            key != id && key != selectedID && (wanted.firstIndex(of: key).map { $0 > priority } ?? true)
        }.sorted {
            let leftVisible = wanted.contains($0.key), rightVisible = wanted.contains($1.key)
            if leftVisible != rightVisible { return !leftVisible }
            if $0.value.used != $1.value.used { return $0.value.used < $1.value.used }
            return $0.key < $1.key
        }
        var victims: [UInt64] = []
        for candidate in candidates {
            if count < limits.documents, (cost + prepared.cost).fits(limits) { break }
            victims.append(candidate.key); count -= 1
            // Recompute bounded totals, avoiding subtraction/underflow admission.
            cost = residents.filter { $0.key != id && !victims.contains($0.key) }.values
                .reduce(AtlasViewportCost()) { $0 + $1.prepared.cost }
        }
        guard count < limits.documents, (cost + prepared.cost).fits(limits) else { return false }
        for victim in victims {
            residents.removeValue(forKey: victim)
            if entries[victim] != nil { setState(victim, .cold) }
        }
        residents[id] = Resident(prepared: prepared, layoutRevision: layoutRevision, used: serial)
        return true
    }

    private func publish() {
        var nextDocuments: [String: AtlasDocument] = [:], nextTiles: [AtlasTextTile] = []
        for id in residents.keys.sorted() {
            guard let entry = entries[id], let resident = residents[id], resident.layoutRevision == layoutRevision,
                  resident.prepared.rawPath == entry.rawPath, let path = entry.sourcePath else { continue }
            let document = resident.prepared.document
            guard document.source.path.utf8.elementsEqual(entry.rawPath), nextDocuments[path] == nil else { continue }
            nextDocuments[path] = document; nextTiles.append(contentsOf: document.renderTiles)
        }
        documents = nextDocuments; tiles = nextTiles; contentRevision = UUID()
        residentCount = residents.count
        residentCost = residents.values.reduce(AtlasViewportCost()) { $0 + $1.prepared.cost }
    }

    private func setState(_ id: UInt64, _ state: AtlasViewportPreviewState) {
        let previous = previewStates[id] ?? .cold
        guard previous != state || previewStates[id] == nil else { return }
        if previous == .readerOnly { readerOnlyCount -= 1 }
        if previous == .unavailable { unavailableCount -= 1 }
        if previous == .capacityLimited { capacityLimitedCount -= 1 }
        if state == .readerOnly { readerOnlyCount += 1 }
        if state == .unavailable { unavailableCount += 1 }
        if state == .capacityLimited { capacityLimitedCount += 1 }
        previewStates[id] = state
    }
}

