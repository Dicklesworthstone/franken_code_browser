import Foundation
import Dispatch

private enum TestFailure: Error { case failed(String) }
private func expect(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    guard condition() else { throw TestFailure.failed(message) }
}
private func emptyReport() -> AtlasSearchReport {
    AtlasSearchReport(hits: [], complete: true, truncated: false,
        unavailableFiles: 0, unsupportedFiles: 0, matchesSeen: 0)
}

/// Instrument the actual production coordinator, not a replacement scheduler.
private final class WorkProbe: @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [(String, String)] = []
    private var active = 0
    private var peak = 0
    private var cancellations: [Bool] = []
    let gate = DispatchSemaphore(value: 0)
    let blockFirst: Bool
    init(blockFirst: Bool = false) { self.blockFirst = blockFirst }
    var calls: [(String, String)] {
        lock.lock(); defer { lock.unlock() }; return recorded
    }
    var maxActive: Int {
        lock.lock(); defer { lock.unlock() }; return peak
    }
    var canceledAtReturn: [Bool] {
        lock.lock(); defer { lock.unlock() }; return cancellations
    }
    func run(root: String, query: String, cancellation: AtlasSearchCancellation) -> AtlasSearchReport {
        lock.lock()
        recorded.append((root, query))
        let first = recorded.count == 1
        active += 1
        peak = max(peak, active)
        lock.unlock()
        if blockFirst && first { gate.wait() }
        lock.lock()
        cancellations.append(cancellation.isCanceled)
        active -= 1
        lock.unlock()
        return emptyReport()
    }
}

@MainActor
private func pump(until condition: () -> Bool) throws {
    let deadline = Date().addingTimeInterval(5)
    while !condition() {
        guard Date() < deadline else { throw TestFailure.failed("coordinator did not finish before test deadline") }
        _ = RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.005))
    }
}

@main
@MainActor
private struct LiteralSearchRegressionTests {
    static func main() throws {
        var passed = 0
        func test(_ name: String, _ body: () throws -> Void) throws {
            try body(); passed += 1; print("PASS \(name)")
        }
        let literals = [" needle ", " ", "\t", "\nneedle\n", "\r\n", "\u{2003}",
            "caf\u{e9}", "cafe\u{301}", "🚀", String(repeating: " ", count: 1_024)]
        for (index, query) in literals.enumerated() {
            try test("literal input \(index) preserves every UTF-8 byte") {
                let root = "/a project/cafe\u{301} "
                let input = try AtlasSearchInput(root: root, query: query)
                try expect(input.root.utf8.elementsEqual(root.utf8), "root rewritten")
                try expect(input.query.utf8.elementsEqual(query.utf8), "query rewritten")
                try AtlasSearchCoordinator.validate(root: root, query: query)
            }
        }
        let invalid: [(String, String, String)] = [
            ("empty root", "", "x"), ("empty query", "/r", ""),
            ("NUL root", "/r\0x", "x"), ("NUL query", "/r", "a\0b"),
            ("oversize root", String(repeating: "r", count: 16_385), "x"),
            ("oversize query", "/r", String(repeating: " ", count: 1_025)),
            ("UTF-8 byte limit, not character count", "/r", String(repeating: "🚀", count: 257))
        ]
        for (name, root, query) in invalid {
            try test("reject \(name) without sanitizing it") {
                do {
                    _ = try AtlasSearchInput(root: root, query: query)
                    throw TestFailure.failed("invalid input accepted")
                } catch AtlasSearchError.invalidRequest { }
            }
        }
        try test("exact byte limits remain admissible") {
            _ = try AtlasSearchInput(root: String(repeating: "r", count: 16_384),
                query: String(repeating: "🚀", count: 256))
        }
        try test("production coordinator forwards literal input unchanged to its worker") {
            let probe = WorkProbe()
            let coordinator = AtlasSearchCoordinator { root, query, flag in
                probe.run(root: root, query: query, cancellation: flag)
            }
            var completions = 0
            var failures = 0
            for query in literals {
                let expected = completions + 1
                let input = try AtlasSearchInput(root: "/project with spaces ", query: query)
                try coordinator.submit(input: input) { result in
                    if case .failure = result { failures += 1 }
                    completions += 1
                }
                try pump { completions == expected }
            }
            try expect(failures == 0, "worker delivery failed")
            try expect(probe.calls.count == literals.count, "lost input")
            for ((root, actual), expected) in zip(probe.calls, literals) {
                try expect(root.utf8.elementsEqual("/project with spaces ".utf8), "root changed at dispatch")
                try expect(actual.utf8.elementsEqual(expected.utf8), "query changed at dispatch")
            }
        }
        try test("invalid submission preserves valid in-flight work and generation") {
            let probe = WorkProbe(blockFirst: true)
            let coordinator = AtlasSearchCoordinator { root, query, flag in
                probe.run(root: root, query: query, cancellation: flag)
            }
            var delivered = 0
            let first = try coordinator.submit(root: "/r", query: " keep spaces ") { result in
                if case .success = result { delivered += 1 }
            }
            try pump { probe.calls.count == 1 }
            for (_, root, query) in invalid {
                do {
                    try coordinator.submit(root: root, query: query) { _ in delivered += 1_000 }
                    throw TestFailure.failed("invalid submission accepted")
                } catch AtlasSearchError.invalidRequest { }
            }
            probe.gate.signal()
            try pump { delivered == 1 }
            try expect(probe.canceledAtReturn == [false], "valid active request canceled")
            let second = try coordinator.submit(root: "/r", query: "next") { _ in delivered += 1 }
            try expect(second == first + 1, "invalid requests consumed identities")
            try pump { delivered == 2 }
        }
        try test("rapid replacement runs only active and newest request") {
            let probe = WorkProbe(blockFirst: true)
            let coordinator = AtlasSearchCoordinator { root, query, flag in
                probe.run(root: root, query: query, cancellation: flag)
            }
            var delivered: [String] = []
            try coordinator.submit(root: "/old", query: "first") { _ in delivered.append("first") }
            try pump { probe.calls.count == 1 }
            for index in 0..<100 {
                try coordinator.submit(root: "/new", query: " request \(index) ") { _ in
                    delivered.append("\(index)")
                }
            }
            probe.gate.signal()
            try pump { !delivered.isEmpty }
            try expect(delivered == ["99"], "superseded result delivered")
            try expect(probe.calls.map { $0.1 } == ["first", " request 99 "], "queue did not coalesce")
            try expect(probe.maxActive == 1, "overlapping foreign calls")
        }
        try test("cancel discards old active and pending deliveries before restart") {
            let probe = WorkProbe(blockFirst: true)
            let coordinator = AtlasSearchCoordinator { root, query, flag in
                probe.run(root: root, query: query, cancellation: flag)
            }
            var delivered: [String] = []
            try coordinator.submit(root: "/r", query: "first") { _ in delivered.append("first") }
            try pump { probe.calls.count == 1 }
            try coordinator.submit(root: "/r", query: "discard") { _ in delivered.append("discard") }
            coordinator.cancel()
            try coordinator.submit(root: "/r", query: "restart") { _ in delivered.append("restart") }
            probe.gate.signal()
            try pump { !delivered.isEmpty }
            try expect(delivered == ["restart"], "canceled result delivered")
            try expect(probe.calls.map { $0.1 } == ["first", "restart"], "canceled pending request ran")
        }
        try test("destroyed coordinator retains access lease until foreign work finishes") {
            final class Lease { }
            let probe = WorkProbe(blockFirst: true)
            var coordinator: AtlasSearchCoordinator? = AtlasSearchCoordinator { root, query, flag in
                probe.run(root: root, query: query, cancellation: flag)
            }
            var lease: Lease? = Lease()
            weak var retained = lease
            var delivered = false
            try coordinator?.submit(root: "/r", query: "literal", accessLease: lease) { _ in delivered = true }
            try pump { probe.calls.count == 1 }
            lease = nil
            coordinator = nil
            try expect(retained != nil, "grant released during foreign read")
            probe.gate.signal()
            try pump { retained == nil }
            try expect(!delivered, "destroyed view received a result")
            try expect(probe.canceledAtReturn == [true], "worker not invalidated")
        }
        try test("explicit file opening does not invent an exact match range") {
            let context = AtlasSearchContext(root: "/r", query: "x", loadGeneration: UUID(),
                atlasRevision: UUID(), scope: "All files", customExtensions: "")
            let hit = SearchHit(id: 0, path: "display-only", sourcePath: "src/real.rs",
                start: 4, end: 5, captureSHA256: nil, captureByteLength: nil)
            let presentation = AtlasSearchPresentation(context: context, hits: [hit], verifiedHitIDs: [])
            let row = presentation.rowID(for: 0)
            try expect(presentation.hit(for: row, in: context) == nil, "unverified match admitted")
            try expect(presentation.filePath(for: row, in: context) == "src/real.rs", "source path unavailable")
            let newer = AtlasSearchPresentation(context: context, hits: [hit], verifiedHitIDs: [])
            try expect(newer.filePath(for: row, in: context) == nil, "old row opens a new report")
            let changed = AtlasSearchContext(root: "/other", query: context.query,
                loadGeneration: context.loadGeneration, atlasRevision: context.atlasRevision,
                scope: context.scope, customExtensions: context.customExtensions)
            try expect(presentation.filePath(for: row, in: changed) == nil, "old path reopened under a different root")
        }
        try test("explicit file opening rejects undecodable paths and unknown IDs") {
            let context = AtlasSearchContext(root: "/r", query: "x", loadGeneration: UUID(),
                atlasRevision: UUID(), scope: "All files", customExtensions: "")
            let hit = SearchHit(id: 0, path: "escaped-name", sourcePath: nil,
                start: 0, end: 1, captureSHA256: nil, captureByteLength: nil)
            let p = AtlasSearchPresentation(context: context, hits: [hit], verifiedHitIDs: [])
            try expect(p.filePath(for: p.rowID(for: 0), in: context) == nil, "display label used as path")
            try expect(p.filePath(for: p.rowID(for: 9), in: context) == nil, "unknown row opened")
        }
        print("\(passed) literal search and coordinator regression tests passed")
    }
}
