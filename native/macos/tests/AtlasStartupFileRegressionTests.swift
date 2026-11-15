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
    static func outcome(_ arguments: [String], root: String? = "/project", permitted: Bool = true) -> Outcome {
        var request = AtlasStartupFileRequest(arguments: arguments)
        return request.consume(root: root, permitsProjectArguments: permitted)
    }

    static func normalSelectionIsConsumedOnce() {
        var request = AtlasStartupFileRequest(arguments: argv("main.rs"))
        require(request.isPending, "explicit filename is pending")
        require(request.consume(root: "/project") == .open(path: "main.rs", line: nil), "must open the exact filename")
        require(!request.isPending, "request is consumed")
        require(request.consume(root: "/project") == .none, "must not reopen after refresh")
    }
    static func ordinaryStartupDoesNotSelect() {
        for args in [["app"], ["app", "-fcb.root", "/project"], ["app", "/project"]] {
            require(outcome(args) == .none, "no filename action without explicit flag")
        }
    }
    static func rootReplacementConsumesWithoutActivation() {
        var request = AtlasStartupFileRequest(arguments: argv("main.rs"))
        require(request.consume(root: "/other") == .differentRoot, "stale root")
        require(request.consume(root: "/project") == .none, "old root cannot revive the action")
        require(outcome(argv("main.rs"), root: nil) == .differentRoot, "unbound project cannot select")
    }
    static func nativeIdentityIsNotUnicodeCanonicalEquality() {
        let composed = "é.rs", decomposed = "e\u{301}.rs"
        require(composed == decomposed, "negative control must be Swift-canonically equal")
        guard case .open(let path, _) = outcome(argv(decomposed)) else { fatalError("missing filename") }
        require(path.utf8.elementsEqual(decomposed.utf8) && !path.utf8.elementsEqual(composed.utf8), "raw filename identity")
        require(outcome(argv(composed, root: "/é"), root: "/e\u{301}") == .differentRoot, "raw root identity")
    }
    static func punctuationIsData() {
        for name in ["my source.rs", "-fcb.root", "-fcb.line", "quote\".rs", "line\nfeed.rs", "tab\t.rs", "back\\slash.rs", "資料.rs"] {
            require(outcome(argv(name)) == .open(path: name, line: nil), "native punctuation must survive hex framing")
        }
    }
    static func explicitFilesDoNotDependOnDiscoveryOrDisplayFilters() {
        for name in [".hidden", "ignored.log", "last-file-outside-catalog.rs", "unknown.extension"] {
            require(outcome(argv(name)) == .open(path: name, line: nil),
                "the exact filename needs no metadata row; the reader performs actual source admission")
        }
    }
    static func sandboxDoesNotAcceptCommandLineGrants() {
        require(outcome(argv("main.rs"), permitted: false) == .grantRequired, "picker grant required")
        require(outcome(argv("main.rs") + ["-fcb.line", "120"], permitted: false) == .grantRequired,
            "line navigation cannot create a root grant")
    }
    static func malformedFramingIsRejected() {
        for args in [
            ["app", "-fcb.file-hex"], ["app", "-fcb.file-hex", "61"],
            argv("main.rs") + ["-fcb.root", "/other"],
            argv("main.rs") + ["-fcb.file-hex", "62"],
            argv("main.rs", root: "relative"), argv("main.rs", root: "/nul\0root")
        ] { require(outcome(args) == .invalid, "invalid startup framing") }
        for value in ["", "6", "zz", "FF", "ff", "00", "2e", "2e2e", "2f61", "612f62", "c0af"] {
            require(outcome(["app", "-fcb.root", "/project", "-fcb.file-hex", value]) == .invalid,
                "invalid hex, traversal, NUL or malformed UTF-8")
        }
    }
    static func inputBoundsAreEnforced() {
        require(outcome(argv("x") + Array(repeating: "ignored", count: 64)) == .invalid, "argument bound")
        require(outcome(argv(String(repeating: "x", count: 16_385))) == .invalid, "hex byte bound")
        require(outcome(argv("x", root: "/" + String(repeating: "a", count: 16_384))) == .invalid, "root byte bound")
        require(outcome(argv("x") + [String(repeating: "a", count: 32_768), String(repeating: "b", count: 32_768)]) == .invalid, "total byte bound")
    }
    static func requestedLinesPreserveFullWidthIdentity() {
        for line in [UInt64(1), 120, 9_007_199_254_740_993, UInt64.max] {
            require(outcome(argv("main.rs") + ["-fcb.line", String(line)]) == .open(path: "main.rs", line: line),
                "one-based line is preserved without floating-point conversion")
        }
        require(outcome(["app", "-fcb.line", "120", "-fcb.root", "/project", "-fcb.file-hex", hex("main.rs")])
            == .open(path: "main.rs", line: 120), "option order does not change the request")
    }
    static func invalidLineRequestsCannotSilentlyOpenLineOne() {
        for value in ["", "0", "01", "+1", "-1", "1.0", " 1", "1 ", "18446744073709551616", "-fcb.root"] {
            require(outcome(argv("main.rs") + ["-fcb.line", value]) == .invalid, "invalid line must not be ignored")
        }
        for args in [argv("main.rs") + ["-fcb.line"],
            argv("main.rs") + ["-fcb.line", "1", "-fcb.line", "2"],
            ["app", "-fcb.root", "/project", "-fcb.line", "120"]] {
            require(outcome(args) == .invalid, "line requires one explicit filename and one canonical value")
        }
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
        require(outcome(argv("main.rs"), root: report.requestedRoot) == .open(path: "main.rs", line: nil), "host establishes root identity")
        let unbound = try AtlasProjectCatalog.decode(data)
        require(unbound.requestedRoot == nil, "unbound decoder remains unbound")
    }
    static func main() throws {
        normalSelectionIsConsumedOnce()
        ordinaryStartupDoesNotSelect()
        rootReplacementConsumesWithoutActivation()
        nativeIdentityIsNotUnicodeCanonicalEquality()
        punctuationIsData()
        explicitFilesDoNotDependOnDiscoveryOrDisplayFilters()
        sandboxDoesNotAcceptCommandLineGrants()
        malformedFramingIsRejected()
        inputBoundsAreEnforced()
        requestedLinesPreserveFullWidthIdentity()
        invalidLineRequestsCannotSilentlyOpenLineOne()
        try rootProvenanceIsHostSupplied()
        print("12 startup file regression cases passed")
    }
}
