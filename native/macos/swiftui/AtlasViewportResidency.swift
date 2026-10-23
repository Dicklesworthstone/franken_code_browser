import Foundation
import CoreGraphics
import Observation
import Dispatch

/// A hierarchy over immutable metadata. Exact viewport culling happens before
/// leaf candidates consume the bounded query allowance, so a late small parcel
/// in a dense region remains reachable when the camera zooms into that region.
private struct AtlasViewportIndex {
    private struct Leaf { let id: UInt64; let rect: CGRect }
    private struct Node {
        let bounds: CGRect
        let children: [Int]
        let leaves: [Leaf]
    }
    private let nodes: [Node]
    init(entries: [AtlasProjectCatalog.Entry]) {
        var result: [Node] = []
        result.reserveCapacity(entries.count * 2)
        func build(_ members: [AtlasProjectCatalog.Entry]) -> Int {
            let bounds = members.reduce(CGRect.null) { $0.union(Self.rect($1)) }
            let index = result.count
            result.append(Node(bounds: bounds, children: [], leaves: []))
            if members.count <= 8 {
                result[index] = Node(bounds: bounds, children: [], leaves: members.map { Leaf(id: $0.id, rect: Self.rect($0)) })
            } else {
                let horizontal = bounds.width >= bounds.height
                let ordered = members.sorted {
                    let left = horizontal ? $0.x + $0.width / 2 : $0.y + $0.height / 2
                    let right = horizontal ? $1.x + $1.width / 2 : $1.y + $1.height / 2
                    return left == right ? $0.id < $1.id : left < right
                }
                let middle = ordered.count / 2
                let left = build(Array(ordered[..<middle])), right = build(Array(ordered[middle...]))
                result[index] = Node(bounds: bounds, children: [left, right], leaves: [])
            }
            return index
        }
        if !entries.isEmpty { _ = build(entries) }
        nodes = result
    }
    static func rect(_ entry: AtlasProjectCatalog.Entry) -> CGRect {
        CGRect(x: entry.x, y: entry.y, width: entry.width, height: entry.height)
    }
    func candidates(in viewport: CGRect, scale: Double) -> [UInt64] {
        guard !nodes.isEmpty else { return [] }
        var stack = [0], result: [UInt64] = [], visits = 0
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
                    result.append(leaf.id)
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

/// Fixed metadata parcels and evictable source are separate lifetimes. Only
/// install changes the layout domain; viewport arrivals change contentRevision
/// and cannot move neighboring parcels, reset the camera or authorize a reader.
@Observable @MainActor final class AtlasViewportResidency {
    private(set) var root = ""
    private(set) var layoutRevision = UUID()
    private(set) var isInstalled = false
    private(set) var isLoading = false
    private(set) var isSuspended = false
    private(set) var documents: [String: AtlasDocument] = [:]
    private(set) var tiles: [AtlasTextTile] = []
    private(set) var contentRevision = UUID()
    private(set) var previewStates: [UInt64: AtlasViewportPreviewState] = [:]
    private(set) var residentCost = AtlasViewportCost()
    private(set) var residentCount = 0
    private(set) var pendingCount = 0
    private var readerOnlyCount = 0, unavailableCount = 0, capacityLimitedCount = 0

    @ObservationIgnored private let limits: AtlasViewportLimits
    @ObservationIgnored private let worker: AtlasViewportWorker
    @ObservationIgnored private let retirement: AtlasNativeRetirement
    @ObservationIgnored private var entries: [UInt64: AtlasProjectCatalog.Entry] = [:]
    @ObservationIgnored private var index = AtlasViewportIndex(entries: [])
    @ObservationIgnored private var rawIDs: [[UInt8]: UInt64] = [:]
    @ObservationIgnored private var residents: [UInt64: Resident] = [:]
    @ObservationIgnored private var wanted: [UInt64] = []
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

    init(limits: AtlasViewportLimits = AtlasViewportLimits(), transport: AtlasViewportSourceTransport = .live) {
        precondition(limits.valid)
        self.limits = limits; worker = AtlasViewportWorker(transport: transport)
        // Source rows can be shared by an explicit scope's old/new placement.
        // Allow both bounded resident sets plus one full preparation. Older
        // accepted/flight frames consume this same finite pool and apply
        // backpressure until their completion releases its lifetime credits.
        retirement = AtlasNativeRetirement(maximumSlots: limits.documents * 2 + 8,
            maximumBytes: limits.estimatedBytes * 2 + AtlasViewportLimits.preparationBytes)
        retirement.onAvailability { [weak self] in DispatchQueue.main.async { [weak self] in self?.pump() } }
    }

    var retainedNativeBytes: Int { retirement.reservedBytes }
    var retainedNativeSlots: Int { retirement.reservedSlots }

    var summary: String {
        guard isInstalled else { return "Source previews are not prepared." }
        var result = "\(documents.count) resident source previews"
        if isLoading || pendingCount > 0 { result += " · \(pendingCount + (isLoading ? 1 : 0)) loading or queued" }
        if readerOnlyCount > 0 { result += " · \(readerOnlyCount) reader-only" }
        if unavailableCount > 0 { result += " · \(unavailableCount) previews unavailable" }
        if capacityLimitedCount > 0 { result += " · \(capacityLimitedCount) waiting for preview memory" }
        return result + ". Zoom to load source; every parcel remains selectable."
    }

    /// Metadata admission is all-or-nothing. `preservingCaptures` is reserved
    /// for an explicit scope repack of this exact project; refresh defaults to
    /// a new source observation domain even when its root spelling is unchanged.
    @discardableResult func install(root nextRoot: String, entries next: [AtlasProjectCatalog.Entry],
        layoutRevision nextRevision: UUID, accessLease: AnyObject?, sourceCacheHandle: UInt64 = 0,
        cacheLease: AnyObject? = nil, preservingCaptures: Bool = false) -> Bool {
        guard nextRoot.hasPrefix("/"), nextRoot.utf8.count <= 16_384, !nextRoot.utf8.contains(0),
              next.count <= AtlasProjectCatalog.maximumFiles,
              sourceCacheHandle == 0 || cacheLease != nil else { return false }
        var newEntries: [UInt64: AtlasProjectCatalog.Entry] = [:], newRawIDs: [[UInt8]: UInt64] = [:]
        var spellings: [String: (UInt64, [UInt8])] = [:], ambiguous = Set<UInt64>()
        for entry in next {
            let raw = entry.rawPath
            guard entry.id > 0, newEntries[entry.id] == nil, newRawIDs[raw] == nil,
                  !raw.isEmpty, raw.count <= 16_384, raw.first != 47, !raw.contains(0),
                  !raw.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                      $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
                  }), [entry.x, entry.y, entry.width, entry.height].allSatisfy({ $0.isFinite && abs($0) <= 1e12 }),
                  entry.width > 0, entry.height > 0,
                  entry.sourcePath.map({ $0.utf8.elementsEqual(raw) }) ?? (String(bytes: raw, encoding: .utf8) == nil) else {
                return false
            }
            newEntries[entry.id] = entry; newRawIDs[raw] = entry.id
            if let path = entry.sourcePath {
                if let previous = spellings[path], previous.1 != raw { ambiguous.insert(previous.0); ambiguous.insert(entry.id) }
                else { spellings[path] = (entry.id, raw) }
            }
        }
        let preserve = preservingCaptures && isInstalled && self.root.utf8.elementsEqual(nextRoot.utf8)
        active?.cancellation.cancel()
        if !preserve {
            residents = [:]
        }
        self.root = nextRoot; layoutRevision = nextRevision
        entries = newEntries; rawIDs = newRawIDs; index = AtlasViewportIndex(entries: next)
        self.accessLease = accessLease; self.cacheLease = cacheLease; self.sourceCacheHandle = sourceCacheHandle
        isInstalled = true; isSuspended = false; selectedID = nil
        wanted = []; blocked = []; pendingCount = 0; lastViewport = nil; lastSelected = nil
        previewStates = [:]; readerOnlyCount = 0; unavailableCount = 0; capacityLimitedCount = 0
        for entry in next where entry.sourcePath == nil || ambiguous.contains(entry.id)
            || entry.observedBytes > UInt64(AtlasViewportLimits.sourceFileBytes) {
            setState(entry.id, .readerOnly)
        }
        publish()
        isLoading = active != nil
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
        selectedID = selected.flatMap { rawIDs[$0] }
        var candidates: [(id: UInt64, area: Double, distance: Double)] = []
        for id in index.candidates(in: worldRect, scale: scale) {
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
        var next: [UInt64] = []
        if let selectedID { next.append(selectedID) }
        for candidate in candidates where candidate.id != selectedID {
            guard next.count < limits.maximumDemand else { break }
            guard previewStates[candidate.id] != .readerOnly else { continue }
            next.append(candidate.id)
        }
        setDemand(next)
    }

    func suspend() {
        isSuspended = true; active?.cancellation.cancel()
        for id in wanted where previewStates[id] == .queued || previewStates[id] == .capacityLimited { setState(id, .cold) }
        wanted = []; pendingCount = 0; lastViewport = nil
    }

    func close() {
        active?.cancellation.cancel()
        isInstalled = false; isSuspended = true; root = ""; layoutRevision = UUID()
        entries = [:]; rawIDs = [:]; index = AtlasViewportIndex(entries: [])
        accessLease = nil; cacheLease = nil; sourceCacheHandle = 0
        wanted = []; blocked = []; selectedID = nil; previewStates = [:]
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
        pump()
    }

    func document(forRawPath raw: [UInt8]) -> AtlasDocument? {
        guard let id = rawIDs[raw], let resident = residents[id], resident.layoutRevision == layoutRevision,
              resident.prepared.rawPath == raw else { return nil }
        return resident.prepared.document
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
        pump()
    }

    private func pump() {
        pendingCount = wanted.filter { id in
            residents[id]?.layoutRevision != layoutRevision && previewStates[id] != .readerOnly
                && previewStates[id] != .unavailable && !blocked.contains(id) && active?.entry.id != id
        }.count
        guard isInstalled, !isSuspended, active == nil else { return }
        guard let id = wanted.first(where: {
            residents[$0]?.layoutRevision != layoutRevision && previewStates[$0] != .readerOnly
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
            cacheLease: cacheLease, reuse: reused, retirementLease: credit)
        active = work; isLoading = true; pendingCount = max(0, pendingCount - 1); setState(id, .loading)
        worker.submit(work) { [weak self, work] result in self?.finish(work, result: result) }
    }

    private func finish(_ work: AtlasViewportWork, result: AtlasViewportWorkResult) {
        guard active?.id == work.id else { return }
        active = nil; isLoading = false
        guard isInstalled, !isSuspended, layoutRevision == work.layoutRevision,
              root.utf8.elementsEqual(work.root.utf8), let entry = entries[work.entry.id],
              entry.rawPath == work.entry.rawPath, wanted.contains(entry.id), !work.cancellation.isCanceled else {
            if layoutRevision == work.layoutRevision, previewStates[work.entry.id] == .loading {
                setState(work.entry.id, .cold)
            }
            pump(); return
        }
        switch result {
        case .canceled:
            setState(entry.id, .cold)
        case .refused(let state):
            setState(entry.id, state)
            if state == .capacityLimited { blocked.insert(entry.id) }
        case .prepared(let prepared):
            guard prepared.rawPath == entry.rawPath,
                  prepared.document.source.path.utf8.elementsEqual(entry.rawPath),
                  prepared.document.renderTiles.allSatisfy({
                      $0.path.utf8.elementsEqual(entry.rawPath) && $0.parcelRect == AtlasViewportIndex.rect(entry)
                  }) else { setState(entry.id, .unavailable); pump(); return }
            if admit(prepared, for: entry.id) {
                setState(entry.id, prepared.document.source.text.isEmpty ? .empty : .ready)
                publish()
            } else {
                setState(entry.id, .capacityLimited); blocked.insert(entry.id)
            }
        }
        pump()
    }

    private func admit(_ prepared: AtlasViewportPreparedSource, for id: UInt64) -> Bool {
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
