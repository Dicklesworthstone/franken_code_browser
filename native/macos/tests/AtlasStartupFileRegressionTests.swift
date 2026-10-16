import Foundation

// Compile with swiftui/AtlasProjectCatalog.swift as a standalone test executable
// in the configured qualification lane. No AppKit, source I/O or bridge stubs.
@main struct AtlasStartupFileRegressionTests {
    typealias Outcome = AtlasStartupFileRequest.Outcome
    static func require(_ condition: @autoclosure () -> Bool, _ message: String) {
        if !condition() { fatalError(message) }
    }
    static func hex(_ name: String) -> String { name.utf8.map { String(format: "%02x", $0) }.joined() }
    static func argv(_ name: String, root: String = "/project") -> [String] {
        ["FrankenCodeBrowser", "-fcb.root", root, "-fcb.file-hex", hex(name)]
    }
    static func row(_ id: UInt64, _ name: String, openable: Bool = true) -> AtlasProjectCatalog.Entry {
        .init(id: id, rawPath: Array(name.utf8), displayPath: "display-only",
            sourcePath: openable ? name : nil, observedBytes: 10, x: 0, y: 0, width: 1, height: 1)
    }
    static func catalog(_ rows: [AtlasProjectCatalog.Entry], root: String? = "/project", complete: Bool = true) -> AtlasProjectCatalog {
        .init(entries: rows, discoveryComplete: complete, policy: "static-product-exclusions", requestedRoot: root)
    }
    static func outcome(_ arguments: [String], in report: AtlasProjectCatalog,
        visible: [AtlasProjectCatalog.Entry]? = nil, permitted: Bool = true) -> Outcome {
        var request = AtlasStartupFileRequest(arguments: arguments)
        return request.consume(catalog: report, visibleEntries: visible ?? report.entries, permitsProjectArguments: permitted)
    }

    static func normalSelectionIsConsumedOnce() {
        let rows = [row(1, "other.rs"), row(2, "main.rs")]
        var request = AtlasStartupFileRequest(arguments: argv("main.rs"))
        require(request.consume(catalog: catalog(rows), visibleEntries: rows) == .open(2), "must open the named row")
        require(request.consume(catalog: catalog(rows), visibleEntries: rows) == .none, "must not reopen after refresh")
    }
    static func ordinaryStartupDoesNotSelect() {
        let report = catalog([row(1, "main.rs")])
        for args in [["app"], ["app", "-fcb.root", "/project"], ["app", "/project"]] {
            require(outcome(args, in: report) == .none, "no filename action without explicit flag")
        }
    }
    static func rootReplacementConsumesWithoutActivation() {
        let rows = [row(1, "main.rs")]
        var request = AtlasStartupFileRequest(arguments: argv("main.rs"))
        require(request.consume(catalog: catalog(rows, root: "/other"), visibleEntries: rows) == .differentRoot, "stale root")
        require(request.consume(catalog: catalog(rows), visibleEntries: rows) == .none, "old root cannot revive the action")
        require(outcome(argv("main.rs"), in: catalog(rows, root: nil)) == .differentRoot, "unbound reports cannot select")
    }
    static func nativeIdentityIsNotUnicodeCanonicalEquality() {
        let composed = "é.rs", decomposed = "e\u{301}.rs"
        require(composed == decomposed, "negative control must be Swift-canonically equal")
        let report = catalog([row(1, composed), row(2, decomposed)])
        require(outcome(argv(decomposed), in: report) == .open(2), "raw filename identity")
        require(outcome(argv(composed, root: "/é"), in: catalog(report.entries, root: "/e\u{301}")) == .differentRoot, "raw root identity")
    }
    static func punctuationIsData() {
        for name in ["my source.rs", "-fcb.root", "quote\".rs", "line\nfeed.rs", "tab\t.rs", "back\\slash.rs", "資料.rs"] {
            let report = catalog([row(9, name)])
            require(outcome(argv(name), in: report) == .open(9), "native punctuation must survive hex framing")
        }
    }
    static func missingFilesPreserveCoverage() {
        for complete in [true, false] {
            require(outcome(argv("main.rs"), in: catalog([], complete: complete)) == .notCatalogued(partial: !complete), "missing is not proven absent")
        }
        let report = catalog([row(1, "main.rs")], complete: false)
        require(outcome(argv("main.rs"), in: report) == .open(1), "partial catalog does not forbid a known file")
    }
    static func filtersAndAvailabilityRemainAuthoritative() {
        let report = catalog([row(1, "main.rs")])
        require(outcome(argv("main.rs"), in: report, visible: []) == .filtered, "do not bypass display filter")
        require(outcome(argv("main.rs"), in: report, visible: [row(1, "wrong.rs")]) == .filtered, "ID alone cannot authorize")
        require(outcome(argv("main.rs"), in: catalog([row(1, "main.rs", openable: false)])) == .unavailable, "no lossy source path")
    }
    static func sandboxDoesNotAcceptCommandLineGrants() {
        let report = catalog([row(1, "main.rs")])
        require(outcome(argv("main.rs"), in: report, permitted: false) == .grantRequired, "picker grant required")
    }
    static func malformedFramingIsRejected() {
        let report = catalog([row(1, "main.rs")])
        for args in [
            ["app", "-fcb.file-hex"], ["app", "-fcb.file-hex", "61"],
            argv("main.rs") + ["-fcb.root", "/other"],
            argv("main.rs") + ["-fcb.file-hex", "62"],
            argv("main.rs", root: "relative"), argv("main.rs", root: "/nul\0root")
        ] { require(outcome(args, in: report) == .invalid, "invalid startup framing") }
        for value in ["", "6", "zz", "FF", "ff", "00", "2e", "2e2e", "2f61", "612f62", "c0af"] {
            require(outcome(["app", "-fcb.root", "/project", "-fcb.file-hex", value], in: report) == .invalid,
                "invalid hex, traversal, NUL or malformed UTF-8")
        }
    }
    static func inputBoundsAreEnforced() {
        let report = catalog([])
        require(outcome(argv("x") + Array(repeating: "ignored", count: 64), in: report) == .invalid, "argument bound")
        require(outcome(argv(String(repeating: "x", count: 16_385)), in: report) == .invalid, "hex byte bound")
        require(outcome(argv("x", root: "/" + String(repeating: "a", count: 16_384)), in: report) == .invalid, "root byte bound")
        require(outcome(argv("x") + [String(repeating: "a", count: 32_768), String(repeating: "b", count: 32_768)], in: report) == .invalid, "total byte bound")
    }
    static func rootProvenanceIsHostSupplied() throws {
        let wire: [String: Any] = [
            "schema": "fcb.project-catalog/1", "status": "ok", "command": "catalog",
            "identity_scope": "response-local", "native_presented": false, "source_payload_read": false,
            "payload_bytes_read": "0", "read_calls": "0", "catalogued_files": "1",
            "discovery_complete": true, "world": ["w": 4096, "h": 4096], "policy": "static",
            "requestedRoot": "/forged", "files": [["file_id": "1", "observed_bytes": "10",
                "path": ["encoding": "unix-bytes", "hex": hex("main.rs"), "display": "main.rs"],
                "x": 0, "y": 0, "w": 1, "h": 1]]
        ]
        let data = try JSONSerialization.data(withJSONObject: wire)
        let report = try AtlasProjectCatalog.decode(data, requestedRoot: "/project")
        require(report.requestedRoot == "/project", "wire cannot replace host provenance")
        require(outcome(argv("main.rs"), in: report) == .open(1), "decoded production report admits the file")
        let unbound = try AtlasProjectCatalog.decode(data)
        require(unbound.requestedRoot == nil, "unbound decoder remains unbound")
    }
    static func main() throws {
        normalSelectionIsConsumedOnce()
        ordinaryStartupDoesNotSelect()
        rootReplacementConsumesWithoutActivation()
        nativeIdentityIsNotUnicodeCanonicalEquality()
        punctuationIsData()
        missingFilesPreserveCoverage()
        filtersAndAvailabilityRemainAuthoritative()
        sandboxDoesNotAcceptCommandLineGrants()
        malformedFramingIsRejected()
        inputBoundsAreEnforced()
        try rootProvenanceIsHostSupplied()
        print("11 startup file regression cases passed")
    }
}
