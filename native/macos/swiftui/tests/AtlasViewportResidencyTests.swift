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

/// A bounded service adapter fixture, not a Markdown parser. It replays already
/// prepared rows as an independently mapped display document so these scenarios
/// exercise the production worker/cache/placement boundary without reader APIs.
private final class ViewportMarkdownFixture: @unchecked Sendable {
    enum Behavior { case display, unsupported, overBudget }
    private let lock = NSLock()
    private var calls: [(path: String, text: String, source: ObjectIdentifier)] = []
    private var active = 0, maximum = 0, canceledReturns = 0
    let firstGate: DispatchSemaphore?
    let behavior: Behavior
    init(blockFirst: Bool = false, behavior: Behavior = .display) {
        firstGate = blockFirst ? DispatchSemaphore(value: 0) : nil
        self.behavior = behavior
    }
    var snapshot: (paths: [String], texts: [String], sources: [ObjectIdentifier], maximum: Int, canceledReturns: Int) {
        lock.lock(); defer { lock.unlock() }
        return (calls.map { $0.path }, calls.map { $0.text }, calls.map { $0.source }, maximum, canceledReturns)
    }
    var transport: AtlasViewportMarkdownTransport {
        AtlasViewportMarkdownTransport(prepare: { [self] source, limits, cancellation in
            precondition(!Thread.isMainThread, "Markdown preparation must stay off the interaction actor")
            lock.lock()
            calls.append((source.source.path, source.source.text, ObjectIdentifier(source.source)))
            let ordinal = calls.count
            active += 1; maximum = max(maximum, active)
            lock.unlock()
            defer { lock.lock(); active -= 1; lock.unlock() }
            if ordinal == 1, let firstGate { precondition(firstGate.wait(timeout: .now() + 5) == .success) }
            if cancellation.isCanceled { lock.lock(); canceledReturns += 1; lock.unlock() }
            // Deliberately return a complete late candidate after cancellation:
            // the production worker/model must reject it independently.
            if case .unsupported = behavior { return nil }
            guard let header = source.tiles.first?.preparedHeader else { return nil }
            let lines = source.tiles.flatMap { $0.preparedLines ?? [] }
            guard !lines.isEmpty else { return nil }
            let rows: [AtlasTileDisplay.Row] = lines.indices.map { index in
                .init(origin: CGPoint(x: 4, y: 34 + Double(index) * AtlasTextTile.lineHeight),
                    bounds: CGRect(x: 4, y: 22 + Double(index) * AtlasTextTile.lineHeight,
                        width: AtlasTextTile.width - 8, height: AtlasTextTile.lineHeight))
            }
            let drawings: [AtlasTileDisplay.Drawing] = lines.indices.map { .text($0) }
            let regions = lines.indices.map {
                AtlasTileSourceRegion(sourceRange: lines[$0].sourceRange, rect: rows[$0].bounds)
            }
            let display = AtlasTileDisplay(height: 22 + Double(lines.count) * AtlasTextTile.lineHeight,
                rows: rows, drawings: drawings, sourceRegions: regions)
            let tile = AtlasTextTile(path: source.source.path, preparedLines: lines, header: header,
                sourceRange: NSRange(location: 0, length: source.source.text.utf16.count), display: display)
            let bytes: Int
            // The preview alone is admitted; adding the retained source,
            // glyphs and fitted display exceeds the combined resident budget.
            if case .overBudget = behavior { bytes = limits.estimatedBytes - 1 }
            else { bytes = display.estimatedBytes }
            return AtlasDocument(source: source.source, capture: source.capture, tiles: [tile],
                presentation: .markdown, presentationBytes: bytes)
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
    /// Tests drive the production pressure controller explicitly; host memory
    /// pressure must never change fixture demand or timing behind their backs.
    static func residency(limits: AtlasViewportLimits = AtlasViewportLimits(),
        transport: AtlasViewportSourceTransport = .live, markdown: AtlasViewportMarkdownTransport? = nil,
        memoryPressure: AtlasViewportMemoryPressure? = nil) -> AtlasViewportResidency {
        AtlasViewportResidency(limits: limits, transport: transport, markdown: markdown,
            memoryPressure: memoryPressure ?? AtlasViewportMemoryPressure())
    }

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
        try await readerOnlyMetadataDoesNotCrowdOutSource()
        try await stationaryViewportBackfillsUnavailableSource()
        try await newestDemandWaitsForEnteredCapture()
        try await replacedRootDrainsItsGrant()
        try await selectedProtectionCapacityRetryAndEviction()
        try await scopeRefitsRetainedRowsWithoutRecapture()
        try await refusalsRemainExplicitAndRetryable()
        try await byteIdentityAndMetadataAdmission()
        boundedShapingStopsBeforeAnotherTile()
        try await finiteRetirementCreditsAndFinalFrameOwnership()
        try await retirementBackpressureResumesLatestDemand()
        try await markdownWarmToggleCachesExactCapture()
        try await markdownRapidToggleRetainsReadySource()
        try await markdownScopeRefitPreservesMapsAndRetirement()
        try await markdownProjectReplacementRejectsOldCandidate()
        try await markdownFallbackPreservesExactSource()
        try await markdownDemandIsVisibleOnly()
        try await preparedArtifactsSurviveWarmReopen()
        try await preparedArtifactsInvalidateExactDependencies()
        try await preparedArtifactCorruptionAndCaptureMismatchFallBack()
        try await preparedArtifactsReflowAndRasterBudgetAreDistinct()
        try await preparedArtifactCancellationRetainsEnteredLease()
        try await preparedFontChangeRejectsEnteredCandidate()
        try await preparedCacheRefusalAndDecoderBudgetsPreserveSource()
        pressureRecoveryRequiresQuietIntervals()
        try await pressureTrimsBytesAndRetainsIndependentOwners()
        try await criticalPressureRejectsCanceledCaptureAfterRecovery()
        try await pressureRecoveryRestoresStationaryDemandInStages()
        try await pressureRecoveryRetriesCapacityWithUnchangedDemand()
        try await pressureRespectsHiddenClosedAndReinstalledViews()
        print("AtlasViewportResidency: 32 real worker/demand/placement/lifetime/cache/pressure scenarios passed")
    }

    static func coldOverviewAndLateParcel() async throws {
        let fixture = ViewportFixture(), model = residency(transport: fixture.transport)
        // Metadata before the target exceeds the former eager 64MiB prefix.
        let files = (0..<80).map { entry(UInt64($0 + 1), "file-\($0).rs", x: Double($0) * 200, bytes: 2 * 1024 * 1024) }
        let layout = UUID()
        precondition(!model.hasInstalledLayout(root: "/fixture", layoutRevision: layout))
        precondition(model.install(root: "/fixture", entries: files, layoutRevision: layout, accessLease: nil))
        precondition(model.isInstalled && model.documents.isEmpty && model.tiles.isEmpty)
        precondition(model.hasInstalledLayout(root: "/fixture", layoutRevision: layout),
            "Show atlas must be available before a first visible frame can request source")
        precondition(!model.hasInstalledLayout(root: "/other", layoutRevision: layout)
            && !model.hasInstalledLayout(root: "/fixture", layoutRevision: UUID()),
            "metadata from another root or layout cannot authorize the atlas route")
        model.suspend()
        precondition(model.hasInstalledLayout(root: "/fixture", layoutRevision: layout)
            && fixture.snapshot.paths.isEmpty,
            "returning from Project Files must remain possible without eagerly capturing source")
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
        precondition(!model.hasInstalledLayout(root: "/fixture", layoutRevision: layout),
            "closing the project withdraws the atlas navigation route")
        try await until { model.retainedNativeSlots == 0 }
    }

    static func denseRegionHasNoPrefixStarvation() async throws {
        let fixture = ViewportFixture(), model = residency(transport: fixture.transport)
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

    static func readerOnlyMetadataDoesNotCrowdOutSource() async throws {
        let fixture = ViewportFixture(), model = residency(transport: fixture.transport)
        // More than an entire spatial candidate window of reader-only parcels
        // near the viewport center must leave the visible source reachable.
        let oversized = UInt64(AtlasViewportLimits.sourceFileBytes) + 1
        var files = (1...5000).map { entry(UInt64($0), "asset-\($0).bin", bytes: oversized) }
        let source = entry(5001, "source.rs", x: 170)
        files.append(source)
        precondition(model.install(root: "/fixture", entries: files, layoutRevision: UUID(), accessLease: nil))
        model.updateViewport(CGRect(x: -170, y: 0, width: 500, height: 120), scale: 1, selectedPath: nil)
        try await ready(model, source)
        precondition(fixture.snapshot.paths == ["/fixture/source.rs"] && model.previewStates[1] == .readerOnly)
        model.close()
        try await until { model.retainedNativeSlots == 0 }
    }

    static func stationaryViewportBackfillsUnavailableSource() async throws {
        var limits = AtlasViewportLimits(); limits.maximumDemand = 1
        let fixture = ViewportFixture(initiallyUnavailable: true)
        let model = residency(limits: limits, transport: fixture.transport)
        let first = entry(1, "first.rs"), second = entry(2, "second.rs", x: 160)
        precondition(model.install(root: "/fixture", entries: [first, second], layoutRevision: UUID(), accessLease: nil))
        model.updateViewport(rect(first).union(rect(second)), scale: 1, selectedPath: first.sourcePath)
        try await ready(model, second)
        precondition(model.previewStates[first.id] == .unavailable && fixture.snapshot.maximum == 1)
        precondition(fixture.snapshot.paths == ["/fixture/first.rs", "/fixture/second.rs"])
        // Explicit retry restores the original selection's priority even though
        // its slot was filled and the viewport has remained stationary.
        model.retryUnavailable()
        try await ready(model, first)
        precondition(fixture.snapshot.paths == ["/fixture/first.rs", "/fixture/second.rs", "/fixture/first.rs"])
        model.close()
        try await until { model.retainedNativeSlots == 0 }
    }

    static func newestDemandWaitsForEnteredCapture() async throws {
        let fixture = ViewportFixture(blockFirst: true), model = residency(transport: fixture.transport)
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
        let fixture = ViewportFixture(blockFirst: true), model = residency(transport: fixture.transport)
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
        let fixture = ViewportFixture(), model = residency(limits: limits, transport: fixture.transport)
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
        let fixture = ViewportFixture(), model = residency(transport: fixture.transport)
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
        precondition(model.hasInstalledLayout(root: "/fixture", layoutRevision: nextLayout)
            && !model.hasInstalledLayout(root: "/fixture", layoutRevision: firstLayout),
            "a scope repack is navigable while source placement is cold, but old geometry is revoked")
        model.updateViewport(rect(moved), scale: 4, selectedPath: nil)
        try await ready(model, moved)
        precondition(fixture.snapshot.paths == ["/fixture/scope.rs"], "a scope repack reuses captured source and shaped rows")
        precondition(model.layoutRevision == nextLayout && model.contentRevision != oldContent)
        precondition(model.tiles.allSatisfy { $0.parcelRect == rect(moved) })
        precondition(oldFrame.map(\.rect) == oldRects && oldFrame.allSatisfy { $0.parcelRect == rect(original) })
        precondition(!model.tiles.contains { current in oldFrame.contains { $0 === current } },
            "a new placement cannot mutate an older accepted frame")
        let emptyLayout = UUID()
        precondition(model.install(root: "/fixture", entries: [], layoutRevision: emptyLayout,
            accessLease: nil, preservingCaptures: true))
        precondition(model.isInstalled && model.documents.isEmpty && model.tiles.isEmpty)
        precondition(model.hasInstalledLayout(root: "/fixture", layoutRevision: emptyLayout),
            "an explicitly empty file scope does not strand the user in Project Files")
        model.close()
        precondition(model.retainedNativeSlots > 0 && !oldFrame.isEmpty)
        oldFrame = []
        try await until { model.retainedNativeSlots == 0 }
    }

    static func refusalsRemainExplicitAndRetryable() async throws {
        let fixture = ViewportFixture(initiallyUnavailable: true), model = residency(transport: fixture.transport)
        let file = entry(1, "retry.rs")
        let layout = UUID()
        precondition(model.install(root: "/fixture", entries: [file], layoutRevision: layout, accessLease: nil))
        model.updateViewport(rect(file), scale: 4, selectedPath: nil)
        try await until { model.previewStates[file.id] == .unavailable && !model.isLoading }
        model.updateViewport(rect(file), scale: 5, selectedPath: nil)
        precondition(fixture.snapshot.paths.count == 1 && model.documents.isEmpty,
            "an unavailable source does not create an empty capture or an automatic retry loop")
        precondition(model.hasInstalledLayout(root: "/fixture", layoutRevision: layout),
            "source refusal must not remove navigation to its metadata parcel")
        model.retryUnavailable()
        try await ready(model, file)
        precondition(fixture.snapshot.paths.count == 2)
        model.close()
        try await until { model.retainedNativeSlots == 0 }

        let emptyFixture = ViewportFixture(empty: true), emptyModel = residency(transport: emptyFixture.transport)
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
        let fixture = ViewportFixture(), model = residency(transport: fixture.transport)
        let composed = entry(1, "é.rs"), decomposed = entry(2, "e\u{301}.rs", x: 200)
        precondition(composed.sourcePath == decomposed.sourcePath && composed.rawPath != decomposed.rawPath)
        let layout = UUID()
        precondition(model.install(root: "/fixture", entries: [composed, decomposed], layoutRevision: layout, accessLease: nil))
        model.updateViewport(CGRect(x: 0, y: 0, width: 400, height: 120), scale: 4, selectedPath: nil)
        precondition(model.previewStates[1] == .readerOnly && model.previewStates[2] == .readerOnly)
        precondition(model.documents.isEmpty && fixture.snapshot.paths.isEmpty,
            "canonically equivalent names cannot share one String-keyed source preview")
        precondition(model.hasInstalledLayout(root: "/fixture", layoutRevision: layout),
            "an entirely reader-only project still exposes its selectable metadata atlas")
        precondition(!model.install(root: "/other", entries: [composed, entry(1, "duplicate.rs")],
            layoutRevision: UUID(), accessLease: nil))
        precondition(model.root == "/fixture" && model.layoutRevision == layout,
            "invalid replacement metadata leaves the accepted project unchanged")
        let raw = AtlasProjectCatalog.Entry(id: 3, rawPath: [0xff], displayPath: "\\xff",
            sourcePath: nil, observedBytes: 5, x: 0, y: 0, width: 120, height: 120)
        precondition(model.install(root: "/fixture", entries: [raw], layoutRevision: UUID(), accessLease: nil))
        model.updateViewport(rect(raw), scale: 4, selectedPath: nil)
        precondition(model.previewStates[3] == .readerOnly && fixture.snapshot.paths.isEmpty)
        let rawRootLayout = UUID()
        precondition(model.install(root: "/cafe\u{301}", entries: [raw], layoutRevision: rawRootLayout, accessLease: nil))
        precondition(model.hasInstalledLayout(root: "/cafe\u{301}", layoutRevision: rawRootLayout)
            && !model.hasInstalledLayout(root: "/caf\u{e9}", layoutRevision: rawRootLayout),
            "canonical-equivalent root strings are not interchangeable atlas authority")
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

        let fixture = ViewportFixture(), model = residency(transport: fixture.transport), file = entry(1, "held.rs")
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
        let fixture = ViewportFixture(), model = residency(limits: limits, transport: fixture.transport)
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

    static func markdownReady(_ model: AtlasViewportResidency, _ file: AtlasProjectCatalog.Entry) async throws {
        try await until { model.previewStates[file.id] == .markdown && !model.isLoading }
        precondition(model.document(forRawPath: file.rawPath)?.presentation == .markdown)
        precondition(model.tiles.contains { $0.path.utf8.elementsEqual(file.rawPath) && $0.display != nil })
    }

    static func markdownWarmToggleCachesExactCapture() async throws {
        let source = ViewportFixture(), markdown = ViewportMarkdownFixture()
        let model = residency(transport: source.transport, markdown: markdown.transport)
        let file = entry(1, "README.md"), layout = UUID()
        precondition(model.install(root: "/fixture", entries: [file], layoutRevision: layout, accessLease: nil))
        model.updateViewport(rect(file), scale: 4, selectedPath: nil)
        try await ready(model, file)
        var original: AtlasSource? = model.document(forRawPath: file.rawPath)?.source
        let sourceCost = model.residentCost
        precondition(markdown.snapshot.paths.isEmpty && original != nil)
        model.setMarkdownPreviewEnabled(true)
        // Optional presentation work does not withdraw the accepted source.
        precondition(model.document(forRawPath: file.rawPath)?.source === original
            && model.document(forRawPath: file.rawPath)?.presentation == .source)
        try await markdownReady(model, file)
        precondition(model.document(forRawPath: file.rawPath)?.source === original
            && model.document(forRawPath: file.rawPath)?.capture.text.utf8.elementsEqual(original!.text.utf8) == true)
        precondition(model.residentCost.sourceBytes == sourceCost.sourceBytes
            && model.residentCost.estimatedBytes > sourceCost.estimatedBytes,
            "cached display storage is charged separately while retaining one exact source capture")
        model.setMarkdownPreviewEnabled(false)
        try await ready(model, file)
        precondition(model.document(forRawPath: file.rawPath)?.presentation == .source
            && model.document(forRawPath: file.rawPath)?.source === original)
        model.setMarkdownPreviewEnabled(true)
        try await markdownReady(model, file)
        precondition(source.snapshot.paths == ["/fixture/README.md"] && markdown.snapshot.paths == ["README.md"],
            "a warm enable-disable-enable must neither recapture source nor prepare Markdown twice")
        precondition(markdown.snapshot.sources == [ObjectIdentifier(original!)]
            && model.layoutRevision == layout && model.tiles.allSatisfy { $0.parcelRect == rect(file) })
        model.close(); original = nil
        try await until { model.retainedNativeSlots == 0 && model.retainedNativeBytes == 0 }
    }

    static func markdownRapidToggleRetainsReadySource() async throws {
        let source = ViewportFixture(), markdown = ViewportMarkdownFixture(blockFirst: true)
        let model = residency(transport: source.transport, markdown: markdown.transport)
        let file = entry(1, "rapid.md")
        precondition(model.install(root: "/fixture", entries: [file], layoutRevision: UUID(), accessLease: nil))
        model.updateViewport(rect(file), scale: 4, selectedPath: nil)
        try await ready(model, file)
        var original: AtlasSource? = model.document(forRawPath: file.rawPath)?.source
        var originalFrame = model.tiles
        let oldRects = originalFrame.map(\.rect)
        model.setMarkdownPreviewEnabled(true)
        try await until { markdown.snapshot.paths.count == 1 }
        model.setMarkdownPreviewEnabled(false)
        model.setMarkdownPreviewEnabled(true)
        model.setMarkdownPreviewEnabled(false)
        precondition(model.document(forRawPath: file.rawPath)?.source === original
            && model.document(forRawPath: file.rawPath)?.presentation == .source
            && originalFrame.map(\.rect) == oldRects,
            "rapid mode changes must leave accepted source/frame geometry readable")
        precondition(source.snapshot.paths.count == 1 && markdown.snapshot.paths.count == 1,
            "replacement preferences wait for the entered preparation to drain")
        markdown.firstGate!.signal()
        try await until { !model.isLoading && model.previewStates[file.id] == .ready }
        precondition(markdown.snapshot.canceledReturns == 1
            && model.document(forRawPath: file.rawPath)?.presentation == .source,
            "a late completed Markdown candidate cannot override the final Source choice")
        model.setMarkdownPreviewEnabled(true)
        try await markdownReady(model, file)
        precondition(markdown.snapshot.paths.count == 2 && markdown.snapshot.maximum == 1
            && source.snapshot.paths.count == 1 && model.document(forRawPath: file.rawPath)?.source === original)
        model.setMarkdownPreviewEnabled(false)
        try await ready(model, file)
        model.setMarkdownPreviewEnabled(true)
        try await markdownReady(model, file)
        precondition(markdown.snapshot.paths.count == 2, "the successful replacement must become the warm cache")
        model.close(); original = nil; originalFrame = []
        try await until { model.retainedNativeSlots == 0 && model.retainedNativeBytes == 0 }
    }

    static func markdownScopeRefitPreservesMapsAndRetirement() async throws {
        let source = ViewportFixture(), markdown = ViewportMarkdownFixture()
        let model = residency(transport: source.transport, markdown: markdown.transport)
        let file = entry(1, "scope.markdown"), firstLayout = UUID()
        model.setMarkdownPreviewEnabled(true)
        precondition(model.install(root: "/fixture", entries: [file], layoutRevision: firstLayout, accessLease: nil))
        model.updateViewport(rect(file), scale: 4, selectedPath: nil)
        try await markdownReady(model, file)
        var original: AtlasSource? = model.document(forRawPath: file.rawPath)?.source
        var frame = model.tiles
        let oldRects = frame.map(\.rect)
        let oldMappings = frame.map { $0.display!.sourceRegions.map(\.sourceRange) }
        let moved = entry(1, "scope.markdown", x: 400, y: 200, width: 320, height: 240), nextLayout = UUID()
        precondition(model.install(root: "/fixture", entries: [moved], layoutRevision: nextLayout,
            accessLease: nil, preservingCaptures: true))
        precondition(model.documents.isEmpty && model.tiles.isEmpty)
        model.updateViewport(rect(moved), scale: 4, selectedPath: nil)
        try await markdownReady(model, moved)
        precondition(source.snapshot.paths == ["/fixture/scope.markdown"]
            && markdown.snapshot.paths == ["scope.markdown"]
            && model.document(forRawPath: moved.rawPath)?.source === original,
            "a geometry-only scope repack must reuse the exact source and prepared document")
        precondition(model.layoutRevision == nextLayout && model.layoutRevision != firstLayout
            && model.tiles.allSatisfy { $0.parcelRect == rect(moved) })
        precondition(frame.map(\.rect) == oldRects
            && frame.map { $0.display!.sourceRegions.map(\.sourceRange) } == oldMappings
            && model.tiles.map { $0.display!.sourceRegions.map(\.sourceRange) } == oldMappings,
            "original-source mappings survive refit without mutating the old accepted frame")
        precondition(!model.tiles.contains { current in frame.contains { $0 === current } })
        model.close()
        precondition(model.residentCost == AtlasViewportCost() && model.retainedNativeBytes > 0,
            "closing the LRU cannot release storage still held by a presented Markdown frame")
        frame = []
        precondition(model.retainedNativeSlots > 0, "the independent source anchor retains its charged original capture")
        withExtendedLifetime(original) {}
        original = nil
        try await until { model.retainedNativeSlots == 0 && model.retainedNativeBytes == 0 }
    }

    static func markdownProjectReplacementRejectsOldCandidate() async throws {
        let source = ViewportFixture(), markdown = ViewportMarkdownFixture(blockFirst: true)
        let model = residency(transport: source.transport, markdown: markdown.transport)
        let file = entry(1, "same.md")
        var grant: ViewportAccess? = ViewportAccess()
        weak var observedGrant = grant
        precondition(model.install(root: "/old", entries: [file], layoutRevision: UUID(), accessLease: grant))
        grant = nil
        model.updateViewport(rect(file), scale: 4, selectedPath: nil)
        try await ready(model, file)
        var oldSource: AtlasSource? = model.document(forRawPath: file.rawPath)?.source
        model.setMarkdownPreviewEnabled(true)
        try await until { markdown.snapshot.paths.count == 1 }
        model.close()
        let replacement = UUID()
        precondition(model.install(root: "/new", entries: [file], layoutRevision: replacement, accessLease: nil))
        model.updateViewport(rect(file), scale: 4, selectedPath: nil)
        precondition(observedGrant != nil && source.snapshot.paths == ["/old/same.md"]
            && model.document(forRawPath: file.rawPath) == nil,
            "a replaced project keeps its entered worker grant but lends no source to the new root")
        markdown.firstGate!.signal()
        try await markdownReady(model, file)
        try await until { observedGrant == nil }
        precondition(model.root == "/new" && model.layoutRevision == replacement
            && model.document(forRawPath: file.rawPath)?.source.text.contains("/new/same.md") == true
            && model.document(forRawPath: file.rawPath)?.source !== oldSource)
        precondition(source.snapshot.paths == ["/old/same.md", "/new/same.md"]
            && markdown.snapshot.texts.count == 2
            && markdown.snapshot.texts[0].contains("/old/same.md")
            && markdown.snapshot.texts[1].contains("/new/same.md")
            && markdown.snapshot.sources[0] != markdown.snapshot.sources[1]
            && markdown.snapshot.canceledReturns == 1,
            "late old-root display data cannot be applied under a matching filename or slot")
        model.close(); oldSource = nil
        try await until { model.retainedNativeSlots == 0 && model.retainedNativeBytes == 0 }
    }

    static func markdownFallbackPreservesExactSource() async throws {
        for behavior in [ViewportMarkdownFixture.Behavior.unsupported, .overBudget] {
            let source = ViewportFixture(), markdown = ViewportMarkdownFixture(behavior: behavior)
            let model = residency(transport: source.transport, markdown: markdown.transport)
            let file = entry(1, "fallback.mdown")
            precondition(model.install(root: "/fixture", entries: [file], layoutRevision: UUID(), accessLease: nil))
            model.updateViewport(rect(file), scale: 4, selectedPath: nil)
            try await ready(model, file)
            var original: AtlasSource? = model.document(forRawPath: file.rawPath)?.source
            model.setMarkdownPreviewEnabled(true)
            try await until { model.previewStates[file.id] == .sourceFallback && !model.isLoading }
            precondition(model.document(forRawPath: file.rawPath)?.presentation == .source
                && model.document(forRawPath: file.rawPath)?.source === original
                && model.document(forRawPath: file.rawPath)?.capture.text.utf8.elementsEqual(original!.text.utf8) == true,
                "unsupported or oversized optional display must fall back to the accepted exact source")
            precondition(model.previewStates[file.id] != .readerOnly && model.previewStates[file.id] != .capacityLimited,
                "a renderer refusal must not hide source that already fits the budget")
            model.updateViewport(rect(file), scale: 5, selectedPath: nil)
            model.updateViewport(rect(file), scale: 6, selectedPath: nil)
            precondition(!model.isLoading && source.snapshot.paths.count == 1 && markdown.snapshot.paths.count == 1,
                "fallback is accepted state, not a redraw-driven reread/reparse loop")
            model.setMarkdownPreviewEnabled(false)
            try await ready(model, file)
            precondition(model.document(forRawPath: file.rawPath)?.source === original
                && source.snapshot.paths.count == 1 && markdown.snapshot.paths.count == 1)
            model.close(); original = nil
            try await until { model.retainedNativeSlots == 0 && model.retainedNativeBytes == 0 }
        }
    }

    static func markdownDemandIsVisibleOnly() async throws {
        let source = ViewportFixture(), markdown = ViewportMarkdownFixture()
        let model = residency(transport: source.transport, markdown: markdown.transport)
        var files = (0..<80).map { entry(UInt64($0 + 1), "doc-\($0).MD", x: Double($0) * 200) }
        let code = entry(100, "docs.md/source.rs", x: 20_000)
        files.append(code)
        precondition(model.install(root: "/fixture", entries: files, layoutRevision: UUID(), accessLease: nil))
        model.setMarkdownPreviewEnabled(true)
        model.updateViewport(CGRect(x: 0, y: 0, width: 21_000, height: 120), scale: 0.01, selectedPath: nil)
        precondition(source.snapshot.paths.isEmpty && markdown.snapshot.paths.isEmpty && !model.isLoading,
            "enabling Markdown in an overview must not eagerly prepare the project")
        let last = files[79]
        model.updateViewport(rect(last), scale: 4, selectedPath: nil)
        try await markdownReady(model, last)
        precondition(source.snapshot.paths == ["/fixture/doc-79.MD"] && markdown.snapshot.paths == ["doc-79.MD"])
        model.updateViewport(rect(code), scale: 4, selectedPath: nil)
        try await ready(model, code)
        precondition(model.document(forRawPath: code.rawPath)?.presentation == .source
            && markdown.snapshot.paths == ["doc-79.MD"], "a Markdown directory name must not classify source files as Markdown")
        model.setMarkdownPreviewEnabled(false)
        model.setMarkdownPreviewEnabled(true)
        precondition(markdown.snapshot.paths == ["doc-79.MD"] && source.snapshot.paths.count == 2,
            "offscreen residents must not trigger global preference preparation")
        model.updateViewport(rect(last), scale: 4, selectedPath: nil)
        try await markdownReady(model, last)
        precondition(markdown.snapshot.paths.count == 1 && source.snapshot.paths.count == 2,
            "returning to the retained Markdown capture must reuse its warm display")
        model.close()
        try await until { model.retainedNativeSlots == 0 && model.retainedNativeBytes == 0 }
    }

    static func pressureRecoveryRequiresQuietIntervals() {
        let monitor = AtlasViewportMemoryPressure(), quiet = AtlasViewportMemoryPressure.quietInterval
        monitor.receive([.normal, .warning, .critical], at: 100)
        precondition(monitor.state.phase == .critical && monitor.state.selectedOnly,
            "the most severe coalesced system event wins")
        monitor.receive(.normal, at: 200)
        monitor.receive(.normal, at: 201)
        monitor.advanceRecovery(at: 200 + quiet - 1)
        precondition(monitor.state.phase == .recovery && monitor.state.selectedOnly,
            "normal signals cannot bypass or restart the first quiet interval")
        monitor.advanceRecovery(at: 200 + quiet)
        precondition(monitor.state.phase == .recovery && !monitor.state.selectedOnly)
        monitor.receive(.warning, at: 300 + quiet)
        monitor.advanceRecovery(at: 200 + quiet * 2)
        precondition(monitor.state.phase == .constrained,
            "a timer from an older recovery cannot undo renewed pressure")
        monitor.receive(.normal, at: 400 + quiet * 2)
        monitor.advanceRecovery(at: 400 + quiet * 3)
        precondition(monitor.state == .normal)
        monitor.receive(.critical, at: UInt64.max - 2)
        monitor.receive(.normal, at: UInt64.max - 1)
        monitor.advanceRecovery(at: UInt64.max)
        precondition(monitor.state.phase == .recovery && monitor.state.selectedOnly,
            "monotonic deadline overflow remains conservative")
    }

    static func pressureTrimsBytesAndRetainsIndependentOwners() async throws {
        let monitor = AtlasViewportMemoryPressure(), fixture = ViewportFixture()
        var limits = AtlasViewportLimits()
        let oneSource = "let exact_capture = \"/fixture/aa.rs\";\n".utf8.count
        limits.sourceBytes = oneSource * 3
        let model = residency(limits: limits, transport: fixture.transport, memoryPressure: monitor)
        let a = entry(1, "aa.rs"), b = entry(2, "bb.rs", x: 200), c = entry(3, "cc.rs", x: 400)
        let layout = UUID(), viewport = rect(a).union(rect(c))
        precondition(model.install(root: "/fixture", entries: [a, b, c], layoutRevision: layout, accessLease: nil))
        model.updateViewport(viewport, scale: 4, selectedPath: a.sourcePath)
        try await until { model.residentCount == 3 && !model.isLoading }
        var selected: AtlasSource? = model.document(forRawPath: a.rawPath)?.source
        var pinned: AtlasSource? = model.document(forRawPath: b.rawPath)?.source
        var frame = model.tiles.filter { $0.path.utf8.elementsEqual(b.rawPath) }
        let frameRects = frame.map(\.rect), capturedPaths = fixture.snapshot.paths
        precondition(!frame.isEmpty && selected != nil && pinned != nil)
        precondition(model.residentCost.sourceBytes == limits.sourceBytes)
        monitor.receive(.warning, at: 1)
        precondition(model.residentCount == 1 && model.residentCost.sourceBytes <= limits.sourceBytes / 2,
            "warning trims resident byte cost even below its eight-document ceiling")
        precondition(model.document(forRawPath: a.rawPath)?.source === selected
            && model.document(forRawPath: b.rawPath) == nil && model.layoutRevision == layout)
        precondition(frame.map(\.rect) == frameRects && pinned?.text.contains("/fixture/bb.rs") == true,
            "eviction does not mutate independently retained frame or reader captures")
        precondition(fixture.snapshot.paths == capturedPaths && !model.isLoading,
            "idle pressure trimming must not immediately recapture its evicted victims")
        monitor.receive(.critical, at: 2)
        precondition(model.residentCount == 1 && model.document(forRawPath: a.rawPath)?.source === selected)
        model.close(); selected = nil
        precondition(model.retainedNativeSlots > 0 && model.retainedNativeBytes > 0)
        frame = []
        precondition(model.retainedNativeSlots > 0, "the independent source still owns its retirement credit")
        withExtendedLifetime(pinned) {}
        pinned = nil
        try await until { model.retainedNativeSlots == 0 && model.retainedNativeBytes == 0 }
    }

    static func criticalPressureRejectsCanceledCaptureAfterRecovery() async throws {
        let monitor = AtlasViewportMemoryPressure(), fixture = ViewportFixture(blockFirst: true)
        let model = residency(transport: fixture.transport, memoryPressure: monitor)
        let a = entry(1, "first.rs"), b = entry(2, "later.rs", x: 200)
        let viewport = rect(a).union(rect(b)), quiet = AtlasViewportMemoryPressure.quietInterval
        precondition(model.install(root: "/fixture", entries: [a, b], layoutRevision: UUID(), accessLease: nil))
        model.updateViewport(viewport, scale: 4, selectedPath: nil)
        try await until { fixture.snapshot.paths == ["/fixture/first.rs"] }
        monitor.receive(.critical, at: 1)
        model.updateViewport(viewport, scale: 4, selectedPath: b.sourcePath)
        monitor.receive(.normal, at: 2)
        monitor.advanceRecovery(at: 2 + quiet)
        monitor.advanceRecovery(at: 2 + quiet * 2)
        precondition(fixture.snapshot.paths.count == 1 && model.documents.isEmpty,
            "pressure and recovery cannot overlap a second worker with entered foreign capture")
        fixture.firstGate!.signal()
        try await until { model.previewStates[a.id] == .ready && model.previewStates[b.id] == .ready && !model.isLoading }
        precondition(fixture.snapshot.paths == ["/fixture/first.rs", "/fixture/later.rs", "/fixture/first.rs"],
            "a canceled result stays rejected when recovery wants that same file again")
        precondition(fixture.snapshot.maximum == 1)
        model.close()
        try await until { model.retainedNativeSlots == 0 }
    }

    static func pressureRecoveryRestoresStationaryDemandInStages() async throws {
        let monitor = AtlasViewportMemoryPressure(), fixture = ViewportFixture()
        var limits = AtlasViewportLimits(); limits.pixels = 16 * 1024 * 1024
        let model = residency(limits: limits, transport: fixture.transport, memoryPressure: monitor)
        let files = (0..<12).map { entry(UInt64($0 + 1), "recover-\($0).rs", x: Double($0) * 200) }
        let layout = UUID(), quiet = AtlasViewportMemoryPressure.quietInterval
        monitor.receive(.critical, at: 1)
        precondition(model.install(root: "/fixture", entries: files, layoutRevision: layout, accessLease: nil))
        model.updateViewport(rect(files.first!).union(rect(files.last!)), scale: 4, selectedPath: files[0].sourcePath)
        try await ready(model, files[0])
        let selected = ObjectIdentifier(model.document(forRawPath: files[0].rawPath)!.source)
        precondition(model.residentCount == 1 && fixture.snapshot.paths.count == 1,
            "newly installed views inherit existing critical pressure")
        monitor.receive(.normal, at: 100)
        monitor.advanceRecovery(at: 100 + quiet - 1)
        precondition(model.residentCount == 1 && fixture.snapshot.paths.count == 1)
        monitor.advanceRecovery(at: 100 + quiet)
        try await until { model.residentCount == 8 && !model.isLoading }
        precondition(model.memoryPressure.phase == .recovery && fixture.snapshot.paths.count == 8)
        monitor.advanceRecovery(at: 100 + quiet * 2 - 1)
        precondition(model.residentCount == 8 && !model.isLoading)
        monitor.advanceRecovery(at: 100 + quiet * 2)
        try await until { model.residentCount == 12 && !model.isLoading }
        precondition(model.memoryPressure == .normal && fixture.snapshot.paths.count == 12)
        precondition(model.layoutRevision == layout
            && ObjectIdentifier(model.document(forRawPath: files[0].rawPath)!.source) == selected,
            "stationary recovery expands demand without reparsing the selected capture or moving parcels")
        model.close()
        try await until { model.retainedNativeSlots == 0 }
    }

    static func pressureRecoveryRetriesCapacityWithUnchangedDemand() async throws {
        let monitor = AtlasViewportMemoryPressure(), fixture = ViewportFixture()
        var limits = AtlasViewportLimits()
        limits.sourceBytes = "let exact_capture = \"/fixture/budget.rs\";\n".utf8.count
        let model = residency(limits: limits, transport: fixture.transport, memoryPressure: monitor)
        let file = entry(1, "budget.rs"), quiet = AtlasViewportMemoryPressure.quietInterval
        monitor.receive(.warning, at: 1)
        precondition(model.install(root: "/fixture", entries: [file], layoutRevision: UUID(), accessLease: nil))
        model.updateViewport(rect(file), scale: 4, selectedPath: nil)
        try await until { model.previewStates[file.id] == .capacityLimited && !model.isLoading }
        precondition(model.residentCount == 0 && fixture.snapshot.paths.count == 1)
        monitor.receive(.normal, at: 2)
        precondition(model.previewStates[file.id] == .capacityLimited && !model.isLoading,
            "entering recovery with unchanged caps must not recapture a refused preview")
        monitor.advanceRecovery(at: 2 + quiet)
        try await ready(model, file)
        precondition(fixture.snapshot.paths.count == 2,
            "the unchanged visible demand retries once its original byte budget returns")
        monitor.receive(.warning, at: 3 + quiet)
        precondition(model.previewStates[file.id] == .capacityLimited && model.residentCount == 0)
        model.updateViewport(rect(file), scale: 4, selectedPath: file.sourcePath)
        try await ready(model, file)
        precondition(model.residentCost.sourceBytes == limits.sourceBytes
            && fixture.snapshot.paths.count == 3 && model.memoryPressure.phase == .constrained,
            "newly selected source may exceed the reduced soft cap while respecting the original hard limit")
        model.close()
        try await until { model.retainedNativeSlots == 0 }
    }

    static func pressureRespectsHiddenClosedAndReinstalledViews() async throws {
        let monitor = AtlasViewportMemoryPressure(), fixture = ViewportFixture()
        let model = residency(transport: fixture.transport, memoryPressure: monitor)
        let file = entry(1, "hidden.rs"), quiet = AtlasViewportMemoryPressure.quietInterval
        precondition(model.install(root: "/fixture", entries: [file], layoutRevision: UUID(), accessLease: nil))
        model.updateViewport(rect(file), scale: 4, selectedPath: nil)
        try await ready(model, file)
        model.suspend()
        monitor.receive(.critical, at: 1)
        precondition(model.isSuspended && model.residentCount == 0 && !model.isLoading)

        let otherFixture = ViewportFixture(), other = residency(transport: otherFixture.transport, memoryPressure: monitor)
        precondition(other.install(root: "/fixture", entries: [file], layoutRevision: UUID(), accessLease: nil))
        other.updateViewport(rect(file), scale: 4, selectedPath: nil)
        precondition(other.memoryPressure.phase == .critical && otherFixture.snapshot.paths.isEmpty)
        other.updateViewport(rect(file), scale: 4, selectedPath: file.sourcePath)
        try await ready(other, file)
        var pinned: AtlasSource? = other.document(forRawPath: file.rawPath)?.source
        let moved = entry(1, "hidden.rs", x: 500)
        precondition(other.install(root: "/fixture", entries: [moved], layoutRevision: UUID(),
            accessLease: nil, preservingCaptures: true))
        precondition(other.memoryPressure.phase == .critical && other.residentCount == 0 && other.documents.isEmpty,
            "scope replacement cannot silently retain optional previews during existing critical pressure")
        precondition(pinned?.text.contains("/fixture/hidden.rs") == true)
        other.close()

        let refusedFixture = ViewportFixture(initiallyUnavailable: true)
        let refused = residency(transport: refusedFixture.transport, memoryPressure: monitor)
        precondition(refused.install(root: "/fixture", entries: [file], layoutRevision: UUID(), accessLease: nil))
        refused.updateViewport(rect(file), scale: 4, selectedPath: file.sourcePath)
        try await until { refused.previewStates[file.id] == .unavailable && !refused.isLoading }
        monitor.receive(.normal, at: 2)
        monitor.advanceRecovery(at: 2 + quiet)
        monitor.advanceRecovery(at: 2 + quiet * 2)
        precondition(model.isSuspended && !model.isLoading && model.pendingCount == 0 && fixture.snapshot.paths.count == 1)
        precondition(!other.isInstalled && !other.isLoading && otherFixture.snapshot.paths.count == 1,
            "normal pressure does not reopen closed views or resume hidden demand")
        precondition(refused.previewStates[file.id] == .unavailable && refusedFixture.snapshot.paths.count == 1,
            "memory recovery does not retry explicit unavailable source")
        model.updateViewport(rect(file), scale: 4, selectedPath: nil)
        try await ready(model, file)
        precondition(!model.isSuspended && fixture.snapshot.paths.count == 2)
        model.close(); refused.close(); pinned = nil
        try await until { model.retainedNativeSlots == 0 && other.retainedNativeSlots == 0 && refused.retainedNativeSlots == 0 }

        var transient: AtlasViewportResidency? = residency(transport: ViewportFixture().transport, memoryPressure: monitor)
        weak var observed = transient
        precondition(transient!.install(root: "/fixture", entries: [], layoutRevision: UUID(), accessLease: nil))
        transient = nil
        precondition(observed == nil, "the shared monitor cannot retain abandoned window histories")
        monitor.receive(.warning, at: 3 + quiet * 2)
    }

}

