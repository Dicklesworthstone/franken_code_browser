import Foundation

private enum TestFailure: Error { case failed(String) }

private func expect(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    guard condition() else { throw TestFailure.failed(message) }
}

@main
private struct SearchPresentationRegressionTests {
    static func main() throws {
        var passed = 0
        func test(_ name: String, _ body: () throws -> Void) throws {
            try body()
            passed += 1
            print("PASS \(name)")
        }
        let generation = UUID()
        let revision = UUID()
        func context(root: String = "/workspace", query: String = "needle",
                     load: UUID? = nil, atlas: UUID? = nil, scope: String = "All files",
                     extensions: String = "") -> AtlasSearchContext {
            AtlasSearchContext(root: root, query: query, loadGeneration: load ?? generation,
                atlasRevision: atlas ?? revision, scope: scope, customExtensions: extensions)
        }
        func hit(id: Int = 0, path: String? = "src/main.rs", start: UInt64 = 4,
                 end: UInt64 = 10, digest: String? = String(repeating: "a", count: 64),
                 length: UInt64? = 40) -> SearchHit {
            SearchHit(id: id, path: path ?? "escaped-name", sourcePath: path,
                start: start, end: end, captureSHA256: digest, captureByteLength: length)
        }
        let current = context()
        let sourceHit = hit()
        let accepted = AtlasSearchPresentation(context: current, hits: [sourceHit], verifiedHitIDs: [0])
        let row = accepted.rowID(for: 0)
        try test("verified current hit activates its original capture witness") {
            try expect(accepted.hit(for: row, in: current) == sourceHit, "wrong hit")
            try expect(accepted.availability(for: row, in: current).allowsActivation, "not actionable")
        }
        try test("missing geometry requires source verification rather than disabling the reader") {
            let pending = AtlasSearchPresentation(context: current, hits: [sourceHit], verifiedHitIDs: [])
            try expect(pending.hit(for: pending.rowID(for: 0), in: current) == nil, "borrowed geometry")
            try expect(pending.captureCandidate(for: pending.rowID(for: 0), in: current) == sourceHit, "lost capture witness")
            try expect(pending.availability(for: pending.rowID(for: 0), in: current).label == "Verify source on open", "missing explanation")
        }
        try test("repeated identical search cannot reuse a previous report row") {
            let replacement = AtlasSearchPresentation(context: current, hits: [hit(start: 20, end: 26)], verifiedHitIDs: [0])
            try expect(replacement.hit(for: row, in: current) == nil, "old row activated a new offset")
            try expect(replacement.hit(for: replacement.rowID(for: 0), in: current)?.start == 20, "fresh row rejected")
        }
        try test("foreign report identity is rejected even for a valid integer index") {
            let forged = AtlasSearchRowID(report: UUID(), hit: 0)
            try expect(accepted.hit(for: forged, in: current) == nil, "foreign row admitted")
        }
        try test("unknown hit IDs cannot borrow another row's proof") {
            try expect(accepted.hit(for: accepted.rowID(for: 99), in: current) == nil, "unknown hit admitted")
        }
        let changes: [(String, AtlasSearchContext)] = [
            ("root switch", context(root: "/other")),
            ("same-root refresh", context(load: UUID())),
            ("layout revision", context(atlas: UUID())),
            ("query replacement", context(query: "other")),
            ("query leading space", context(query: " needle")),
            ("query trailing space", context(query: "needle ")),
            ("query case", context(query: "Needle")),
            ("scope switch", context(scope: "Rust")),
            ("custom extension edit", context(extensions: "rs,swift"))
        ]
        for (name, changed) in changes {
            try test("\(name) invalidates both rendering and activation") {
                try expect(!accepted.isCurrent(in: changed), "stale overlay still current")
                try expect(accepted.hit(for: row, in: changed) == nil, "stale hit opened")
                try expect(accepted.captureCandidate(for: row, in: changed) == nil, "stale read admitted")
                try expect(accepted.availability(for: row, in: changed).label == "Search again — results changed", "no stale explanation")
            }
        }
        try test("byte-distinct canonical-equivalent queries are different searches") {
            let composed = context(query: "caf\u{e9}")
            let decomposed = context(query: "cafe\u{301}")
            try expect(composed.query == decomposed.query, "fixture not canonically equal")
            try expect(!composed.matches(decomposed), "query normalized implicitly")
        }
        try test("byte-distinct canonical-equivalent roots cannot rebind") {
            let composed = context(root: "/caf\u{e9}")
            let decomposed = context(root: "/cafe\u{301}")
            try expect(composed.root == decomposed.root, "fixture not canonically equal")
            try expect(!composed.matches(decomposed), "root normalized implicitly")
        }
        try test("unsupported path is retained without an activation target") {
            let p = AtlasSearchPresentation(context: current, hits: [hit(path: nil)], verifiedHitIDs: [0])
            let r = p.rowID(for: 0)
            try expect(p.hit(for: r, in: current) == nil, "display path used as source path")
            try expect(p.availability(for: r, in: current).label == "Filename cannot be opened", "wrong explanation")
        }
        let invalid: [(String, SearchHit)] = [
            ("missing digest", hit(digest: nil)),
            ("empty digest", hit(digest: "")),
            ("short digest", hit(digest: String(repeating: "a", count: 63))),
            ("non-hex digest", hit(digest: String(repeating: "z", count: 64))),
            ("missing capture length", hit(length: nil)),
            ("empty range", hit(start: 4, end: 4)),
            ("reversed range", hit(start: 9, end: 4)),
            ("range beyond capture", hit(end: 41)),
            ("overflow-sized offset", hit(start: UInt64.max - 1, end: UInt64.max))
        ]
        for (name, invalidHit) in invalid {
            try test("\(name) cannot activate despite a listed verified ID") {
                let p = AtlasSearchPresentation(context: current, hits: [invalidHit], verifiedHitIDs: [0])
                try expect(p.hit(for: p.rowID(for: 0), in: current) == nil, "invalid witness admitted")
                try expect(p.captureCandidate(for: p.rowID(for: 0), in: current) == nil, "invalid read admitted")
            }
        }
        try test("duplicate hit identities fail closed without a dictionary trap") {
            let p = AtlasSearchPresentation(context: current,
                hits: [sourceHit, hit(start: 20, end: 26)], verifiedHitIDs: [0])
            try expect(p.hit(for: p.rowID(for: 0), in: current) == nil, "ambiguous row admitted")
        }
        try test("independent report values preserve the original immutable witness") {
            let other = AtlasSearchPresentation(context: current, hits: [hit(id: 1)], verifiedHitIDs: [1])
            try expect(other.hit(for: row, in: current) == nil, "cross-report hit admitted")
            try expect(accepted.hit(for: row, in: current) == sourceHit, "original report mutated")
        }
        try test("decoded engine response feeds the production activation model") {
            let json = #"{"schema":"fcb.cli/1","status":"ok","command":"search","scope":"workspace","mode":"decoded-text-literal","workspace_complete":true,"discovery_complete":true,"truncated":false,"matches_seen":"1","hits":[{"path":{"encoding":"unix-bytes","hex":"7372632f6d61696e2e7273","display":"src/main.rs"},"original_range":{"start":"4","end":"10"},"capture_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","capture_byte_length":"40"}],"unavailable_files":[],"unsupported_text_files":[]}"#
            let report = try AtlasSearchReport.decode(json)
            let p = AtlasSearchPresentation(context: current, hits: report.hits, verifiedHitIDs: [0])
            try expect(report.complete, "valid response not complete")
            try expect(p.hit(for: p.rowID(for: 0), in: current) == sourceHit, "wire witness changed")
        }
        try test("pending source completion cannot reuse row zero from a replacement report") {
            let pending = AtlasSearchPresentation(context: current, hits: [sourceHit], verifiedHitIDs: [])
            let oldRow = pending.rowID(for: 0)
            let replacement = AtlasSearchPresentation(context: current,
                hits: [hit(start: 20, end: 26)], verifiedHitIDs: [])
            try expect(pending.captureCandidate(for: oldRow, in: current) == sourceHit, "valid read denied")
            try expect(replacement.captureCandidate(for: oldRow, in: current) == nil, "stale completion admitted")
            try expect(replacement.captureCandidate(for: replacement.rowID(for: 0), in: current)?.start == 20,
                "new capture witness lost")
        }
        print("\(passed) search presentation regression tests passed")
    }
}
