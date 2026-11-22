import Foundation

/// Production directory codec and opening-state checks. No filesystem,
/// discovery engine, SwiftUI window or native presentation is exercised here.
@main private enum AtlasDirectoryFocusTests {
    @MainActor private static var checks = 0
    @MainActor private static func check(_ value: @autoclosure () -> Bool, _ message: String) {
        precondition(value(), message); checks += 1
    }
    private static func hex(_ bytes: [UInt8]) -> String {
        bytes.map { ($0 < 16 ? "0" : "") + String($0, radix: 16) }.joined()
    }
    private static func wire(directory: AtlasDirectoryFocus, paths: [[UInt8]],
        complete: Bool = true, policy: String = "repository-rules-v1") -> [String: Any] {
        let files: [[String: Any]] = paths.enumerated().map { index, raw in
            ["file_id": String(index + 1), "observed_bytes": "17",
             "path": ["encoding": "unix-bytes", "hex": hex(raw), "display": "untrusted file label"],
             "x": index * 10, "y": 0, "w": 8, "h": 8]
        }
        return ["schema": "fcb.project-directory-catalog/1", "status": "ok", "command": "directory-catalog",
                "identity_scope": "response-local", "native_presented": false, "source_payload_read": false,
                "payload_bytes_read": "0", "read_calls": "0", "catalogued_files": String(files.count),
                "discovery_complete": complete, "policy": policy, "world": ["w": 4096, "h": 4096],
                "path_basis": "project-root",
                "directory": ["encoding": "unix-bytes", "hex": hex(directory.rawPath), "display": "label only"],
                "files": files]
    }
    private static func decode(_ object: [String: Any], directory: AtlasDirectoryFocus,
        policy: String = "repository-rules-v1") throws -> AtlasProjectCatalog {
        try AtlasProjectCatalog.decodeDirectory(JSONSerialization.data(withJSONObject: object),
            requestedRoot: "/project", directory: directory, requestedPolicy: policy)
    }
    @MainActor private static func refuse(_ object: [String: Any], directory: AtlasDirectoryFocus,
        policy: String = "repository-rules-v1") throws {
        do { _ = try decode(object, directory: directory, policy: policy); preconditionFailure("Invalid directory receipt admitted") }
        catch is AtlasProjectCatalog.DecodeError { checks += 1 }
    }

    @MainActor static func main() throws {
        let src = try AtlasDirectoryFocus(rawPath: Array("src".utf8))
        let nested = try AtlasDirectoryFocus(rawPath: Array("src/native/ui".utf8))
        let root = try AtlasDirectoryFocus(rawPath: [])
        check(root == .project && root.parent == nil && root.display == "Project", "Root is a separate sentinel")
        check(nested.parent?.rawPath == Array("src/native".utf8) && src.parent == .project, "Parents preserve original-root-relative bytes")
        let containing = try AtlasDirectoryFocus.containingFile(rawPath: Array("src/native/a.rs".utf8))
        let topFile = try AtlasDirectoryFocus.containingFile(rawPath: Array("README.md".utf8))
        check(containing.rawPath == Array("src/native".utf8) && topFile == .project, "Containing folder derives from raw file identity")
        check(src.containsFile(Array("src/a.rs".utf8)) && src.containsFile(Array("src/native/a.rs".utf8)), "Immediate and deep descendants")
        for path in ["src", "src/", "src2/a.rs", "sra/a.rs", "a.rs", "/src/a.rs", "src/../a.rs", "src//a.rs", ""] {
            check(!src.containsFile(Array(path.utf8)), "No equal path, prefix sibling, traversal or empty member")
        }
        check(root.containsFile(Array("src/a.rs".utf8)) && !root.containsFile(Array("../a.rs".utf8)), "Root still requires a normalized file")
        for raw in [[UInt8]([0]), Array("/src".utf8), Array("src/".utf8), Array("src//a".utf8),
                    Array(".".utf8), Array("..".utf8), Array("a/./b".utf8), Array("a/../b".utf8),
                    Array(repeating: UInt8(97), count: 2049)] {
            do { _ = try AtlasDirectoryFocus(rawPath: raw); preconditionFailure("Invalid directory admitted") }
            catch is AtlasDirectoryFocus.ValidationError { checks += 1 }
        }
        let boundary = try AtlasDirectoryFocus(rawPath: Array(repeating: 97, count: 2048))
        check(boundary.rawPath.count == 2048, "Directory cap is inclusive")
        let punctuation = try AtlasDirectoryFocus(rawPath: Array("drive:name\\folder".utf8))
        check(punctuation.parent == .project, "Unix colon and backslash are ordinary path bytes")
        let composed = try AtlasDirectoryFocus(rawPath: Array("é".utf8))
        let decomposed = try AtlasDirectoryFocus(rawPath: Array("e\u{301}".utf8))
        check(composed != decomposed && Set([composed, decomposed]).count == 2, "Canonical-equivalent Unicode names remain different")
        let escaped = try AtlasDirectoryFocus(rawPath: Array("a\n\u{202e}\\b".utf8))
        check(!escaped.display.contains("\n") && !escaped.display.contains("\u{202e}")
            && escaped.display.contains("\\u{a}") && escaped.display.contains("\\u{202e}"), "Controls and bidi formatting cannot forge rows")
        let rawDirectory = try AtlasDirectoryFocus(rawPath: [255, 47, 115])
        check(rawDirectory.display == "\\xff/s" && rawDirectory.parent?.rawPath == [255], "Raw names remain addressable with escaped labels")

        let good = wire(directory: src, paths: [Array("src/a.rs".utf8), Array("src/nested/b.rs".utf8)], complete: false)
        let report = try decode(good, directory: src)
        check(report.requestedDirectory == src && report.requestedRoot == "/project"
            && !report.discoveryComplete && report.entries.count == 2, "Receipt remains bound to requested root and directory")
        check(report.entries[0].rawPath == Array("src/a.rs".utf8) && report.entries[0].sourcePath == "src/a.rs",
            "Files retain full original-root-relative paths, never directory-relative paths")
        let raw = try decode(wire(directory: rawDirectory, paths: [rawDirectory.rawPath + [47, 255]]), directory: rawDirectory)
        check(raw.entries[0].sourcePath == nil && raw.requestedDirectory == rawDirectory, "Non-UTF8 directory/file identities survive")
        let completeEmpty = try decode(wire(directory: src, paths: []), directory: src)
        let partialEmpty = try decode(wire(directory: src, paths: [], complete: false), directory: src)
        check(completeEmpty.discoveryComplete && !partialEmpty.discoveryComplete, "Empty and unknown membership remain distinct")
        let all = "all-regular-files/no-rule-files-v1"
        let included = try decode(wire(directory: src, paths: [], policy: all), directory: src, policy: all)
        check(included.matchesDiscoveryPolicy(all), "Directory transport can bind include-excluded scope")
        try refuse(good, directory: src, policy: all)
        try refuse(wire(directory: src, paths: [], policy: all), directory: src)
        for (key, value) in [("schema", "fcb.project-catalog/1"), ("command", "catalog"), ("path_basis", "directory"),
                             ("status", "error"), ("payload_bytes_read", "1"), ("read_calls", "1")] {
            var bad = good; bad[key] = value; try refuse(bad, directory: src)
        }
        for key in ["directory", "path_basis"] {
            var bad = good; bad.removeValue(forKey: key); try refuse(bad, directory: src)
        }
        for echo in [["encoding": "unix-bytes", "hex": hex(Array("other".utf8)), "display": "src"],
                     ["encoding": "lossy", "hex": hex(src.rawPath), "display": "src"],
                     ["encoding": "unix-bytes", "hex": "737263", "display": ""],
                     ["encoding": "unix-bytes", "hex": "73726A", "display": "src"],
                     ["encoding": "unix-bytes", "hex": "737263", "display": String(repeating: "x", count: 16_385)]] {
            var bad = good; bad["directory"] = echo; try refuse(bad, directory: src)
        }
        for path in ["a.rs", "src2/a.rs", "src", "src/../a.rs"] {
            try refuse(wire(directory: src, paths: [Array(path.utf8)]), directory: src)
        }
        try refuse(good, directory: .project)
        let data = try JSONSerialization.data(withJSONObject: good)
        do { _ = try AtlasProjectCatalog.decode(data, requestedRoot: "/project"); preconditionFailure("Directory schema accepted as root") }
        catch is AtlasProjectCatalog.DecodeError { checks += 1 }
        do {
            _ = try AtlasProjectCatalog.decodeDirectory(Data(repeating: 32, count: AtlasProjectCatalog.maximumResponseBytes + 1),
                requestedRoot: "/project", directory: src)
            preconditionFailure("Oversized directory response admitted")
        } catch AtlasProjectCatalog.DecodeError.invalidResponse { checks += 1 }

        // The candidate is a value copy. A refused layout can discard it while
        // old response-local row IDs continue to address the accepted catalog.
        var opening = AtlasProjectOpening()
        let ticket = opening.begin()
        check(opening.acceptProgressive(for: ticket), "First page authorizes project browsing")
        let emptyRevision = opening.catalogRevision
        var staged = opening
        check(staged.replaceAtlasCatalog(report, in: ticket.project, replacing: emptyRevision, hasOpenableFiles: true),
            "First focused atlas can be privately prepared")
        check(opening.catalog == nil && opening.catalogRevision == emptyRevision && !opening.hasAtlas,
            "Staging does not mutate accepted project state")
        opening = staged
        let firstRevision = opening.catalogRevision
        check(firstRevision != emptyRevision && opening.hasAtlas && opening.projectID == ticket.project,
            "Publication advances catalog identity without replacing the project")
        let otherDirectory = try AtlasDirectoryFocus(rawPath: Array("other".utf8))
        let replacement = try decode(wire(directory: otherDirectory, paths: [Array("other/a.rs".utf8)]), directory: otherDirectory)
        staged = opening
        check(staged.replaceAtlasCatalog(replacement, in: ticket.project, replacing: firstRevision, hasOpenableFiles: true),
            "Existing atlas accepts a bounded replacement")
        check(opening.file(1, in: ticket.project, catalogRevision: firstRevision)?.rawPath == Array("src/a.rs".utf8),
            "Old accepted row remains correct while replacement is private")
        opening = staged
        check(opening.file(1, in: ticket.project, catalogRevision: firstRevision) == nil
            && opening.file(1, in: ticket.project, catalogRevision: opening.catalogRevision)?.rawPath == Array("other/a.rs".utf8),
            "Reused response-local IDs cannot activate an old row")
        let acceptedRevision = opening.catalogRevision
        check(!opening.replaceAtlasCatalog(report, in: UUID(), replacing: acceptedRevision, hasOpenableFiles: true)
            && !opening.replaceAtlasCatalog(report, in: ticket.project, replacing: firstRevision, hasOpenableFiles: true),
            "Wrong project and stale catalog cannot replace the accepted atlas")
        check(opening.catalogRevision == acceptedRevision && opening.catalog?.requestedDirectory == otherDirectory,
            "Refusal leaves accepted membership untouched")
        opening.close()
        check(opening.catalogRevision != acceptedRevision && opening.file(1, in: ticket.project, catalogRevision: acceptedRevision) == nil,
            "Close revokes project and catalog actions")

        var preparing = AtlasProjectOpening()
        let firstPage = preparing.begin()
        check(preparing.acceptProgressive(for: firstPage), "Separate project becomes ready")
        let preview = preparing.beginPreviews()!
        let previewRevision = preparing.catalogRevision
        check(!preparing.replaceAtlasCatalog(report, in: firstPage.project, replacing: previewRevision, hasOpenableFiles: true),
            "Directory replacement cannot take over active preview work")
        check(preparing.acceptPreviewCatalog(report, for: preview) && preparing.catalogRevision != previewRevision,
            "Preview publication also invalidates old row closures")
        preparing.cancel()
        check(preparing.catalog?.requestedDirectory == src, "Cancel retains accepted preview membership")
        print("AtlasDirectoryFocusTests: \(checks) checks passed")
    }
}
