import Foundation

@main struct AtlasSearchTests {
    static var checks = 0
    static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        precondition(condition(), message)
        checks += 1
    }
    static func main() throws {
        func envelope(_ hits: [[String: Any]] = []) -> [String: Any] {
            ["schema": "fcb.cli/1", "status": "ok", "command": "search", "scope": "workspace",
             "mode": "decoded-text-literal", "workspace_complete": true, "discovery_complete": true,
             "truncated": false, "matches_seen": String(hits.count), "hits": hits,
             "unavailable_files": [], "unsupported_text_files": []]
        }
        func hit(hex: String = "7372632f612e7273", display: String = "src/a.rs",
                 start: String = "2", end: String = "8") -> [String: Any] {
            ["path": ["encoding": "unix-bytes", "hex": hex, "display": display],
             "original_range": ["start": start, "end": end]]
        }
        func decode(_ fields: [String: Any]) throws -> AtlasSearchReport {
            let data = try JSONSerialization.data(withJSONObject: fields, options: [.sortedKeys])
            return try AtlasSearchReport.decode(String(decoding: data, as: UTF8.self))
        }
        func rejects(_ fields: [String: Any]) {
            do { _ = try decode(fields); preconditionFailure("malformed result accepted") }
            catch { checks += 1 }
        }
        let empty = try decode(envelope())
        check(empty.complete && empty.hits.isEmpty, "real empty search is complete")
        check(empty.summary.hasPrefix("No exact matches"), "complete empty copy")
        let positive = try decode(envelope([hit()]))
        check(positive.complete && positive.hits.count == 1, "ordinary exact result")
        check(positive.hits[0].sourcePath == "src/a.rs", "native path decoded")
        check(positive.hits[0].start == 2 && positive.hits[0].end == 8, "exact offset domain")
        check(positive.hits[0].captureSHA256 == nil, "legacy results cannot claim exact capture identity")
        var identified = hit()
        identified["capture_sha256"] = String(repeating: "a", count: 64)
        identified["capture_byte_length"] = "12"
        let verified = try decode(envelope([identified])).hits[0]
        check(verified.captureSHA256 == String(repeating: "a", count: 64) && verified.captureByteLength == 12,
              "capture identity reaches native match resolver")
        for digest in ["", String(repeating: "a", count: 63), String(repeating: "G", count: 64)] {
            var malformed = identified; malformed["capture_sha256"] = digest; rejects(envelope([malformed]))
        }
        for length in ["7", "012", "-1", "18446744073709551616"] {
            var malformed = identified; malformed["capture_byte_length"] = length; rejects(envelope([malformed]))
        }
        for field in ["capture_sha256", "capture_byte_length"] {
            var incomplete = identified; incomplete.removeValue(forKey: field); rejects(envelope([incomplete]))
        }
        let maxOffset = try decode(envelope([hit(start: "18446744073709551614", end: "18446744073709551615")]))
        check(maxOffset.hits[0].end == UInt64.max, "full-width offsets never become zero")
        let escaped = try decode(envelope([hit(hex: "61200a2e7273", display: "a \\n.rs")]))
        check(escaped.hits[0].sourcePath == "a \n.rs", "display escaping must not choose filesystem path")
        check(escaped.hits[0].path == "a \\n.rs", "escaped presentation remains intact")
        let opaque = try decode(envelope([hit(hex: "ff2e7273", display: "\\xff.rs")]))
        check(opaque.hits.count == 1 && opaque.hits[0].sourcePath == nil, "retain non-UTF8 result without lossy activation")
        var limited = envelope()
        limited["truncated"] = true
        let truncated = try decode(limited)
        check(!truncated.complete && truncated.summary.contains("More matches may exist"), "truncation is not a complete negative")
        for flag in ["workspace_complete", "discovery_complete"] {
            var partial = envelope(); partial[flag] = false
            let result = try decode(partial)
            check(!result.complete && result.summary.hasPrefix("Partial search"), "partial discovery/capture")
        }
        for field in ["unavailable_files", "unsupported_text_files"] {
            var partial = envelope([hit()]); partial[field] = [["file_id": "3"]]
            let result = try decode(partial)
            check(!result.complete, "explicit missing coverage overrides contradictory complete flag")
        }
        var mismatch = envelope([hit()]); mismatch["matches_seen"] = "2"
        let mismatched = try decode(mismatch)
        check(!mismatched.complete, "omitted result cannot claim full delivery")
        for hex in ["0", "gg", "00", "2f61", "2e2e2f61", "612f2e2f62", "612f2f62"] {
            rejects(envelope([hit(hex: hex)]))
        }
        for offset in ["-1", "00", "+1", "18446744073709551616", "not-a-number"] {
            rejects(envelope([hit(start: offset)]))
        }
        rejects(envelope([hit(start: "9", end: "2")]))
        var missing = envelope(); missing.removeValue(forKey: "workspace_complete"); rejects(missing)
        var numericBool = envelope(); numericBool["workspace_complete"] = 1; rejects(numericBool)
        var foreign = envelope(); foreign["schema"] = "unknown/2"; rejects(foreign)
        let badPayloads: [String?] = [nil, "", "{}", "not json"]
        for payload in badPayloads {
            do { _ = try AtlasSearchReport.decode(payload); preconditionFailure("failure became empty results") }
            catch { checks += 1 }
        }
        precondition(AtlasFileScope.markdown.includes("docs/README.MD"))
        precondition(!AtlasFileScope.markdown.includes("docs/md/code.rs"))
        precondition(AtlasFileScope.python.includes("typing.pyi"))
        precondition(AtlasFileScope.rust.includes("nested/Cargo.toml"))
        precondition(AtlasFileScope.rust.includes("src/lib.rs"))
        precondition(!AtlasFileScope.rust.includes("src/lib.rs.bak"))
        precondition(AtlasFileScope.custom.includes("a.SWIFT", custom: ".md, .SWIFT"))
        precondition(!AtlasFileScope.custom.includes("README", custom: "... ,"))
        precondition(AtlasFileScope.all.includes("README"))
        print("AtlasSearch: \(checks) coverage, path and offset checks passed")
    }
}
