import Foundation

// Run with the real coordinator, queue, wire decoders and refresh fixture.
// These tests exercise the native host contract, not physical-Mac/Rust I/O.
@main @MainActor struct AtlasReaderReplacementRegressionTests {
    typealias Fixture = AtlasReaderRefreshFixture
    static func require(_ condition: @autoclosure () -> Bool, _ message: String) {
        if !condition() { fatalError(message) }
    }
    static func input() throws -> AtlasReaderInput { try .init(root: "/project", path: "notes.md") }
    static func wait(_ condition: () -> Bool) async throws {
        for _ in 0..<5000 {
            if condition() { return }
            try await Task.sleep(nanoseconds: 1_000_000)
        }
        fatalError("fixture did not reach its required terminal state")
    }
    static func rejected(_ work: () async throws -> Void) async {
        do { try await work(); fatalError("operation unexpectedly succeeded") }
        catch { require(error is AtlasReaderError || error is CancellationError, "typed failure") }
    }
    static func rejectsCommit(_ reader: AtlasReaderCoordinator, _ token: AtlasReaderReplacement) {
        do { _ = try reader.commitOpen(token); fatalError("stale or foreign capture committed") }
        catch { require(error is AtlasReaderError, "typed commit refusal") }
    }
    static func preserved(_ reader: AtlasReaderCoordinator, _ original: AtlasReaderPage) async throws {
        let page = try await reader.read(.firstPage)
        require(page.identity.matches(original.identity) && page.text == original.text,
                "accepted source must remain readable from its original capture")
    }

    static func stagedSuccessDoesNotRetireOldUntilCommit() async throws {
        let f = Fixture(), other = Fixture()
        let source = AtlasReaderCoordinator(transport: f.transport)
        let reader = AtlasReaderCoordinator(transport: other.transport)
        let old = try await source.open(input: input())
        // Both owners have a ready proposal with identical-looking paths and
        // bytes. Refusing a foreign token must preserve the valid own proposal.
        let ownProposal = try await reader.stageOpen(input: input())
        f.change { $0.text = "replacement\n" }
        let proposal = try await source.stageOpen(input: input())
        require(f.snapshot.closed[old.identity.owner] == nil, "staging cannot retire accepted capture")
        rejectsCommit(reader, proposal)
        let ownPage = try reader.commitOpen(ownProposal)
        require(ownPage.text == "old capture\n", "foreign refusal preserves own candidate")
        let next = try source.commitOpen(proposal)
        require(next.text == "replacement\n" && next.identity.owner != old.identity.owner, "new capture commits")
        rejectsCommit(source, proposal)
        try await preserved(source, next)
        try await wait { f.snapshot.closed[old.identity.owner] == 1 }
        source.close(); reader.close()
        try await wait { f.snapshot.closed[next.identity.owner] == 1 && other.snapshot.closed.count == 1 }
        require(f.snapshot.retirementFailures == 0 && other.snapshot.retirementFailures == 0, "all sessions retired")
    }

    static func admissionAndCaptureFailuresPreserveOld() async throws {
        let f = Fixture()
        let reader = AtlasReaderCoordinator(transport: f.transport)
        let old = try await reader.open(input: input())
        f.change { $0.refuseCreate = true }
        await rejected { _ = try await reader.open(input: input()) }
        try await preserved(reader, old)
        f.change { $0.refuseCreate = false }
        for failure in [Fixture.Failure.open, .page, .identity] {
            f.change { $0.failure = failure }
            await rejected { _ = try await reader.open(input: input()) }
            try await preserved(reader, old)
        }
        reader.close()
        try await wait { f.snapshot.closed.count == 4 }
        require(f.snapshot.closed.values.allSatisfy { $0 == 1 }, "each refused candidate retires exactly once")
    }

    static func cancelReadyProposalPreservesOld() async throws {
        let f = Fixture()
        let source = AtlasReaderCoordinator(transport: f.transport)
        let old = try await source.open(input: input())
        let proposal = try await source.stageOpen(input: input())
        source.cancel()
        rejectsCommit(source, proposal)
        try await preserved(source, old)
        source.close()
        try await wait { f.snapshot.closed.count == 2 }
    }

    static func navigationRetiresUncommittedProposal() async throws {
        let f = Fixture()
        let source = AtlasReaderCoordinator(transport: f.transport)
        let old = try await source.open(input: input())
        let proposal = try await source.stageOpen(input: input())
        // Reading the accepted capture is a new explicit intent, not a request
        // to publish the speculative replacement behind the caller's back.
        try await preserved(source, old)
        rejectsCommit(source, proposal)
        source.close()
    }

    static func delayedCanceledCaptureRetainsGrantThroughDrain() async throws {
        let f = Fixture(), gate = Fixture.Gate()
        let reader = AtlasReaderCoordinator(transport: f.transport)
        let old = try await reader.open(input: input())
        f.change { $0.gate = gate }
        let attempt = Task { () throws -> AtlasReaderReplacement in
            try await reader.stageOpen(input: input(), accessLease: Fixture.Lease(f))
        }
        try await wait { gate.didEnter }
        reader.cancel()
        require(f.snapshot.closed[2] == nil && f.snapshot.releasedLeases == 0,
                "cancellation cannot close a handle or release its grant inside foreign I/O")
        gate.release()
        await rejected { _ = try await attempt.value }
        try await wait { f.snapshot.closed[2] == 1 && f.snapshot.releasedLeases == 1 }
        try await preserved(reader, old)
        reader.close()
    }

    static func taskCancellationAloneCannotPublish() async throws {
        let f = Fixture(), gate = Fixture.Gate()
        let reader = AtlasReaderCoordinator(transport: f.transport)
        let old = try await reader.open(input: input())
        f.change { $0.gate = gate }
        let attempt = Task { try await reader.stageOpen(input: input()) }
        try await wait { gate.didEnter }
        attempt.cancel()
        gate.release()
        await rejected { _ = try await attempt.value }
        try await preserved(reader, old)
        reader.close()
    }

    static func lateFailureCannotDiscardNewProposal() async throws {
        let f = Fixture(), gate = Fixture.Gate()
        let reader = AtlasReaderCoordinator(transport: f.transport)
        _ = try await reader.open(input: input())
        f.change { $0.gate = gate; $0.failure = .open }
        let first = Task { try await reader.stageOpen(input: input()) }
        try await wait { gate.didEnter }
        f.change { $0.failure = .none; $0.text = "newest capture\n" }
        let second = Task { try await reader.stageOpen(input: input()) }
        try await wait { f.snapshot.nextHandle == 4 }
        gate.release()
        await rejected { _ = try await first.value }
        let token = try await second.value
        let page = try reader.commitOpen(token)
        require(page.identity.owner == 3 && page.text == "newest capture\n", "newest capture survives late failure")
        reader.close()
        try await wait { f.snapshot.closed.count == 3 }
        require(f.snapshot.closed.values.allSatisfy { $0 == 1 }, "old, canceled and new handles retire once")
    }

    static func oldQuerySurvivesFailureButNeverRefreshCommit() async throws {
        let f = Fixture()
        let reader = AtlasReaderCoordinator(transport: f.transport, search: f.search)
        _ = try await reader.open(input: input())
        let report = try await reader.find("old")
        guard let oldHit = report.target(at: 0) else { fatalError("missing positive-control hit") }
        f.change { $0.failure = .page }
        await rejected { _ = try await reader.stageOpen(input: input()) }
        let retained = try await reader.read(.hit(oldHit))
        require(retained.selection?.target == oldHit, "failed recapture preserves old exact-hit authority")
        f.change { $0.failure = .none; $0.text = "old replacement\n" }
        let proposal = try await reader.stageOpen(input: input())
        _ = try reader.commitOpen(proposal)
        let calls = f.snapshot.hits.count
        await rejected { _ = try await reader.read(.hit(oldHit)) }
        let fresh = try await reader.find("old")
        require(fresh.generation > report.generation, "refresh must not recycle old query generations")
        await rejected { _ = try await reader.read(.hit(oldHit)) }
        require(f.snapshot.hits.count == calls, "stale target refused before native activation")
        guard let freshHit = fresh.target(at: 0) else { fatalError("missing new hit") }
        let page = try await reader.read(.hit(freshHit))
        require(page.text == "old replacement\n", "new capture remains searchable")
        reader.close()
        _ = try await reader.open(input: input())
        let reopened = try await reader.find("old")
        require(reopened.generation > fresh.generation, "close/open cannot revive escaped query targets either")
        reader.close()
    }

    static func liveTransportIsExplicitAndNeverAFallback() async throws {
        let captured = Fixture(), live = Fixture()
        live.change { $0.nextHandle = 100; $0.text = "live capture\n" }
        let reader = AtlasReaderCoordinator(transport: captured.transport)
        let old = try await reader.open(input: input())
        require(live.snapshot.opens.isEmpty, "normal open uses only its granted transport")
        captured.change { $0.failure = .open }
        await rejected { _ = try await reader.stageOpen(input: input()) }
        require(live.snapshot.opens.isEmpty, "failed capture transport must not try live I/O")
        try await preserved(reader, old)
        let proposal = try await reader.stageOpen(input: input(), using: live.transport)
        let page = try reader.commitOpen(proposal)
        require(page.identity.owner == 100 && page.text == "live capture\n", "explicit live request commits live capture")
        try await preserved(reader, page)
        require(live.snapshot.opens.count == 1 && live.snapshot.windows.count == 2, "later paging uses new session transport")
        reader.close()
    }

    static func closeRevokesProposalAndInitialFailureIsRetryable() async throws {
        let f = Fixture()
        let reader = AtlasReaderCoordinator(transport: f.transport)
        f.change { $0.failure = .page }
        await rejected { _ = try await reader.open(input: input()) }
        await rejected { _ = try await reader.read(.firstPage) }
        f.change { $0.failure = .none }
        let proposal = try await reader.stageOpen(input: input())
        reader.close()
        rejectsCommit(reader, proposal)
        let page = try await reader.open(input: input())
        try await preserved(reader, page)
        reader.close()
    }

    static func main() async throws {
        try await stagedSuccessDoesNotRetireOldUntilCommit()
        try await admissionAndCaptureFailuresPreserveOld()
        try await cancelReadyProposalPreservesOld()
        try await navigationRetiresUncommittedProposal()
        try await delayedCanceledCaptureRetainsGrantThroughDrain()
        try await taskCancellationAloneCannotPublish()
        try await lateFailureCannotDiscardNewProposal()
        try await oldQuerySurvivesFailureButNeverRefreshCommit()
        try await liveTransportIsExplicitAndNeverAFallback()
        try await closeRevokesProposalAndInitialFailureIsRetryable()
        print("10 retained-reader replacement regression cases passed")
    }
}
