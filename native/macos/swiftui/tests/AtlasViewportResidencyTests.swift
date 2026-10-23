import Foundation
import CoreGraphics
import Dispatch

private final class ViewportFixture: @unchecked Sendable {
    private let lock = NSLock()
    private var paths: [String] = []
    private var active = 0, maximum = 0
    let firstGate: DispatchSemaphore?
    let initiallyUnavailable: Bool
    let empty: Bool
    init(blockFirst: Bool = false, initiallyUnavailable: Bool = false, empty: Bool = false) {
        firstGate = blockFirst ? DispatchSemaphore(value: 0) : nil
        self.initiallyUnavailable = initiallyUnavailable; self.empty = empty
    }
    var snapshot: (paths: [String], maximum: Int) {
        lock.lock(); defer { lock.unlock() }; return (paths, maximum)
    }
    var transport: AtlasViewportSourceTransport {
        AtlasViewportSourceTransport(load: { [self] path, _, _ in
            precondition(!Thread.isMainThread, "capture must never run on the interaction actor")
            lock.lock(); paths.append(path); let ordinal = paths.count
            active += 1; maximum = max(maximum, active); lock.unlock()
            defer { lock.lock(); active -= 1; lock.unlock() }
            if ordinal == 1, let firstGate { precondition(firstGate.wait(timeout: .now() + 5) == .success) }
            if ordinal == 1, initiallyUnavailable { return nil }
            let text = empty ? "" : "let exact_capture = \"\(path)\";\n"
            let runs: [[String: String]] = text.isEmpty ? [] : [["start": "0", "length": String(text.utf16.count), "role": "plain"]]
            return AtlasProjectSourcePacket(json: try JSONSerialization.data(withJSONObject:
                ["schema": "fcb.source-document/1", "text": text, "runs": runs]), key: nil)
        })
    }
}

private final class ViewportAccess {}
private final class ReleaseRecord: @unchecked Sendable {
    private let lock = NSLock()
    private var events: [Bool] = []
    func append(main: Bool) { lock.lock(); events.append(main); lock.unlock() }
    var snapshot: [Bool] { lock.lock(); defer { lock.unlock() }; return events }
}
private final class ReleaseProbe {
    let record: ReleaseRecord
    init(_ record: ReleaseRecord) { self.record = record }
    deinit { record.append(main: Thread.isMainThread) }
}

/// Production scheduling, CoreText preparation, fixed placement and storage
/// lifetime admission. The injected bytes are fixtures, not a source parser or
/// proof of a native window/GPU performance result.
@MainActor @main struct AtlasViewportResidencyTests {
    static func entry(_ id: UInt64, _ path: String, x: Double = 0, y: Double = 0,
        width: Double = 160, height: Double = 120, bytes: UInt64 = 32) -> AtlasProjectCatalog.Entry {
        .init(id: id, rawPath: Array(path.utf8), displayPath: path, sourcePath: path, observedBytes: bytes,
            x: x, y: y, width: width, height: height)
    }
    static func rect(_ entry: AtlasProjectCatalog.Entry) -> CGRect {
        CGRect(x: entry.x, y: entry.y, width: entry.width, height: entry.height)
    }
    static func until(_ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(5)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 1_000_000) }
        precondition(condition(), "bounded viewport test deadline")
    }
    static func ready(_ model: AtlasViewportResidency, _ entry: AtlasProjectCatalog.Entry) async throws {
        try await until { model.previewStates[entry.id] == .ready || model.previewStates[entry.id] == .empty }
        precondition(model.document(forRawPath: entry.rawPath)?.source.path.utf8.elementsEqual(entry.rawPath) == true)
    }
    static func main() async throws {
        try await coldOverviewAndLateParcel()
        try await denseRegionHasNoPrefixStarvation()
        try await newestDemandWaitsForEnteredCapture()
        try await replacedRootDrainsItsGrant()
        try await selectedProtectionCapacityRetryAndEviction()
        try await scopeRefitsRetainedRowsWithoutRecapture()
        try await refusalsRemainExplicitAndRetryable()
        try await byteIdentityAndMetadataAdmission()
        boundedShapingStopsBeforeAnotherTile()
        try await finiteRetirementCreditsAndFinalFrameOwnership()
        try await retirementBackpressureResumesLatestDemand()
        print("AtlasViewportResidency: 11 real worker/demand/placement/lifetime scenarios passed")
    }

    static func coldOverviewAndLateParcel() async throws {
        let fixture = ViewportFixture(), model = AtlasViewportResidency(transport: fixture.transport)
        // Metadata before the target exceeds the former eager 64MiB prefix.
        let files = (0..<80).map { entry(UInt64($0 + 1), "file-\($0).rs", x: Double($0) * 200, bytes: 2 * 1024 * 1024) }
        let layout = UUID()
        precondition(model.install(root: "/fixture", entries: files, layoutRevision: layout, accessLease: nil))
        precondition(model.isInstalled && model.documents.isEmpty && model.tiles.isEmpty)
        model.updateViewport(CGRect(x: 0, y: 0, width: 16_000, height: 120), scale: 0.01, selectedPath: nil)
        precondition(!model.isLoading && model.pendingCount == 0 && fixture.snapshot.paths.isEmpty,
            "a complete overview must not enqueue source for the entire catalog")
        let target = files.last!
        model.updateViewport(rect(target), scale: 4, selectedPath: nil)
        try await ready(model, target)
        precondition(fixture.snapshot.paths == ["/fixture/file-79.rs"])
        precondition(model.layoutRevision == layout && model.tiles.allSatisfy { $0.parcelRect == rect(target) })
        precondition(model.documents.count == 1 && model.residentCost.sourceBytes < 1024)
        model.close()
        try await until { model.retainedNativeSlots == 0 }
    }

    static func denseRegionHasNoPrefixStarvation() async throws {
        let fixture = ViewportFixture(), model = AtlasViewportResidency(transport: fixture.transport)
        var files = (0..<5000).map {
            entry(UInt64($0 + 1), "dense-\($0).rs", x: Double($0) / 10_000, width: 0.00008, height: 0.001)
        }
        // All tiny files shared one cell in a fixed 32x32 grid. Exact hierarchy
        // culling must reach the last file without clicking it first.
        files.append(entry(9000, "huge.rs", x: 2, width: 10_000, height: 10_000))
        precondition(model.install(root: "/fixture", entries: files, layoutRevision: UUID(), accessLease: nil))
        let target = files[4999]
        model.updateViewport(rect(target), scale: 1_000_000, selectedPath: nil)
        try await ready(model, target)
        precondition(fixture.snapshot.paths.contains("/fixture/dense-4999.rs"))
        precondition(!fixture.snapshot.paths.contains("/fixture/dense-0.rs"))
        model.close()
        try await until { model.retainedNativeSlots == 0 }
    }

    static func newestDemandWaitsForEnteredCapture() async throws {
        let fixture = ViewportFixture(blockFirst: true), model = AtlasViewportResidency(transport: fixture.transport)
        let a = entry(1, "a.rs"), b = entry(2, "b.rs", x: 1000)
        precondition(model.install(root: "/fixture", entries: [a, b], layoutRevision: UUID(), accessLease: nil))
        model.updateViewport(rect(a), scale: 4, selectedPath: nil)
        try await until { fixture.snapshot.paths.count == 1 }
        model.updateViewport(rect(b), scale: 4, selectedPath: nil)
        model.updateViewport(rect(a), scale: 4, selectedPath: nil)
        precondition(fixture.snapshot.paths == ["/fixture/a.rs"], "replacements wait for the entered capture")
        fixture.firstGate!.signal()
        try await ready(model, a)
        precondition(fixture.snapshot.paths == ["/fixture/a.rs", "/fixture/a.rs"],
            "the canceled first capture cannot publish and the intermediate demand never enters")
        precondition(fixture.snapshot.maximum == 1 && model.document(forRawPath: b.rawPath) == nil)
        model.close()
        try await until { model.retainedNativeSlots == 0 }
    }

    static func replacedRootDrainsItsGrant() async throws {
        let fixture = ViewportFixture(blockFirst: true), model = AtlasViewportResidency(transport: fixture.transport)
        let file = entry(1, "same.rs")
        var grant: ViewportAccess? = ViewportAccess()
        weak var observedGrant = grant
        precondition(model.install(root: "/old", entries: [file], layoutRevision: UUID(), accessLease: grant))
        grant = nil
        model.updateViewport(rect(file), scale: 4, selectedPath: nil)
        try await until { fixture.snapshot.paths == ["/old/same.rs"] }
        model.close()
        let nextLayout = UUID()
        precondition(model.install(root: "/new", entries: [file], layoutRevision: nextLayout, accessLease: nil))
        model.updateViewport(rect(file), scale: 4, selectedPath: nil)
        precondition(observedGrant != nil && fixture.snapshot.paths.count == 1,
            "the old root grant outlives cancellation until its worker returns")
        fixture.firstGate!.signal()
        try await ready(model, file)
        try await until { observedGrant == nil }
        precondition(fixture.snapshot.paths == ["/old/same.rs", "/new/same.rs"] && fixture.snapshot.maximum == 1)
        precondition(model.layoutRevision == nextLayout && model.root == "/new")
        precondition(model.document(forRawPath: file.rawPath)?.source.text.contains("/new/same.rs") == true)
        model.close()
        try await until { model.retainedNativeSlots == 0 }
    }

    static func selectedProtectionCapacityRetryAndEviction() async throws {
        var limits = AtlasViewportLimits(); limits.documents = 1
        let fixture = ViewportFixture(), model = AtlasViewportResidency(limits: limits, transport: fixture.transport)
        let a = entry(1, "selected.rs"), b = entry(2, "later.rs", x: 1000)
        let layout = UUID()
        precondition(model.install(root: "/fixture", entries: [a, b], layoutRevision: layout, accessLease: nil))
        model.updateViewport(rect(a), scale: 4, selectedPath: nil)
        try await ready(model, a)
        model.updateViewport(rect(b), scale: 4, selectedPath: a.sourcePath)
        try await until { model.previewStates[b.id] == .capacityLimited && !model.isLoading }
        precondition(model.document(forRawPath: a.rawPath) != nil && model.document(forRawPath: b.rawPath) == nil,
            "an offscreen selected resident is protected from optional preview eviction")
        model.updateViewport(rect(b), scale: 4, selectedPath: nil)
        try await ready(model, b)
        precondition(model.document(forRawPath: a.rawPath) == nil && model.residentCount == 1)
        precondition(fixture.snapshot.paths.filter { $0 == "/fixture/later.rs" }.count == 2,
            "capacity refusal becomes retryable after selection releases its protection")
        model.updateViewport(rect(a), scale: 4, selectedPath: nil)
        try await ready(model, a)
        precondition(model.layoutRevision == layout && model.tiles.allSatisfy { $0.parcelRect == rect(a) })
        precondition(model.document(forRawPath: b.rawPath) == nil && fixture.snapshot.maximum == 1)
        model.close()
        try await until { model.retainedNativeSlots == 0 }
    }

    static func scopeRefitsRetainedRowsWithoutRecapture() async throws {
        let fixture = ViewportFixture(), model = AtlasViewportResidency(transport: fixture.transport)
        let original = entry(1, "scope.rs"), firstLayout = UUID()
        precondition(model.install(root: "/fixture", entries: [original], layoutRevision: firstLayout, accessLease: nil))
        model.updateViewport(rect(original), scale: 4, selectedPath: nil)
        try await ready(model, original)
        var oldFrame = model.tiles
        let oldRects = oldFrame.map(\.rect), oldContent = model.contentRevision
        let moved = entry(1, "scope.rs", x: 500, y: 300, width: 320, height: 240), nextLayout = UUID()
        precondition(model.install(root: "/fixture", entries: [moved], layoutRevision: nextLayout,
            accessLease: nil, preservingCaptures: true))
        precondition(model.documents.isEmpty && model.tiles.isEmpty)
        model.updateViewport(rect(moved), scale: 4, selectedPath: nil)
        try await ready(model, moved)
        precondition(fixture.snapshot.paths == ["/fixture/scope.rs"], "a scope repack reuses captured source and shaped rows")
        precondition(model.layoutRevision == nextLayout && model.contentRevision != oldContent)
        precondition(model.tiles.allSatisfy { $0.parcelRect == rect(moved) })
        precondition(oldFrame.map(\.rect) == oldRects && oldFrame.allSatisfy { $0.parcelRect == rect(original) })
        precondition(!model.tiles.contains { current in oldFrame.contains { $0 === current } },
            "a new placement cannot mutate an older accepted frame")
        precondition(model.install(root: "/fixture", entries: [], layoutRevision: UUID(),
            accessLease: nil, preservingCaptures: true))
        precondition(model.isInstalled && model.documents.isEmpty && model.tiles.isEmpty)
        model.close()
        precondition(model.retainedNativeSlots > 0 && !oldFrame.isEmpty)
        oldFrame = []
        try await until { model.retainedNativeSlots == 0 }
    }

    static func refusalsRemainExplicitAndRetryable() async throws {
        let fixture = ViewportFixture(initiallyUnavailable: true), model = AtlasViewportResidency(transport: fixture.transport)
        let file = entry(1, "retry.rs")
        precondition(model.install(root: "/fixture", entries: [file], layoutRevision: UUID(), accessLease: nil))
        model.updateViewport(rect(file), scale: 4, selectedPath: nil)
        try await until { model.previewStates[file.id] == .unavailable && !model.isLoading }
        model.updateViewport(rect(file), scale: 5, selectedPath: nil)
        precondition(fixture.snapshot.paths.count == 1 && model.documents.isEmpty,
            "an unavailable source does not create an empty capture or an automatic retry loop")
        model.retryUnavailable()
        try await ready(model, file)
        precondition(fixture.snapshot.paths.count == 2)
        model.close()
        try await until { model.retainedNativeSlots == 0 }

        let emptyFixture = ViewportFixture(empty: true), emptyModel = AtlasViewportResidency(transport: emptyFixture.transport)
        let emptyFile = entry(2, "empty.rs", bytes: 0)
        precondition(emptyModel.install(root: "/fixture", entries: [emptyFile], layoutRevision: UUID(), accessLease: nil))
        emptyModel.updateViewport(rect(emptyFile), scale: 4, selectedPath: nil)
        try await ready(emptyModel, emptyFile)
        precondition(emptyModel.previewStates[emptyFile.id] == .empty && emptyModel.documents.count == 1 && emptyModel.tiles.isEmpty)
        precondition(emptyModel.document(forRawPath: emptyFile.rawPath)?.source.text == "")
        emptyModel.close()
        try await until { emptyModel.retainedNativeSlots == 0 }
    }

    static func byteIdentityAndMetadataAdmission() async throws {
        let fixture = ViewportFixture(), model = AtlasViewportResidency(transport: fixture.transport)
        let composed = entry(1, "é.rs"), decomposed = entry(2, "e\u{301}.rs", x: 200)
        precondition(composed.sourcePath == decomposed.sourcePath && composed.rawPath != decomposed.rawPath)
        let layout = UUID()
        precondition(model.install(root: "/fixture", entries: [composed, decomposed], layoutRevision: layout, accessLease: nil))
        model.updateViewport(CGRect(x: 0, y: 0, width: 400, height: 120), scale: 4, selectedPath: nil)
        precondition(model.previewStates[1] == .readerOnly && model.previewStates[2] == .readerOnly)
        precondition(model.documents.isEmpty && fixture.snapshot.paths.isEmpty,
            "canonically equivalent names cannot share one String-keyed source preview")
        precondition(!model.install(root: "/other", entries: [composed, entry(1, "duplicate.rs")],
            layoutRevision: UUID(), accessLease: nil))
        precondition(model.root == "/fixture" && model.layoutRevision == layout,
            "invalid replacement metadata leaves the accepted project unchanged")
        let raw = AtlasProjectCatalog.Entry(id: 3, rawPath: [0xff], displayPath: "\\xff",
            sourcePath: nil, observedBytes: 5, x: 0, y: 0, width: 120, height: 120)
        precondition(model.install(root: "/fixture", entries: [raw], layoutRevision: UUID(), accessLease: nil))
        model.updateViewport(rect(raw), scale: 4, selectedPath: nil)
        precondition(model.previewStates[3] == .readerOnly && fixture.snapshot.paths.isEmpty)
        model.close()
    }

    static func boundedShapingStopsBeforeAnotherTile() {
        func capture(_ rows: Int) -> AtlasHighlightCapture {
            let text = String(repeating: "\n", count: rows)
            return AtlasHighlightCapture(schema: "fcb.source-document/1", text: text,
                runs: rows == 0 ? [] : [.init(start: "0", length: String(rows), role: "plain")])
        }
        // Empty rows use little glyph budget but still create CTLine objects.
        // The last admitted tile must be complete, with no hidden text suffix.
        let whole = AtlasDocument(path: "boundary.rs", capture: capture(80), tileLimit: 1)
        precondition(whole?.tiles.count == 1 && whole?.tiles.first?.lineCount == 80)
        precondition(whole?.tiles.first?.sourceRange == NSRange(location: 0, length: 80))
        precondition(whole?.source.text == capture(80).text)
        precondition(AtlasDocument(path: "boundary.rs", capture: capture(81), tileLimit: 1) == nil,
            "an extra source row refuses the whole preview instead of returning the admitted prefix")
        var capPolls = 0
        let overCap = AtlasDocument(path: "over-cap.rs", capture: capture(160), tileLimit: 1, canceled: {
            capPolls += 1
            return false
        })
        precondition(overCap == nil && capPolls == 81,
            "initial admission and 80 row polls prove refusal before preparing another tile")
        precondition(AtlasDocument(path: "empty.rs", capture: capture(0), tileLimit: 0)?.tiles.isEmpty == true)
        precondition(AtlasDocument(path: "nonempty.rs", capture: capture(1), tileLimit: 0) == nil)
        precondition(AtlasDocument(path: "invalid.rs", capture: capture(0), tileLimit: -1) == nil)
        var polls = 0
        let canceled = AtlasDocument(path: "canceled.rs", capture: capture(160), tileLimit: 2, canceled: {
            polls += 1
            return polls == 5
        })
        precondition(canceled == nil && polls == 5,
            "cancellation is observed between rows before the remaining tile budget can be consumed")
    }

    static func finiteRetirementCreditsAndFinalFrameOwnership() async throws {
        let pool = AtlasNativeRetirement(maximumSlots: 2, maximumBytes: 100), record = ReleaseRecord()
        var first = pool.reserve(bytes: 80), second = pool.reserve(bytes: 20)
        precondition(first != nil && second != nil && pool.reserve(bytes: 1) == nil)
        var probe: ReleaseProbe? = ReleaseProbe(record)
        precondition(first!.publish(storage: probe!, bytes: 60)); probe = nil
        precondition(pool.reservedBytes == 80 && pool.reserve(bytes: 20) == nil,
            "byte headroom cannot bypass finite lifetime slots")
        first = nil
        precondition(record.snapshot.isEmpty, "last interaction owner never destroys the payload inline")
        try await until { record.snapshot.count == 1 && pool.reservedSlots == 1 }
        precondition(record.snapshot == [false] && pool.reservedBytes == 20)
        var third = pool.reserve(bytes: 80)
        precondition(third != nil)
        second = nil; third = nil
        try await until { pool.reservedSlots == 0 && pool.reservedBytes == 0 }

        let fixture = ViewportFixture(), model = AtlasViewportResidency(transport: fixture.transport), file = entry(1, "held.rs")
        precondition(model.install(root: "/fixture", entries: [file], layoutRevision: UUID(), accessLease: nil))
        model.updateViewport(rect(file), scale: 4, selectedPath: nil)
        try await ready(model, file)
        var frame: [AtlasTextTile]? = model.tiles
        var selected: AtlasSource? = model.document(forRawPath: file.rawPath)?.source
        precondition(frame?.isEmpty == false && selected != nil)
        model.close()
        precondition(model.retainedNativeSlots > 0 && model.retainedNativeBytes > 0)
        frame = nil
        precondition(model.retainedNativeSlots > 0, "independent selected source still owns the charged capture")
        withExtendedLifetime(selected) {}
        selected = nil
        try await until { model.retainedNativeSlots == 0 && model.retainedNativeBytes == 0 }
    }

    static func retirementBackpressureResumesLatestDemand() async throws {
        var limits = AtlasViewportLimits(); limits.documents = 1
        let fixture = ViewportFixture(), model = AtlasViewportResidency(limits: limits, transport: fixture.transport)
        let files = (0..<32).map { entry(UInt64($0 + 1), "flight-\($0).rs", x: Double($0) * 1000) }
        precondition(model.install(root: "/fixture", entries: files, layoutRevision: UUID(), accessLease: nil))
        var heldFrames: [[AtlasTextTile]] = [], blocked: AtlasProjectCatalog.Entry?
        // Retaining old accepted-frame payloads must eventually consume the
        // finite lifetime allowance even though the LRU contains only one file.
        for file in files.dropLast() {
            model.updateViewport(rect(file), scale: 4, selectedPath: nil)
            try await until { model.previewStates[file.id] == .ready
                || (model.previewStates[file.id] == .capacityLimited && !model.isLoading) }
            if model.previewStates[file.id] == .capacityLimited { blocked = file; break }
            heldFrames.append(model.tiles)
        }
        precondition(blocked != nil && !heldFrames.isEmpty && model.residentCount == 1)
        let entered = fixture.snapshot.paths.count, latest = files.last!
        model.updateViewport(rect(latest), scale: 4, selectedPath: nil)
        precondition(!model.isLoading && fixture.snapshot.paths.count == entered)
        heldFrames[0] = []
        // Returning a final frame credit wakes the newest demand; the blocked
        // old viewport is never replayed and no polling task is required.
        try await ready(model, latest)
        precondition(fixture.snapshot.paths.last == "/fixture/flight-31.rs")
        precondition(!fixture.snapshot.paths.contains("/fixture/\(blocked!.sourcePath!)"))
        precondition(fixture.snapshot.maximum == 1)
        model.close(); heldFrames = []
        try await until { model.retainedNativeSlots == 0 }
    }
}
