// Production resumable index worker with scripted engine envelopes. No claim
// of Rust execution, filesystem capture, native ABI, or UI performance.
import Foundation

private final class StepControl: @unchecked Sendable {
    enum Action { case normal, cancelAfterReceipt, cancelInsideCall, missingReceipt, invalidReceipt }
    private let lock = NSLock()
    private var action: Action = .normal
    func set(_ value: Action) { lock.lock(); action = value; lock.unlock() }
    func transport(_ base: AtlasIndexedSearchTransport) -> AtlasIndexedSearchTransport {
        AtlasIndexedSearchTransport(create: base.create, open: base.open, indexBegin: base.indexBegin,
            indexStep: { h, g, flag in
                self.lock.lock(); let action = self.action; self.lock.unlock()
                if action == .cancelInsideCall { flag.cancel(); throw AtlasSearchError.canceled }
                let result = try base.indexStep(h, g, flag)
                if action == .cancelAfterReceipt { flag.cancel() }
                if action == .missingReceipt { return nil }
                if action == .invalidReceipt { return "{\"status\":\"ok\"}" }
                return result
            }, queryBegin: base.queryBegin, queryStep: base.queryStep, page: base.page,
            sourceReader: base.sourceReader, close: base.close, retirementFailed: base.retirementFailed)
    }
}
private final class Grant {}

@main private enum AtlasIndexedResumeTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        precondition(condition(), message); checks += 1
    }
    @MainActor private static func canceled(_ run: () throws -> AtlasSearchReport) {
        do { _ = try run(); preconditionFailure("Canceled request succeeded") }
        catch AtlasSearchError.canceled { checks += 1 }
        catch { preconditionFailure("Wrong cancellation result: \(error)") }
    }
    private static func run(_ worker: AtlasIndexedSearch, root: String = "/repo", query: String = "x",
                            access: AtlasSearchAccessLease = AtlasSearchAccessLease(nil),
                            progress: @Sendable (AtlasSearchReport) -> Void = { _ in }) throws -> AtlasSearchReport {
        try worker.run(root: root, query: query, cancellation: AtlasSearchCancellation(), access: access, progress: progress)
    }
    @MainActor static func main() throws {
        do {
            let f = IndexedFixture(), control = StepControl()
            f.configure { $0.files = 4 }
            let worker = AtlasIndexedSearch(transport: control.transport(f.transport))
            control.set(.cancelAfterReceipt)
            var streams: Set<UUID> = []
            for completed in 0..<4 {
                let reports = IndexReports()
                canceled { try run(worker, query: "query-\(completed)", progress: reports.offer) }
                check(f.count("create:") == 1 && f.count("open:") == 1 && f.count("build-begin:") == 1,
                      "Typing resumes one catalog and one builder, not a new source scan")
                check(f.count("build-step:") == completed + 1, "Each admitted file step is performed once")
                check(reports.values.first?.progress?.examinedFiles == completed,
                      "Replacement starts from last acknowledged coverage")
                check(reports.values.allSatisfy { $0.indexStatus?.building == true && $0.capture == nil && $0.hits.isEmpty },
                      "Private construction never masquerades as query/source authority")
                for report in reports.values { if let id = report.streamID { streams.insert(id) } }
            }
            check(streams.count == 4, "New request does not borrow old presentation stream IDs")
            check(f.count("query-begin:") == 0, "Canceled preparation never starts a query")
            control.set(.normal)
            let finished = try run(worker, query: "final")
            check(finished.complete && finished.indexStatus?.reused == true, "Terminal receipt survives its delivery cancellation")
            check(f.count("build-step:") == 4 && f.count("create:") == 1, "No recapture on terminal retry")
            let before = f.count("build-step:")
            _ = try run(worker, query: "another")
            check(f.count("build-step:") == before, "Later queries remain source-I/O free")
            let target = finished.capture!.target(finished.hits[1])!
            _ = try target.openReader(100)
            check(f.count("source:") == 1, "Exact indexed source remains importable after query replacement")
        }
        do {
            let f = IndexedFixture(), worker = AtlasIndexedSearch(transport: f.transport)
            let flag = AtlasSearchCancellation()
            canceled {
                try worker.run(root: "/repo", query: "first", cancellation: flag,
                    access: AtlasSearchAccessLease(nil), progress: { _ in flag.cancel() })
            }
            check(f.count("build-step:") == 0, "Cancellation at publication prevents the next file read")
            do { _ = try run(worker, query: ""); preconditionFailure("Invalid replacement accepted") }
            catch AtlasSearchError.invalidRequest { checks += 1 }
            check(f.count("create:") == 1, "Invalid replacement preserves already admitted build")
            _ = try run(worker)
            check(f.count("create:") == 1 && f.count("build-step:") == 2, "Begun zero-step build resumes")
        }
        for failure in [StepControl.Action.cancelInsideCall, .missingReceipt, .invalidReceipt] {
            let f = IndexedFixture(), control = StepControl()
            let worker = AtlasIndexedSearch(transport: control.transport(f.transport))
            control.set(.cancelAfterReceipt)
            canceled { try run(worker) }
            control.set(failure)
            do { _ = try run(worker); preconditionFailure("Uncertain step accepted") }
            catch { checks += 1 }
            control.set(.normal)
            let retried = try run(worker)
            check(retried.complete && f.count("create:") == 2, "Unknown foreign progress retires the candidate before retry")
            check(f.count("open:") == 2 && f.count("build-begin:") == 2, "Retry establishes a new source universe")
        }
        for changedGrant in [false, true] {
            let f = IndexedFixture(), control = StepControl()
            let worker = AtlasIndexedSearch(transport: control.transport(f.transport))
            let first = Grant(), second = Grant()
            control.set(.cancelAfterReceipt)
            canceled { try run(worker, root: "/r/é", access: AtlasSearchAccessLease(first)) }
            control.set(.normal)
            _ = try run(worker, root: changedGrant ? "/r/é" : "/r/e\u{301}",
                access: AtlasSearchAccessLease(changedGrant ? second : first))
            check(f.count("create:") == 2, "Raw-root or grant replacement cannot resume the old source")
        }
        do {
            let f = IndexedFixture(), control = StepControl()
            f.configure { $0.files = 4; $0.unavailable = 1; $0.pending = 1; $0.fallback = true; $0.discovery = false }
            let worker = AtlasIndexedSearch(transport: control.transport(f.transport))
            control.set(.cancelAfterReceipt)
            canceled { try run(worker) }
            control.set(.normal)
            let result = try run(worker)
            check(!result.complete && result.unavailableFiles == 1, "Resumption preserves unavailable/discovery coverage")
            check(f.count("build-step:") == 3 && result.hits.count == 2, "Uncovered sources retain exact fallback results")
            check(result.indexStatus?.capturedFiles == 2, "Captured membership is not the catalog's file count")
        }
        do {
            let f = IndexedFixture(), control = StepControl()
            control.set(.cancelAfterReceipt)
            var worker: AtlasIndexedSearch? = AtlasIndexedSearch(transport: control.transport(f.transport))
            var grant: Grant? = Grant()
            weak var weakGrant = grant
            canceled { try run(worker!, access: AtlasSearchAccessLease(grant)) }
            grant = nil
            check(weakGrant != nil && f.count("close:") == 0, "Paused builder retains root access")
            worker = nil
            let deadline = Date().addingTimeInterval(5)
            while (weakGrant != nil || f.count("close:") != 1) && Date() < deadline { Thread.sleep(forTimeInterval: 0.001) }
            check(weakGrant == nil && f.count("close:") == 1, "Dismissed worker retires paused capture and grant")
        }
        do {
            let f = IndexedFixture(), worker = AtlasIndexedSearch(transport: f.transport)
            _ = try run(worker)
            let flag = AtlasSearchCancellation()
            f.block { phase in if phase == "query-step" { flag.cancel() } }
            canceled {
                try worker.run(root: "/repo", query: "cancel query", cancellation: flag,
                    access: AtlasSearchAccessLease(nil), progress: { _ in })
            }
            f.block(nil)
            let result = try run(worker)
            check(result.indexStatus?.reused == true && f.count("create:") == 1, "Canceled query cannot evict a committed index")
            let fresh = AtlasIndexedSearch(transport: f.transport)
            _ = try run(fresh)
            check(f.count("create:") == 2, "Explicit refresh remains a fresh owner, not mutable old captures")
        }
        print("AtlasIndexedResumeTests: \(checks) checks passed")
    }
}
