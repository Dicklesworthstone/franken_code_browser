import Foundation

// Production model/coordinator/queue tests with AtlasReaderRefreshFixture at
// the native transport boundary. No substitute source or document model.
@main @MainActor struct AtlasPagedReaderRefreshRegressionTests {
    typealias Fixture = AtlasReaderRefreshFixture
    static func require(_ condition: @autoclosure () -> Bool, _ message: String) {
        if !condition() { fatalError(message) }
    }
    static func wait(_ condition: () -> Bool) async throws {
        for _ in 0..<5000 {
            if condition() { return }
            try await Task.sleep(nanoseconds: 1_000_000)
        }
        fatalError("fixture did not reach its required state")
    }
    static func settle(_ model: AtlasPagedReaderModel) async throws {
        try await wait { !model.busy }
    }
    static func open(_ f: Fixture, lease: AnyObject? = nil) async throws -> AtlasPagedReaderModel {
        let model = AtlasPagedReaderModel(transport: f.transport, search: f.search)
        require(!model.canRefresh, "inert model cannot refresh")
        model.open(root: "/project", path: "notes.md", accessLease: lease)
        try await settle(model)
        require(model.page?.text == "old capture\n" && model.canRefresh, "initial capture is readable")
        return model
    }
    static func selectOldWithHistory(_ model: AtlasPagedReaderModel) async throws {
        model.offsetInput = "4"; model.goToByte(); try await settle(model)
        model.fileQuery = "old"; model.find(); try await settle(model)
        require(model.matchHex == "6f6c64" && model.nativeSelectionRange != nil && model.canGoBack,
                "positive control: old exact hit, copy and navigation history exist")
    }

    static func explicitRefreshReplacesCaptureAndRetiresOldTargets() async throws {
        let f = Fixture(), model = try await open(f)
        try await selectOldWithHistory(model)
        guard let old = model.page, let generation = model.findReport?.generation else { fatalError("missing capture") }
        let oldNavigation = model.navigation
        f.change { $0.text = "old replacement\n" }
        require(f.snapshot.opens.count == 1 && model.page?.text == "old capture\n",
                "a disk change alone does not reread retained source")
        model.refreshFromDisk()
        require(model.isRefreshing && model.busy && !model.canRefresh,
                "refresh admission is synchronous and repeated submission is disabled")
        require(model.page?.identity.matches(old.identity) == true && model.matchHex == "6f6c64",
                "old accepted page and original-byte copy remain until commit")
        try await settle(model)
        require(model.page?.text == "old replacement\n" && model.page?.identity.owner != old.identity.owner,
                "validated new capture and presentation commit together")
        require(model.page?.start == 0 && model.offsetInput == "0" && model.navigation != oldNavigation,
                "changed source starts a fresh first page, not an old byte anchor")
        require(model.findReport == nil && model.matchHex == nil && model.nativeSelectionRange == nil,
                "no old query selection or exact-copy authority survives refresh")
        require(!model.hasOutline && model.outlinePage == nil && !model.outlineRowsAuthorized,
                "outline must be built for the replacement capture")
        require(!model.canGoBack && !model.canGoForward && !model.documentPreview.authorized,
                "old capture history and preview are not reattached")
        require(model.refreshedFromDisk && model.canRefresh && !model.isRefreshing,
                "accepted origin and controls reflect the new capture")
        require(model.fileQuery == "old", "query input is retained for an explicit rerun")
        model.find(); try await settle(model)
        require((model.findReport?.generation ?? 0) > generation && model.matchHex == "6f6c64",
                "new capture can earn fresh exact-match authority")
        model.close()
        try await wait { f.snapshot.closed.count == 2 }
    }

    static func failedRefreshPreservesExactHitAndHistory() async throws {
        let f = Fixture(), model = try await open(f)
        try await selectOldWithHistory(model)
        guard let old = model.page, let generation = model.findReport?.generation else { fatalError("missing old state") }
        let navigation = model.navigation, selection = model.nativeSelectionRange
        for failure in [Fixture.Failure.none, .open, .page, .identity] {
            f.change { $0.refuseCreate = failure == .none; $0.failure = failure; $0.text = "different source\n" }
            model.refreshFromDisk(); try await settle(model)
            require(model.page?.identity.matches(old.identity) == true && model.page?.text == old.text,
                    "failed capture cannot replace visible or underlying source")
            require(model.navigation == navigation && model.nativeSelectionRange == selection,
                    "failed recapture must not reset the current native page/selection")
            require(model.findReport?.generation == generation && model.matchHex == "6f6c64" && model.canMoveHit,
                    "failed refresh preserves the accepted query and exact-copy bytes")
            require(model.canGoBack && !model.refreshedFromDisk && model.canRefresh,
                    "old history and retry remain usable")
            require(model.notice.hasPrefix("Refresh failed."), "failure distinguishes preserved source from fresh source")
        }
        f.change { $0.refuseCreate = false; $0.failure = .none }
        model.back(); try await settle(model)
        require(model.page?.start == 4 && model.page?.identity.matches(old.identity) == true,
                "history still navigates the old retained capture after failure")
        model.close()
    }

    static func canceledRefreshKeepsSourceAndRejectsDuplicateAdmission() async throws {
        let f = Fixture(), gate = Fixture.Gate(), model = try await open(f)
        guard let old = model.page else { fatalError("missing source") }
        f.change { $0.gate = gate; $0.text = "new candidate\n" }
        model.refreshFromDisk()
        try await wait { gate.didEnter }
        let admitted = f.snapshot.nextHandle
        model.refreshFromDisk()
        require(f.snapshot.nextHandle == admitted, "busy refresh does not allocate more candidates")
        model.cancel()
        require(!model.busy && !model.isRefreshing && model.canRefresh,
                "cancel restores accepted reader controls immediately")
        require(model.page?.identity.matches(old.identity) == true && !model.refreshedFromDisk,
                "cancel preserves the old page and provenance")
        let notice = model.notice
        gate.release()
        try await wait { f.snapshot.closed[2] == 1 }
        model.offsetInput = "4"; model.goToByte(); try await settle(model)
        require(model.page?.start == 4 && model.page?.identity.matches(old.identity) == true,
                "old coordinator capture stays navigable after delayed cancellation")
        require(notice.hasPrefix("Refresh canceled."), "explicit cancellation is not reported as successful refresh")
        model.close()
    }

    static func newSourceNavigationSupersedesPendingRefresh() async throws {
        let f = Fixture(), gate = Fixture.Gate(), model = try await open(f)
        guard let old = model.page else { fatalError("missing source") }
        f.change { $0.gate = gate; $0.text = "candidate\n" }
        model.refreshFromDisk(); try await wait { gate.didEnter }
        // A valid source action remains a new intent even while refresh is in
        // flight. It must not read from the unpublished candidate's handle.
        model.offsetInput = "5"; model.goToByte()
        require(!model.isRefreshing, "new navigation supersedes refresh presentation")
        gate.release(); try await settle(model)
        require(model.page?.start == 5 && model.page?.identity.matches(old.identity) == true,
                "newest action reads the old accepted capture, not candidate bytes")
        require(!model.refreshedFromDisk && model.canGoBack, "superseded refresh does not clear new history")
        model.close()
    }

    static func closeAndReopenCannotReceiveLateRefresh() async throws {
        let f = Fixture(), gate = Fixture.Gate()
        let model = try await open(f, lease: Fixture.Lease(f))
        f.change { $0.gate = gate; $0.text = "stale candidate\n" }
        model.refreshFromDisk(); try await wait { gate.didEnter }
        model.close()
        require(model.page == nil && !model.canRefresh && !model.refreshedFromDisk,
                "close retires visible capture and refresh destination")
        require(f.snapshot.releasedLeases == 0, "in-flight capture keeps the granted lease alive")
        f.change { $0.text = "replacement view\n" }
        model.open(root: "/other", path: "next.md", accessLease: nil)
        gate.release(); try await settle(model)
        require(model.page?.text == "replacement view\n"
                && model.page?.identity.pathBytes == Array("/other/next.md".utf8),
                "late refresh cannot publish into the reopened model")
        require(!model.refreshedFromDisk, "replacement open is not mislabeled as old refresh")
        try await wait { f.snapshot.releasedLeases == 1 }
        model.refreshFromDisk(); try await settle(model)
        require(model.page?.identity.pathBytes == Array("/other/next.md".utf8),
                "future refresh uses only the newly opened project/path")
        model.close()
    }

    static func shrinkEmptyCaptureAndRepeatedRefreshAreUsable() async throws {
        let f = Fixture(), model = try await open(f)
        model.offsetInput = "10"; model.goToByte(); try await settle(model)
        f.change { $0.text = "" }
        model.refreshFromDisk(); try await settle(model)
        require(model.page?.identity.capturedBytes == 0 && model.page?.text == "" && model.page?.originalHex == "",
                "a genuinely empty replacement is a valid capture, not a failure fallback")
        require(model.page?.start == 0 && !model.canGoNext && !model.canGoBack && model.canRefresh,
                "shrinking source does not reuse an out-of-range old offset")
        f.change { $0.text = "old restored\n" }
        model.refreshFromDisk(); try await settle(model)
        require(model.page?.text == "old restored\n" && model.canRefresh && model.refreshedFromDisk,
                "refresh remains usable after an empty capture")
        model.close()
        let count = f.snapshot.nextHandle
        model.refreshFromDisk()
        require(f.snapshot.nextHandle == count, "closed models never allocate refresh work")
    }

    static func main() async throws {
        try await explicitRefreshReplacesCaptureAndRetiresOldTargets()
        try await failedRefreshPreservesExactHitAndHistory()
        try await canceledRefreshKeepsSourceAndRejectsDuplicateAdmission()
        try await newSourceNavigationSupersedesPendingRefresh()
        try await closeAndReopenCannotReceiveLateRefresh()
        try await shrinkEmptyCaptureAndRepeatedRefreshAreUsable()
        print("6 native reader refresh regression cases passed")
    }
}
