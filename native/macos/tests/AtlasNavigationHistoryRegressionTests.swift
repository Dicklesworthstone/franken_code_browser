import Foundation

// Compile with the production AtlasNavigationHistory.swift. No I/O or UI mocks.
@main struct AtlasNavigationHistoryRegressionTests {
    typealias History = AtlasNavigationHistory<UInt64>
    static func require(_ condition: @autoclosure () -> Bool, _ message: String) {
        if !condition() { fatalError(message) }
    }
    static func visit(_ value: UInt64, _ history: inout History) {
        require(history.commit(history.visit(value), resolved: value), "fresh visit must commit")
    }
    static func back(_ history: inout History) {
        guard let move = history.back() else { fatalError("missing previous destination") }
        require(history.commit(move, resolved: move.target), "fresh back must commit")
    }
    static func forward(_ history: inout History) {
        guard let move = history.forward() else { fatalError("missing forward destination") }
        require(history.commit(move, resolved: move.target), "fresh forward must commit")
    }
    static func emptyAndFirstVisit() {
        var history = History()
        require(history.current == nil && history.back() == nil && history.forward() == nil, "empty history")
        visit(7, &history)
        require(history.current == 7 && history.retainedCount == 0, "first visit has no phantom predecessor")
    }
    static func roundTripKeepsBothDirections() {
        var history = History()
        for value in [UInt64(10), 20, 30] { visit(value, &history) }
        back(&history); back(&history)
        require(history.current == 10 && !history.canBack && history.canForward, "back to beginning")
        forward(&history); forward(&history)
        require(history.current == 30 && history.canBack && !history.canForward, "forward to end")
        require(history.retainedCount == 2, "travel moves entries instead of duplicating them")
    }
    static func failureAndCancellationDoNotConsumeHistory() {
        var history = History()
        for value in [UInt64(1), 2, 3] { visit(value, &history) }
        let requested = history.back()
        require(requested?.target == 2 && history.current == 3 && history.retainedCount == 2,
                "requesting I/O changes nothing")
        // Dropping the proposal models failed/canceled I/O; retry remains valid.
        require(history.back()?.target == 2 && !history.canForward, "canceled request is not a committed move")
        back(&history)
        let next = history.forward()
        require(next?.target == 3 && history.current == 2, "forward proposal is also non-destructive")
        require(history.forward()?.target == 3 && history.back()?.target == 1, "failed forward retains both directions")
    }
    static func branchOnlyChangesAfterSuccessfulVisit() {
        var history = History()
        for value in [UInt64(1), 2, 3] { visit(value, &history) }
        back(&history)
        let branch = history.visit(9)
        require(history.forward()?.target == 3, "uncommitted branch preserves forward history")
        require(history.commit(branch, resolved: 9), "commit branch")
        require(!history.canForward && history.back()?.target == 2, "successful new branch retires forward history")
        back(&history)
        require(history.current == 2 && history.forward()?.target == 9, "new branch is traversable")
    }
    static func repeatedPositionDoesNotDestroyForwardHistory() {
        var history = History()
        for value in [UInt64(1), 2, 3] { visit(value, &history) }
        back(&history)
        visit(2, &history)
        require(history.forward()?.target == 3 && history.retainedCount == 2, "no-op positioning is not a branch")
    }
    static func lateProposalCannotOverwriteNewNavigation() {
        var history = History()
        visit(1, &history); visit(2, &history)
        guard let old = history.back() else { fatalError("missing back") }
        visit(9, &history)
        require(!history.commit(old, resolved: 1), "late back must be rejected")
        require(history.current == 9 && history.back()?.target == 2, "current navigation survives late response")
    }
    static func resetRejectsOldCaptureAndLookalikeAnchors() {
        var history = History()
        visit(1, &history); visit(2, &history)
        let old = history.visit(3)
        history.reset(to: 2)
        require(!history.commit(old, resolved: 3), "same-looking position in a replacement capture is not the old history")
        require(history.current == 2 && history.retainedCount == 0, "reset preserves only explicit new current")
        history.reset()
        require(history.current == nil && !history.canBack && !history.canForward, "close clears all navigation")
    }
    static func proposalsAreOwnerLocalAndSingleUse() {
        var left = History(), right = History()
        visit(1, &left); visit(1, &right)
        let move = left.visit(2)
        require(!right.commit(move, resolved: 2), "another window cannot commit this move")
        require(left.commit(move, resolved: 2), "own window may commit")
        require(!left.commit(move, resolved: 2), "a move is single-use")
    }
    static func memoryBoundCoversBothDirections() {
        var history = History(capacity: 3)
        for value in UInt64(0)..<100 {
            visit(value, &history)
            require(history.retainedCount <= 3, "bounded visits")
        }
        back(&history); back(&history); back(&history)
        require(history.current == 96 && !history.canBack && history.retainedCount == 3, "oldest entries evicted")
        forward(&history)
        require(history.retainedCount == 3, "both directions share one quota")
        visit(200, &history)
        require(history.retainedCount == 2 && !history.canForward, "branch reclaims superseded future")
    }
    static func zeroCapacityAndOversizedConfiguration() {
        var history = History(capacity: 0)
        visit(1, &history); visit(2, &history)
        require(history.current == 2 && history.retainedCount == 0, "zero quota still admits current page")
        require(History(capacity: -1).capacity == 0, "negative quota cannot underflow")
        require(History(capacity: Int.max).capacity == 128, "hard upper bound")
    }
    static func fullWidthPositionsAreNotNarrowed() {
        var history = History()
        for value in [UInt64(1) << 53, UInt64(1) << 63, UInt64.max] { visit(value, &history) }
        back(&history)
        require(history.current == (UInt64(1) << 63), "64-bit anchor retained")
        forward(&history)
        require(history.current == UInt64.max, "no floating-point or signed-integer roundtrip")
    }
    static func resolvedAnchorReplacesTransientRequest() {
        var history = History()
        visit(1, &history)
        // 999 represents a transient match target; 64 is its accepted plain
        // source window. History must retain only the latter.
        require(history.commit(history.visit(999), resolved: 64), "resolved source window commits")
        back(&history)
        require(history.forward()?.target == 64, "do not resurrect transient selection authority")
    }
    static func main() {
        emptyAndFirstVisit()
        roundTripKeepsBothDirections()
        failureAndCancellationDoNotConsumeHistory()
        branchOnlyChangesAfterSuccessfulVisit()
        repeatedPositionDoesNotDestroyForwardHistory()
        lateProposalCannotOverwriteNewNavigation()
        resetRejectsOldCaptureAndLookalikeAnchors()
        proposalsAreOwnerLocalAndSingleUse()
        memoryBoundCoversBothDirections()
        zeroCapacityAndOversizedConfiguration()
        fullWidthPositionsAreNotNarrowed()
        resolvedAnchorReplacesTransientRequest()
        print("12 navigation history regression cases passed")
    }
}
