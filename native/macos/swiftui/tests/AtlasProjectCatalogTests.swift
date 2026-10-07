import Foundation

@main private enum AtlasProjectCatalogTests {
    private static func file(_ id: String = "1", hex: String = "612e7273", bytes: String = "17") -> [String: Any] {
        ["file_id": id, "observed_bytes": bytes, "path": ["encoding": "unix-bytes", "hex": hex, "display": "label only"],
         "x": 0, "y": 0, "w": 32, "h": 16]
    }
    private static func wire(_ files: [[String: Any]] = [file()], complete: Bool = true) -> [String: Any] {
        ["schema": "fcb.project-catalog/1", "status": "ok", "command": "catalog", "identity_scope": "response-local",
         "native_presented": false, "source_payload_read": false, "payload_bytes_read": "0", "read_calls": "0",
         "discovery_complete": complete, "catalogued_files": String(files.count), "policy": "fixture-policy",
         "world": ["w": 4096, "h": 4096], "files": files]
    }
    private static func decode(_ object: [String: Any]) throws -> AtlasProjectCatalog {
        try AtlasProjectCatalog.decode(JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]))
    }
    @MainActor private static var checks = 0
    @MainActor private static func check(_ value: @autoclosure () -> Bool, _ message: String) {
        precondition(value(), message); checks += 1
    }
    @MainActor private static func refuse(_ object: [String: Any], _ reason: String) throws {
        do { _ = try decode(object); preconditionFailure("Accepted: " + reason) }
        catch is AtlasProjectCatalog.DecodeError { checks += 1 }
    }
    @MainActor static func main() throws {
        let r = try decode(wire())
        check(r.entries.count == 1 && r.entries[0].sourcePath == "a.rs", "Native path comes from reversible bytes, not label")
        check(r.entries[0].displayPath == "label only" && r.entries[0].observedBytes == 17, "Metadata labels and exact sizes retained")
        check(r.entries[0].width == 32 && r.entries[0].height == 16, "Use supplied engine geometry")
        let partial = try decode(wire(complete: false))
        check(!partial.discoveryComplete && partial.entries.count == 1 && partial.summary.contains("more may exist"), "Known partial files are usable")
        let empty = try decode(wire([]))
        check(empty.discoveryComplete && empty.entries.isEmpty, "Real complete empty catalog")
        let unknown = try decode(wire([], complete: false))
        check(!unknown.discoveryComplete && unknown.summary.contains("Partial catalog"), "Unknown membership is not empty success")
        let raw = try decode(wire([file("1"), file("2", hex: "ff2e7273")]))
        check(raw.entries.count == 2 && raw.openableCount == 1, "Non-UTF-8 row is preserved without blocking other files")
        check(raw.entries[1].sourcePath == nil && raw.entries[1].rawPath == [255, 46, 114, 115], "Never synthesize UTF-8 from raw filename")
        check(raw.summary.contains("1 filenames cannot be opened"), "Native opener limitation is visible")
        let unicode = try decode(wire([file("1", hex: "c3a92e7273"), file("2", hex: "65cc812e7273")]))
        check(unicode.entries.count == 2 && unicode.entries[0].rawPath != unicode.entries[1].rawPath,
              "Canonical-equivalent Unicode filenames remain distinct")
        let controls = try decode(wire([file(hex: "610a622e7273")]))
        check(controls.entries[0].rawPath.contains(10), "Control bytes retained as identity, not used as display labels")
        let huge = try decode(wire([file("9007199254740993", bytes: "18446744073709551615")]))
        check(huge.entries[0].id == 9_007_199_254_740_993 && huge.entries[0].observedBytes == UInt64.max, "Full-width identity and byte count")
        for (key, value) in [("schema", "fcb.host-atlas/1"), ("status", "error"), ("command", "search"),
                             ("identity_scope", "retained"), ("payload_bytes_read", "1"), ("read_calls", "1"),
                             ("catalogued_files", "0"), ("catalogued_files", "01"), ("policy", "")] {
            var v = wire(); v[key] = value; try refuse(v, key)
        }
        for key in ["native_presented", "source_payload_read"] {
            var v = wire(); v[key] = true; try refuse(v, key)
            v[key] = 0; try refuse(v, key + " must be boolean")
        }
        for key in ["read_calls", "catalogued_files"] {
            var v = wire(); v[key] = 1; try refuse(v, "no numeric instead of string " + key)
        }
        for id in ["0", "01", "-1", "18446744073709551616", " 1"] {
            try refuse(wire([file(id)]), "invalid ID " + id)
        }
        try refuse(wire([file(), file()]), "duplicate ID/path")
        try refuse(wire([file(), file("2")]), "duplicate raw path")
        try refuse(wire([file(), file("1", hex: "622e7273")]), "duplicate ID")
        for path in ["", "2f61", "2e2e2f61", "612f2e2f62", "612f2e2e2f62", "612f2f62", "612f", "6100", "6A", "f", "zz"] {
            try refuse(wire([file(hex: path)]), "invalid native path " + path)
        }
        for key in ["x", "y", "w", "h"] {
            var f = file(); f[key] = -1; try refuse(wire([f]), "negative geometry " + key)
            f[key] = 8192; try refuse(wire([f]), "out of world geometry " + key)
        }
        var wrong = wire(); wrong["world"] = ["w": 0, "h": 4096]; try refuse(wrong, "invalid world")
        var wrongFile = file(); wrongFile["path"] = ["encoding": "lossy", "hex": "61", "display": "a"]
        try refuse(wire([wrongFile]), "unrecognized path encoding")
        wrongFile = file(); wrongFile["path"] = ["encoding": "unix-bytes", "hex": "61", "display": String(repeating: "x", count: 65_537)]
        try refuse(wire([wrongFile]), "oversized display")
        try refuse(wire(Array(repeating: file(), count: 20_001)), "row admission cap")
        do {
            _ = try AtlasProjectCatalog.decode(Data(repeating: 32, count: AtlasProjectCatalog.maximumResponseBytes + 1))
            preconditionFailure("Oversized response admitted")
        } catch AtlasProjectCatalog.DecodeError.invalidResponse { checks += 1 }
        print("AtlasProjectCatalogTests: \(checks) checks passed")
    }
}
