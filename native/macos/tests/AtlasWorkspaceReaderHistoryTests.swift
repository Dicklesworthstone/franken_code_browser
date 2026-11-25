import Foundation

/// Production history/model/coordinator/queue/decoder tests with injected native
/// marshaling. This does not qualify Rust engines, AppKit, or native grants.
@main @MainActor struct AtlasWorkspaceReaderHistoryTests {
    static func require(_ condition: @autoclosure () -> Bool, _ message: String) {
        if !condition() { fatalError(message) }
    }
    static func wait(_ condition: () -> Bool) async throws {
        for _ in 0..<5000 {
            if condition() { return }
            try await Task.sleep(nanoseconds: 1_000_000)
        }
        fatalError("workspace reader history did not reach the required state")
    }
    static func open(_ history: AtlasWorkspaceReaderHistory, _ fixture: AtlasReaderRefreshFixture,
                     path: String, text: String = "old capture\n", root: String = "/project",
                     access: AnyObject? = nil, line: UInt64? = nil) async throws -> AtlasWorkspaceReaderCapture {
        fixture.change { $0.text = text }
        history.open(root: root, path: path, accessLease: access, initialLine: line) {
            AtlasPagedReaderModel(transport: fixture.transport, search: fixture.search)
        }
        try await wait { !history.busy }
        guard let capture = history.current, capture.path?.utf8.elementsEqual(path.utf8) == true else {
            fatalError("the requested source was not accepted")
        }
        return capture
    }

    static func backAndForwardKeepExactReaderAndFindState() async throws {
        let fixture = AtlasReaderRefreshFixture(), history = AtlasWorkspaceReaderHistory()
        require(history.current == nil && !history.busy && fixture.snapshot.opens.isEmpty,
                "construction performs no file work")
        let first = try await open(history, fixture, path: "first.rs", text: "old capture\nsecond line\n")
        first.reader.fileQuery = "old"; first.reader.find()
        try await wait { !first.reader.busy }
        let query = first.reader.findReport?.generation
        require(query != nil && first.reader.findIsCurrent, "the fixture must exercise real Find state")
        first.reader.offsetInput = "4"; first.reader.goToByte()
        try await wait { !first.reader.busy }
        let page = first.reader.page, navigation = first.reader.navigation
        let second = try await open(history, fixture, path: "second.rs", text: "new source\n")
        let opens = fixture.snapshot.opens, windows = fixture.snapshot.windows
        history.back()
        require(history.current === first && first.reader.page?.identity == page?.identity
                && first.reader.page?.start == 4 && first.reader.navigation == navigation,
                "Back restores the same retained model, page and navigation identity")
        require(first.reader.fileQuery == "old" && first.reader.findReport?.generation == query
                && first.reader.findIsCurrent && first.reader.canGoBack,
                "cross-file navigation preserves Find and within-file page history")
        history.forward()
        require(history.current === second && fixture.snapshot.opens == opens && fixture.snapshot.windows == windows,
                "Source Back/Forward must not reopen paths or replay page requests")
        history.catalogRefreshed()
        history.back()
        require(history.current === first && first.reader.page?.text == page?.text,
                "a same-root discovery refresh retains exact old source bytes")
        history.close()
        try await wait { fixture.snapshot.closed.count == 2 }
    }

    static func failedDestinationPreservesCurrentAndForward() async throws {
        let fixture = AtlasReaderRefreshFixture(), history = AtlasWorkspaceReaderHistory()
        let first = try await open(history, fixture, path: "a.rs")
        let second = try await open(history, fixture, path: "b.rs")
        history.back()
        for failure in [AtlasReaderRefreshFixture.Failure.open, .page, .identity] {
            fixture.change { $0.failure = failure }
            history.open(root: "/project", path: "refused.rs", accessLease: nil) {
                AtlasPagedReaderModel(transport: fixture.transport)
            }
            try await wait { !history.busy }
            require(history.current === first && history.canForward && history.count == 2
                    && first.reader.page != nil && second.reader.page != nil,
                    "a failed capture/page/identity cannot discard the active source or Forward branch")
        }
        fixture.change { $0.failure = .none; $0.refuseCreate = true }
        history.open(root: "/project", path: "pressure.rs", accessLease: nil) {
            AtlasPagedReaderModel(transport: fixture.transport)
        }
        try await wait { !history.busy }
        require(history.current === first && history.canForward && history.notice.contains("unchanged"),
                "native reader admission refusal preserves the accepted history")
        fixture.change { $0.refuseCreate = false }
        let branch = try await open(history, fixture, path: "branch.rs")
        require(history.current === branch && !history.canForward && history.count == 2
                && second.reader.page == nil && first.reader.page != nil,
                "only an accepted destination retires the abandoned Forward branch")
        history.close()
    }

    static func canceledOpeningDrainsBeforeNewestRequestIsCreated() async throws {
        let fixture = AtlasReaderRefreshFixture(), history = AtlasWorkspaceReaderHistory()
        let kept = try await open(history, fixture, path: "kept.rs")
        let gate = AtlasReaderRefreshFixture.Gate()
        fixture.change { $0.gate = gate }
        var factories = 0
        history.open(root: "/project", path: "slow.rs", accessLease: nil) {
            factories += 1
            return AtlasPagedReaderModel(transport: fixture.transport)
        }
        try await wait { gate.didEnter }
        history.open(root: "/project", path: "superseded.rs", accessLease: nil) {
            factories += 1
            return AtlasPagedReaderModel(transport: fixture.transport)
        }
        history.open(root: "/project", path: "latest.rs", accessLease: nil) {
            factories += 1
            return AtlasPagedReaderModel(transport: fixture.transport)
        }
        require(history.busy && history.canceling && history.current === kept && factories == 1
                && fixture.snapshot.opens.count == 2,
                "rapid replacement retains one entered call and one unopened newest request")
        gate.release()
        try await wait { !history.busy }
        require(history.current?.path == "latest.rs" && history.count == 2 && factories == 2,
                "only the newest waiting request is created after drain")
        require(!fixture.snapshot.captures.values.contains(where: { $0.path.hasSuffix("/superseded.rs") }),
                "coalescing must happen before native reader admission")
        try await wait { fixture.snapshot.closed[2] == 1 }
        let accepted = history.current
        let canceledGate = AtlasReaderRefreshFixture.Gate()
        fixture.change { $0.gate = canceledGate }
        history.open(root: "/project", path: "cancel.rs", accessLease: nil) {
            AtlasPagedReaderModel(transport: fixture.transport)
        }
        try await wait { canceledGate.didEnter }
        history.cancel()
        require(history.busy && history.current === accepted, "Cancel retains admission while native work is entered")
        canceledGate.release()
        try await wait { !history.busy }
        require(history.current === accepted && history.count == 2, "late canceled success cannot replace the current source")
        history.close()
    }

    static func rootAndGrantReplacementRevokeHistoryAndPendingReplies() async throws {
        let fixture = AtlasReaderRefreshFixture(), history = AtlasWorkspaceReaderHistory()
        let grant = NSObject(), replacement = NSObject()
        let composed = "/r\u{e9}po", decomposed = "/re\u{301}po"
        require(composed == decomposed && !composed.utf8.elementsEqual(decomposed.utf8),
                "fixture must distinguish Swift canonical equality from Unix bytes")
        let old = try await open(history, fixture, path: "same.rs", root: composed, access: grant)
        let gate = AtlasReaderRefreshFixture.Gate()
        fixture.change { $0.gate = gate }
        history.open(root: composed, path: "pending.rs", accessLease: grant) {
            AtlasPagedReaderModel(transport: fixture.transport)
        }
        try await wait { gate.didEnter }
        var replacements = 0
        history.open(root: decomposed, path: "next.rs", accessLease: grant) {
            replacements += 1
            return AtlasPagedReaderModel(transport: fixture.transport)
        }
        require(history.current == nil && old.reader.page == nil && history.busy && replacements == 0,
                "byte-distinct root invalidates accepted owners and retains the old draining slot")
        gate.release()
        try await wait { !history.busy }
        require(history.current?.root.utf8.elementsEqual(decomposed.utf8) == true
                && history.current?.path == "next.rs" && history.count == 1 && replacements == 1,
                "an old-root reply cannot populate the replacement history")
        let next = history.current
        require(!history.bind(root: decomposed, accessIdentity: ObjectIdentifier(replacement)),
                "a same-path replacement grant revokes retained history")
        require(history.current == nil && next?.reader.page == nil, "grant replacement closes the old source")
        history.close()
    }

    static func acceptedHistoryIsBoundedAndPreparedOwnersTransferOnce() async throws {
        let fixture = AtlasReaderRefreshFixture(), history = AtlasWorkspaceReaderHistory()
        let first = try await open(history, fixture, path: "same.rs", text: "first bytes\n")
        let second = try await open(history, fixture, path: "second.rs")
        let third = try await open(history, fixture, path: "third.rs")
        let fourth = try await open(history, fixture, path: "same.rs", text: "replacement bytes\n")
        require(history.count == 3 && first.reader.page == nil && history.current === fourth,
                "the fourth accepted visit releases only the oldest capture")
        require(fourth !== first && fourth.reader.page?.text == "replacement bytes\n",
                "equal paths do not alias distinct source revisions")
        history.back(); history.back(); history.back()
        require(history.current === second && !history.canBack && third.reader.page != nil,
                "expired history is never silently reopened")
        let prepared = AtlasPagedReaderModel(transport: fixture.transport)
        try await prepared.prepareOpen(root: "/project", path: "quick.rs", accessLease: nil)
        let opens = fixture.snapshot.opens.count
        history.adopt(root: "/project", relativePath: Array("quick.rs".utf8), label: "quick.rs",
            accessLease: nil, reader: prepared)
        try await wait { !history.busy }
        require(history.current?.reader === prepared && fixture.snapshot.opens.count == opens,
                "Quick Open transfers its accepted owner without rereading the filename")
        history.adopt(root: "/project", relativePath: Array("quick.rs".utf8), label: "quick.rs",
            accessLease: nil, reader: prepared)
        require(prepared.page != nil && !history.busy, "re-adopting an already owned reader cannot close it")
        let abandoned = AtlasPagedReaderModel(transport: fixture.transport)
        try await abandoned.prepareOpen(root: "/project", path: "abandoned.rs", accessLease: nil)
        history.adopt(root: "/project", relativePath: Array("abandoned.rs".utf8), label: "abandoned.rs",
            accessLease: nil, reader: abandoned)
        history.cancel() // Cancel before the main-actor Task can enter its closure.
        try await wait { !history.busy }
        require(abandoned.page == nil && history.current?.reader === prepared,
                "a prepared candidate canceled before execution is explicitly retired")
        history.close()
    }

    static func queuedPreparedSourcesRetireOnReplacementAndClose() async throws {
        let fixture = AtlasReaderRefreshFixture(), history = AtlasWorkspaceReaderHistory()
        let kept = try await open(history, fixture, path: "kept.rs")
        let discarded = AtlasPagedReaderModel(transport: fixture.transport)
        let dismissed = AtlasPagedReaderModel(transport: fixture.transport)
        try await discarded.prepareOpen(root: "/project", path: "discarded.rs", accessLease: nil)
        try await dismissed.prepareOpen(root: "/project", path: "dismissed.rs", accessLease: nil)
        let gate = AtlasReaderRefreshFixture.Gate()
        fixture.change { $0.gate = gate }
        history.open(root: "/project", path: "entered.rs", accessLease: nil) {
            AtlasPagedReaderModel(transport: fixture.transport)
        }
        try await wait { gate.didEnter }
        history.adopt(root: "/project", relativePath: Array("discarded.rs".utf8), label: "discarded.rs",
            accessLease: nil, reader: discarded)
        history.adopt(root: "/project", relativePath: Array("dismissed.rs".utf8), label: "dismissed.rs",
            accessLease: nil, reader: dismissed)
        require(discarded.page == nil && dismissed.page != nil && history.current === kept && history.busy,
                "replacing a waiting prepared reader retires it without disturbing entered work")
        history.close()
        require(dismissed.page == nil && kept.reader.page == nil && history.current == nil && history.busy,
                "window/root close retires accepted and waiting prepared owners while the entered slot drains")
        gate.release()
        try await wait { !history.busy }
        require(history.current == nil && history.notice.isEmpty,
                "late completion cannot reopen the closed workspace")
        try await wait { fixture.snapshot.closed.count == 4 }
    }

    static func initialLineIsPreparedOnlyOnce() async throws {
        let fixture = AtlasReaderRefreshFixture(), history = AtlasWorkspaceReaderHistory()
        let first = try await open(history, fixture, path: "startup.rs", line: 27)
        require(first.reader.notice.contains("Could not open requested line 27") && first.reader.page != nil,
                "unsupported initial line resolution keeps the explicit fallback notice and captured page")
        first.reader.offsetInput = "4"; first.reader.goToByte()
        try await wait { !first.reader.busy }
        _ = try await open(history, fixture, path: "other.rs")
        let opens = fixture.snapshot.opens, windows = fixture.snapshot.windows
        history.back()
        require(history.current === first && first.reader.page?.start == 4
                && fixture.snapshot.opens == opens && fixture.snapshot.windows == windows,
                "restoring history cannot replay the CLI startup request or reset its page")
        history.close()
    }

    static func rawSearchCapturesKeepIdentityWithoutLiveReopening() async throws {
        let fixture = AtlasReaderRefreshFixture(), history = AtlasWorkspaceReaderHistory()
        let first = try await open(history, fixture, path: "first.rs")
        let raw: [UInt8] = [115, 114, 99, 47, 255, 46, 114, 115]
        let label = "src/\\xff.rs"
        let target = AtlasSearchCapturedHit(root: "/project", path: label, needle: "old",
            start: 0, end: 3, capturedBytes: UInt64("old capture\n".utf8.count),
            openReader: { handle in
                guard let json = fixture.importCapture(handle, rawPath: raw, label: label) else {
                    throw AtlasReaderError.unavailable
                }
                return json
            }, rawPath: raw)
        history.open(root: "/project", path: label, accessLease: nil, captured: target) {
            AtlasPagedReaderModel(transport: fixture.transport, search: fixture.search, captured: target)
        }
        try await wait { !history.busy }
        guard let native = history.current else { fatalError("raw search capture was not accepted") }
        require(native !== first && native.relativePath == raw && native.path == nil
                && native.reader.page?.identity.pathBytes == raw,
                "history and the reader preserve native bytes instead of the escaped label")
        require(native.reader.selectedHitIndex == 0 && native.reader.findIsCurrent
                && native.reader.page?.text == "old capture\n",
                "the exact search occurrence is selected in the independently retained reader")
        require(!native.reader.canRefresh && fixture.snapshot.opens.count == 1
                && fixture.snapshot.imports.count == 1,
                "a raw search transfer neither opens nor authorizes refreshing an escaped pathname")
        native.reader.refreshFromDisk()
        require(fixture.snapshot.opens.count == 1 && !native.reader.busy,
                "Refresh from disk remains unavailable without an exact UTF-8 destination")
        let windows = fixture.snapshot.windows
        history.back(); history.forward()
        require(history.current === native && fixture.snapshot.windows == windows
                && fixture.snapshot.opens.count == 1,
                "Back/Forward restores the same raw-name reader without new I/O")

        var wrongTarget = target
        wrongTarget.rawPath = Array(label.utf8)
        let mismatched = wrongTarget
        history.open(root: "/project", path: label, accessLease: nil, captured: mismatched) {
            AtlasPagedReaderModel(transport: fixture.transport, search: fixture.search, captured: mismatched)
        }
        try await wait { !history.busy }
        require(history.current === native && history.count == 2 && fixture.snapshot.windows == windows,
                "a display-label identity cannot accept the original raw-name receipt or issue a source window")

        let gate = AtlasReaderRefreshFixture.Gate()
        fixture.change { $0.gate = gate }
        history.open(root: "/project", path: label, accessLease: nil, captured: target) {
            AtlasPagedReaderModel(transport: fixture.transport, search: fixture.search, captured: target)
        }
        try await wait { gate.didEnter }
        history.close()
        require(history.current == nil && native.reader.page == nil && history.busy,
                "closing the root revokes raw search history while entered transfer drains")
        gate.release()
        try await wait { !history.busy }
        require(history.current == nil && fixture.snapshot.opens.count == 1,
                "a late raw-name transfer cannot revive a closed root or fall back to disk")
        try await wait { fixture.snapshot.closed.count == 4 }
    }

    static func main() async throws {
        try await backAndForwardKeepExactReaderAndFindState()
        try await failedDestinationPreservesCurrentAndForward()
        try await canceledOpeningDrainsBeforeNewestRequestIsCreated()
        try await rootAndGrantReplacementRevokeHistoryAndPendingReplies()
        try await acceptedHistoryIsBoundedAndPreparedOwnersTransferOnce()
        try await queuedPreparedSourcesRetireOnReplacementAndClose()
        try await initialLineIsPreparedOnlyOnce()
        try await rawSearchCapturesKeepIdentityWithoutLiveReopening()
        print("8 workspace source history regression cases passed")
    }
}
