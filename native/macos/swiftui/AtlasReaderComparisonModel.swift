import Foundation
import Observation

/// One explicit comparison request and one accepted bounded report. The two
/// readers remain independently owned by their pin host; no source or search
/// state is swapped when a comparison fails, is canceled, or exceeds its budget.
@Observable @MainActor final class AtlasReaderComparisonModel {
    private(set) var report: AtlasReaderComparisonReport?
    private(set) var busy = false
    private(set) var notice = ""
    private(set) var selectedSpanID: UInt64?
    var showsEqual = false
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var operation = UUID()
    @ObservationIgnored private var sources: (AtlasReaderIdentity, AtlasReaderIdentity)?
    @ObservationIgnored private var pendingNavigation: (
        before: AtlasPagedReaderModel, after: AtlasPagedReaderModel,
        beforeTarget: AtlasReaderComparisonNavigation, afterTarget: AtlasReaderComparisonNavigation)?

    deinit { task?.cancel() }

    func compare(before: AtlasPagedReaderModel, after: AtlasPagedReaderModel, refine: Bool = false) {
        guard !busy, before.canCompare, after.canCompare,
              let beforeIdentity = before.page?.identity, let afterIdentity = after.page?.identity else {
            notice = "Wait for both retained readers to finish before comparing."
            return
        }
        task?.cancel()
        let ticket = UUID()
        operation = ticket
        sources = (beforeIdentity, afterIdentity)
        busy = true
        notice = "Comparing the two retained captures…"
        let limits = refine ? AtlasReaderComparisonLimits(editDistance: 512, work: 64 * 1024 * 1024) : .init()
        task = Task { [weak self] in
            do {
                let result = try await before.compare(with: after, limits: limits)
                guard let self, self.operation == ticket, !Task.isCancelled else { return }
                guard result.matches(before: before.page?.identity, after: after.page?.identity) else {
                    throw AtlasReaderError.canceled
                }
                self.report = result
                self.selectedSpanID = nil
                self.busy = false
                self.task = nil
                self.notice = result.summary
            } catch {
                guard let self, self.operation == ticket, !Task.isCancelled else { return }
                self.busy = false
                self.task = nil
                self.notice = (error as? AtlasReaderError)?.message ?? "The retained captures could not be compared."
            }
        }
    }

    var selectedSpan: AtlasReaderComparisonReport.Span? {
        guard let selectedSpanID else { return nil }
        return report?.spans.first { $0.id == selectedSpanID }
    }

    var changePosition: String {
        guard let report else { return "" }
        let changes = report.changes
        guard let selectedSpanID else { return "\(changes.count) changed or unresolved ranges" }
        guard let index = changes.firstIndex(where: { $0.id == selectedSpanID }) else { return "Equal range selected" }
        return "Change \(index + 1) of \(changes.count)"
    }

    func canMoveChange(backwards: Bool) -> Bool { !busy && adjacentChange(backwards: backwards) != nil }

    private func adjacentChange(backwards: Bool) -> AtlasReaderComparisonReport.Span? {
        guard let report else { return nil }
        guard let selectedSpanID else { return backwards ? report.changes.last : report.changes.first }
        return backwards ? report.changes.last(where: { $0.id < selectedSpanID })
            : report.changes.first(where: { $0.id > selectedSpanID })
    }

    func moveChange(backwards: Bool, before: AtlasPagedReaderModel, after: AtlasPagedReaderModel) {
        guard let span = adjacentChange(backwards: backwards) else { return }
        open(spanID: span.id, before: before, after: after)
    }

    /// Stage both bounded windows before changing either reader. Failure or
    /// cancellation preserves the accepted pages, selection and change ordinal.
    func open(spanID: UInt64, before: AtlasPagedReaderModel, after: AtlasPagedReaderModel) {
        guard !busy, let report, report.matches(before: before.page?.identity, after: after.page?.identity),
              let span = report.spans.first(where: { $0.id == spanID }), before !== after else { return }
        let beforeTarget: AtlasReaderComparisonNavigation
        do { beforeTarget = try before.beginComparedRange(span.before, identity: report.before) }
        catch { notice = "Wait for both retained readers to finish before opening a change."; return }
        let afterTarget: AtlasReaderComparisonNavigation
        do { afterTarget = try after.beginComparedRange(span.after, identity: report.after) }
        catch {
            before.cancelComparedRange(beforeTarget)
            notice = "Wait for both retained readers to finish before opening a change."
            return
        }
        let ticket = UUID()
        operation = ticket
        busy = true
        pendingNavigation = (before, after, beforeTarget, afterTarget)
        notice = "Opening both sides of \(span.label.lowercased())…"
        task = Task { [weak self] in
            do {
                let old = try await before.prepareComparedRange(beforeTarget)
                let new = try await after.prepareComparedRange(afterTarget)
                guard let self, self.operation == ticket, !Task.isCancelled,
                      self.report?.generation == report.generation,
                      report.matches(before: before.page?.identity, after: after.page?.identity),
                      before.canCommitComparedRange(old), after.canCommitComparedRange(new) else {
                    throw AtlasReaderError.canceled
                }
                // Validation and both installations run without an await.
                guard before.commitComparedRange(old), after.commitComparedRange(new) else {
                    throw AtlasReaderError.canceled
                }
                self.selectedSpanID = span.id
                self.pendingNavigation = nil
                self.busy = false
                self.task = nil
                self.notice = self.changePosition + " · " + span.label
            } catch {
                before.cancelComparedRange(beforeTarget)
                after.cancelComparedRange(afterTarget)
                guard let self, self.operation == ticket, !Task.isCancelled else { return }
                self.pendingNavigation = nil
                self.busy = false
                self.task = nil
                self.notice = "The change was not opened. "
                    + ((error as? AtlasReaderError)?.message ?? "Both retained readers remain available.")
            }
        }
    }

    func validate(before: AtlasReaderIdentity?, after: AtlasReaderIdentity?) {
        guard let sources else { return }
        guard before.map({ sources.0.matches($0) }) == true,
              after.map({ sources.1.matches($0) }) == true else {
            cancel()
            report = nil
            selectedSpanID = nil
            self.sources = nil
            notice = "A retained source changed or closed. Compare the current retained captures to continue."
            return
        }
    }

    func cancel() {
        operation = UUID()
        task?.cancel()
        task = nil
        if let pending = pendingNavigation {
            pending.before.cancelComparedRange(pending.beforeTarget)
            pending.after.cancelComparedRange(pending.afterTarget)
        }
        pendingNavigation = nil
        busy = false
        notice = "Comparison canceled. The retained readers remain available."
    }

    func close() {
        cancel()
        report = nil
        selectedSpanID = nil
        sources = nil
        notice = ""
    }
}
