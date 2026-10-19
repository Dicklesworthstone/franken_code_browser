import Foundation
import Observation

/// One explicit comparison request and one accepted bounded report. The two
/// readers remain independently owned by their pin host; no source or search
/// state is swapped when a comparison fails, is canceled, or exceeds its budget.
@Observable @MainActor final class AtlasReaderComparisonModel {
    private(set) var report: AtlasReaderComparisonReport?
    private(set) var busy = false
    private(set) var notice = ""
    var showsEqual = false
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var operation = UUID()
    @ObservationIgnored private var sources: (AtlasReaderIdentity, AtlasReaderIdentity)?

    deinit { task?.cancel() }

    func compare(before: AtlasPagedReaderModel, after: AtlasPagedReaderModel, refine: Bool = false) {
        guard !busy, before.canCompare, after.canCompare,
              let beforeIdentity = before.page?.identity, let afterIdentity = after.page?.identity else {
            notice = "Wait for both pinned readers to finish before comparing."
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

    func validate(before: AtlasReaderIdentity?, after: AtlasReaderIdentity?) {
        guard let sources else { return }
        guard before.map({ sources.0.matches($0) }) == true,
              after.map({ sources.1.matches($0) }) == true else {
            cancel()
            report = nil
            self.sources = nil
            notice = "A pinned source changed or closed. Compare the current retained captures to continue."
            return
        }
    }

    func cancel() {
        operation = UUID()
        task?.cancel()
        task = nil
        busy = false
        notice = "Comparison canceled. The retained readers remain available."
    }

    func close() {
        cancel()
        report = nil
        sources = nil
        notice = ""
    }
}
