import Foundation

@main private enum AtlasProjectCatalogTests {
    private static func file(_ id: String = "1", hex: String = "612e7273", bytes: String = "17") -> [String: Any] {
        ["file_id": id, "observed_bytes": bytes, "path": ["encoding": "unix-bytes", "hex": hex, "display": "label only"],
         "x": 0, "y": 0, "w": 32, "h": 16]
    }
    private static func wire(_ files: [[String: Any]] = [file()], complete: Bool = true,
        policy: String = "repository-rules-v1") -> [String: Any] {
        ["schema": "fcb.project-catalog/1", "status": "ok", "command": "catalog", "identity_scope": "response-local",
         "native_presented": false, "source_payload_read": false, "payload_bytes_read": "0", "read_calls": "0",
         "discovery_complete": complete, "catalogued_files": String(files.count), "policy": policy,
         "world": ["w": 4096, "h": 4096], "files": files]
    }
    private static func decode(_ object: [String: Any], requestedPolicy: String = "repository-rules-v1") throws -> AtlasProjectCatalog {
        try AtlasProjectCatalog.decode(JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]),
            requestedPolicy: requestedPolicy)
    }
    @MainActor private static var checks = 0
    @MainActor private static func check(_ value: @autoclosure () -> Bool, _ message: String) {
        precondition(value(), message); checks += 1
    }
    @MainActor private static func refuse(_ object: [String: Any], _ reason: String,
        requestedPolicy: String = "repository-rules-v1") throws {
        do { _ = try decode(object, requestedPolicy: requestedPolicy); preconditionFailure("Accepted: " + reason) }
        catch is AtlasProjectCatalog.DecodeError { checks += 1 }
    }
    @MainActor private static func exactFileScope() throws {
        let raw: [UInt8] = [0xff, 47, 97, 46, 114, 115]
        let focus = try AtlasFileFocus(rawPath: raw), target = AtlasCatalogTarget.file(focus)
        check(target != .directory(focus.directory) && target.containsFile(raw)
              && !target.containsFile([0xff, 47, 98, 46, 114, 115]), "File scope is exact, not its containing folder")
        let rootFile = try AtlasFileFocus(rawPath: Array("late.rs".utf8))
        check(rootFile.directory.isProject && !AtlasCatalogTarget.file(rootFile).isProject
              && AtlasCatalogTarget.file(rootFile).parentDirectory == .project, "Root-level file keeps a distinct target")
        let atLimit = try AtlasFileFocus(rawPath: Array(repeating: 97, count: 2048))
        check(atLimit.rawPath.count == 2048, "Exact file byte boundary is admitted")
        let deepest = Array(repeating: "a", count: 32).joined(separator: "/")
        _ = try AtlasFileFocus(rawPath: Array(deepest.utf8))
        _ = try AtlasFileFocus(rawPath: [92, 58, 0xff])
        for invalid in [[UInt8](), Array("/file".utf8), Array("../file".utf8), Array("a//b".utf8),
                        Array("a/./b".utf8), Array("a/".utf8), [97, 0, 98], Array(repeating: 97, count: 2049),
                        Array((deepest + "/a").utf8)] {
            do { _ = try AtlasFileFocus(rawPath: invalid); preconditionFailure("Invalid file target admitted") }
            catch { checks += 1 }
        }
        var valid = wire([file(hex: "ff2f612e7273")])
        valid["schema"] = "fcb.project-file-catalog/1"; valid["command"] = "file-catalog"
        valid["path_basis"] = "project-root"
        valid["file"] = ["encoding": "unix-bytes", "hex": "ff2f612e7273", "display": "escaped file"]
        func decodeFile(_ value: [String: Any]) throws -> AtlasProjectCatalog {
            try AtlasProjectCatalog.decodeFile(JSONSerialization.data(withJSONObject: value),
                requestedRoot: "/project", file: focus, requestedPolicy: "repository-rules-v1")
        }
        let accepted = try decodeFile(valid)
        check(accepted.requestedTarget == target && accepted.requestedRoot == "/project"
              && accepted.entries[0].rawPath == raw && accepted.entries[0].sourcePath == nil,
              "Exact non-UTF8 scope and request root survive without lossy path conversion")
        check(accepted.summary.contains("Single-file scope"), "File completeness is never labeled as folder completeness")
        var empty = valid; empty["files"] = [[String: Any]](); empty["catalogued_files"] = "0"
        let completeEmpty = try decodeFile(empty)
        check(completeEmpty.discoveryComplete, "Policy-excluded exact file can have a complete empty receipt")
        empty["discovery_complete"] = false
        let partialEmpty = try decodeFile(empty)
        check(!partialEmpty.discoveryComplete, "Incomplete ancestor discovery remains qualified")
        var corrupt: [[String: Any]] = []
        for (key, value) in [("schema", "fcb.project-catalog/1" as Any), ("command", "catalog" as Any),
                             ("path_basis", "directory" as Any), ("policy", "all-regular-files/no-rule-files-v1" as Any),
                             ("file", ["encoding": "unix-bytes", "hex": "ff2f622e7273", "display": "sibling"] as Any),
                             ("directory", ["encoding": "unix-bytes", "hex": "ff", "display": "parent"] as Any)] {
            var valueWire = valid; valueWire[key] = value; corrupt.append(valueWire)
        }
        var missing = valid; missing.removeValue(forKey: "file"); corrupt.append(missing)
        var sibling = valid; sibling["files"] = [file(hex: "ff2f622e7273")]; corrupt.append(sibling)
        var extra = valid; extra["files"] = [file(hex: "ff2f612e7273"), file("2", hex: "ff2f622e7273")]
        extra["catalogued_files"] = "2"; corrupt.append(extra)
        for bad in corrupt {
            do { _ = try decodeFile(bad); preconditionFailure("Wrong exact-file receipt admitted") }
            catch AtlasProjectCatalog.DecodeError.invalidResponse { checks += 1 }
        }
    }


    @MainActor private static func atlasFileNavigation() throws {
        func member(_ index: Int, path: [UInt8]? = nil, width: Double = 32) -> AtlasProjectCatalog.Entry {
            let raw = path ?? Array("file-\(index).rs".utf8)
            return AtlasProjectCatalog.Entry(id: UInt64(index + 1), rawPath: raw, displayPath: "untrusted label",
                sourcePath: String(bytes: raw, encoding: .utf8), observedBytes: 17,
                x: Double(index), y: 0, width: width, height: 16)
        }
        let frame = AtlasCatalogNavigation.Frame(layout: UUID(), content: UUID())
        let members = (0..<65).map { member($0) }
        var navigation = AtlasCatalogNavigation()
        check(navigation.install(members, frame: frame), "Install shares an admitted catalog")
        navigation.setActive(true)
        let first = navigation.rows[0].id
        check(navigation.rows.count == 32 && navigation.focusedRow == nil,
              "Accessibility starts with at most 32 rows and independent focus")
        check(!navigation.focus(first, expected: frame)
              && navigation.entry(for: first, expected: frame, action: .read) == nil,
              "A pending layout cannot lend metadata to a native action")
        let wrongContent = AtlasCatalogNavigation.Frame(layout: frame.layout, content: UUID())
        check(!navigation.acknowledge(wrongContent) && navigation.acknowledge(frame),
              "Only the exact presented content opens the action boundary")
        check(navigation.focus(first, expected: frame)
              && navigation.entry(for: first, expected: frame, action: .read)?.rawPath == members[0].rawPath,
              "An acknowledged row opens its exact native path")
        check(navigation.rows[0].label == "file-0.rs", "Labels are escaped from raw paths, never authority")
        check(navigation.movePage(1, expected: frame) && navigation.pageOffset == 32
              && navigation.rows.count == 32 && navigation.focusedOrdinal == 32,
              "Next page advances a bounded window")
        check(navigation.movePage(1, expected: frame) && navigation.pageOffset == 64
              && navigation.rows.count == 1 && !navigation.movePage(1, expected: frame),
              "Final partial page is retained without phantom rows")
        let last = navigation.focusedRow!.id
        check(navigation.entry(for: last, expected: frame, action: .read)?.id == 65,
              "Later windows retain exact response identities")
        check(navigation.moveFocus(1, expected: frame) && navigation.focusedOrdinal == 0
              && navigation.pageOffset == 0 && !navigation.movePage(-1, expected: frame),
              "Keyboard traversal wraps without scanning or skipping entries")
        check(navigation.moveFocus(-1, expected: frame) && navigation.focusedOrdinal == 64,
              "Reverse keyboard traversal reaches the final window")
        check(!navigation.moveFocus(0, expected: frame) && !navigation.movePage(Int.max, expected: frame),
              "Invalid navigation directions never overflow")

        check(navigation.request(wrongContent) && navigation.focusedRow?.id == last
              && navigation.pageOffset == 64 && !navigation.accepts(frame),
              "Source replacement preserves focus and page while fencing actions")
        check(navigation.entry(for: last, expected: frame, action: .read) == nil
              && navigation.entry(for: last, expected: wrongContent, action: .read) == nil
              && !navigation.acknowledge(frame),
              "Old and unpresented replacement receipts are both refused")
        check(navigation.acknowledge(wrongContent)
              && navigation.entry(for: last, expected: wrongContent, action: .read)?.id == 65,
              "The same file becomes actionable after its new content is presented")
        navigation.setActive(false)
        check(!navigation.acknowledge(wrongContent) && !navigation.moveFocus(1, expected: wrongContent)
              && navigation.entry(for: last, expected: wrongContent, action: .read) == nil,
              "Hidden, canceled, or detached navigation cannot activate sources")
        navigation.setActive(true)
        check(!navigation.accepts(wrongContent) && navigation.acknowledge(wrongContent),
              "Returning requires a compatible presentation receipt")

        let replacement = AtlasCatalogNavigation.Frame(layout: UUID(), content: UUID())
        check(navigation.install([member(64, path: Array("different.rs".utf8))], frame: replacement)
              && navigation.acknowledge(replacement) && navigation.focusedRow == nil,
              "Scope/filter replacement resets page and focus")
        check(navigation.entry(for: last, expected: replacement, action: .read) == nil,
              "A reused response ID cannot activate a replacement layout")
        let accepted = navigation.rows[0].id
        let forgedPath = AtlasCatalogNavigation.Identity(layout: replacement.layout, ordinal: 0,
            file: accepted.file, rawPath: Array("other.rs".utf8))
        let forgedID = AtlasCatalogNavigation.Identity(layout: replacement.layout, ordinal: 0,
            file: accepted.file + 1, rawPath: accepted.rawPath)
        check(navigation.entry(for: forgedPath, expected: replacement, action: .read) == nil
              && navigation.entry(for: forgedID, expected: replacement, action: .reveal) == nil,
              "Ordinal alone never authorizes a different path or file ID")

        let special = [
            member(0, path: Array("é.rs".utf8)), member(1, path: Array("e\u{301}.rs".utf8)),
            member(2, path: [255, 46, 114, 115]), member(3, path: Array("a\nb.rs".utf8)),
            member(4, path: Array("../outside.rs".utf8)), member(5, width: .nan)
        ]
        check(navigation.install(special, frame: frame) && navigation.acknowledge(frame),
              "An admitted catalog keeps unusual or unavailable rows visible")
        let rows = navigation.rows
        check(rows[0].id != rows[1].id
              && navigation.entry(for: rows[0].id, expected: frame, action: .read)?.rawPath == special[0].rawPath
              && navigation.entry(for: rows[1].id, expected: frame, action: .read)?.rawPath == special[1].rawPath,
              "Canonically equivalent Unicode names retain distinct exact identities")
        check(rows[2].canReveal && !rows[2].canRead && rows[2].limitation != nil
              && navigation.entry(for: rows[2].id, expected: frame, action: .reveal) != nil
              && navigation.entry(for: rows[2].id, expected: frame, action: .read) == nil,
              "Non-UTF-8 rows can be revealed without inventing an openable path")
        check(rows[3].label == "a\\u{a}b.rs" && !rows[3].label.contains("\n")
              && navigation.entry(for: rows[3].id, expected: frame, action: .read)?.rawPath.contains(10) == true,
              "Control characters are escaped only for display; original identity survives")
        for row in rows.suffix(2) {
            check(!row.canReveal && !row.canRead && row.limitation != nil
                  && navigation.entry(for: row.id, expected: frame, action: .reveal) == nil,
                  "Malformed paths and geometry have explicit non-activating rows")
        }
        check(navigation.install([], frame: replacement) && navigation.acknowledge(replacement)
              && navigation.rows.isEmpty && !navigation.moveFocus(1, expected: replacement)
              && !navigation.movePage(1, expected: replacement),
              "Empty/filtered-out catalogs are usable without fabricated targets")
        let maximum = (0..<AtlasProjectCatalog.maximumFiles).map { member($0) }
        check(navigation.install(maximum, frame: frame) && navigation.acknowledge(frame)
              && navigation.moveFocus(-1, expected: frame)
              && navigation.focusedOrdinal == maximum.count - 1 && navigation.rows.count <= 32,
              "The full atlas bound remains reachable through one bounded window")
        check(!navigation.install(Array(repeating: member(0), count: AtlasProjectCatalog.maximumFiles + 1),
                                  frame: replacement)
              && navigation.frame == nil && navigation.rows.isEmpty && !navigation.accepts(frame),
              "Oversized replacement cannot keep an older actionable frame")
    }

    @MainActor static func main() throws {
        try exactFileScope()
        try atlasFileNavigation()
        let r = try decode(wire())
        check(r.entries.count == 1 && r.entries[0].sourcePath == "a.rs", "Native path comes from reversible bytes, not label")
        check(r.entries[0].displayPath == "label only" && r.entries[0].observedBytes == 17, "Metadata labels and exact sizes retained")
        check(r.entries[0].width == 32 && r.entries[0].height == 16, "Use supplied engine geometry")
        let rules = "repository-rules-v1", all = "all-regular-files/no-rule-files-v1"
        let fingerprint = String(repeating: "0123456789abcdef", count: 4)
        let fingerprinted = try decode(wire(policy: rules + ":" + fingerprint))
        check(fingerprinted.policy == rules + ":" + fingerprint && fingerprinted.matchesDiscoveryPolicy(rules),
              "Snapshot fingerprint survives policy identity validation")
        let included = try decode(wire(policy: all), requestedPolicy: all)
        check(included.matchesDiscoveryPolicy(all) && !included.matchesDiscoveryPolicy(rules),
              "Include-excluded receipt remains a distinct requested scope")
        try refuse(wire(policy: rules), "Narrower rules receipt for include-excluded", requestedPolicy: all)
        try refuse(wire(policy: all), "Broader include-excluded receipt for repository rules")
        try refuse(wire(), "Unknown requested discovery policy", requestedPolicy: "invented")
        for policy in ["fixture-policy", "product-defaults/no-rule-files-v1", rules + ":", rules + "#" + fingerprint,
                       rules + ":" + String(fingerprint.dropLast()), rules + ":" + fingerprint + "0",
                       rules + ":" + fingerprint.uppercased(), rules + ":" + String(repeating: "g", count: 64)] {
            try refuse(wire(policy: policy), "Unknown or malformed discovery identity " + policy)
        }
        try refuse(wire(policy: all + ":" + fingerprint), "Include-excluded never carries repository rules", requestedPolicy: all)
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
