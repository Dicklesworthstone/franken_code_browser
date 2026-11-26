import Foundation

/// Production locator and navigation-transaction regressions. These metadata
/// fixtures do not qualify filesystem discovery, native grants, or atlas pixels.
@main struct AtlasReaderAtlasNavigationTests {
    private struct Input {
        var project = UUID()
        var root = "/project"
        var grant: NSObject? = NSObject()
        var browser = NSObject()
        var visit = UUID()
        var activation = UUID()
        var captureRoot = "/project"
        var relativePath: [UInt8]
        var identity: AtlasReaderIdentity

        init(path: [UInt8] = Array("zz-late/README.md".utf8), absolute: Bool = false) {
            relativePath = path
            identity = AtlasReaderIdentity(owner: 7, file: 20_001, revision: 3, capturedBytes: 64,
                pathBytes: absolute ? Array("/project/".utf8) + path : path,
                encoding: "utf8", displayPath: "display label, not a pathname")
        }
        func locator() -> AtlasReaderAtlasNavigation? {
            AtlasReaderAtlasNavigation(project: project, root: root, grant: grant.map(ObjectIdentifier.init),
                browser: ObjectIdentifier(browser), visit: visit, activation: activation,
                captureRoot: captureRoot, relativePath: relativePath, identity: identity)
        }
    }

    private static func identity(_ original: AtlasReaderIdentity, owner: UInt64? = nil,
        file: UInt64? = nil, revision: UInt64? = nil, bytes: UInt64? = nil,
        path: [UInt8]? = nil, encoding: String? = nil, display: String? = nil) -> AtlasReaderIdentity {
        AtlasReaderIdentity(owner: owner ?? original.owner, file: file ?? original.file,
            revision: revision ?? original.revision, capturedBytes: bytes ?? original.capturedBytes,
            pathBytes: path ?? original.pathBytes, encoding: encoding ?? original.encoding,
            displayPath: display ?? original.displayPath)
    }

    static func lateFilesDoNotRequireAtlasMembership() throws {
        let input = Input(), target = input.locator()!
        let earlier = Array("src/first.rs".utf8)
        let partialCatalog = AtlasProjectCatalog(entries: [.init(id: 1, rawPath: earlier,
            displayPath: "src/first.rs", sourcePath: "src/first.rs", observedBytes: 12,
            x: 0, y: 0, width: 10, height: 10)], discoveryComplete: false,
            policy: "repository-rules-v1", requestedRoot: input.root)
        precondition(!partialCatalog.discoveryComplete
            && !partialCatalog.entries.contains(where: { $0.rawPath == input.relativePath }))
        let destination = target.fileDestination!
        precondition(destination.catalogTarget.file?.rawPath == input.relativePath
            && destination.catalogTarget.containsFile(input.relativePath),
            "a retained reader beyond the first atlas catalog can name an exact single-file destination")
        precondition(destination.fileScope == .all && destination.customExtensions.isEmpty,
            "an independent Quick Open or source-link selection must not inherit a hiding file-type filter")
        precondition(target.directory?.rawPath == Array("zz-late".utf8)
            && target.directoryDestination?.catalogTarget == .directory(target.directory!)
            && target.directoryDestination?.fileScope == .all,
            "containing-folder focus and browsing derive from the same admitted raw file")
        precondition(target.isCurrent(input.locator()), "creating a destination performs no reader or project mutation")
    }

    static func currentContextRejectsEveryRetiredDomain() {
        let input = Input(), accepted = input.locator()!
        var next = input; next.project = UUID()
        precondition(!accepted.isCurrent(next.locator()), "a replacement project cannot reuse a queued locate action")
        next = input; next.grant = NSObject()
        precondition(!accepted.isCurrent(next.locator()), "an equal root with a replacement grant is a different authority")
        next = input; next.grant = nil
        precondition(!accepted.isCurrent(next.locator()), "an absent grant cannot satisfy the previous grant identity")
        next = input; next.browser = NSObject()
        precondition(!accepted.isCurrent(next.locator()), "a replacement discovery browser retires the old locator")
        next = input; next.visit = UUID()
        precondition(!accepted.isCurrent(next.locator()), "a same-name source visit cannot consume the former visit's action")
        next = input; next.activation = UUID()
        precondition(next.visit == input.visit && next.identity.matches(input.identity)
            && !accepted.isCurrent(next.locator()),
            "Source Back then Forward cannot revive a queued action for the same retained capture")
        for changed in [identity(input.identity, owner: 8), identity(input.identity, file: 20_002),
                        identity(input.identity, revision: 4), identity(input.identity, bytes: 65),
                        identity(input.identity, encoding: "utf16le")] {
            next = input; next.identity = changed
            precondition(!accepted.isCurrent(next.locator()), "the complete accepted capture identity must remain current")
        }
        next = input; next.identity = identity(input.identity, display: "a different escaped label")
        precondition(accepted.isCurrent(next.locator()), "display labels are not source authority")
        precondition(!accepted.isCurrent(nil), "closed, busy or absent current readers cannot authorize queued work")
    }

    static func rawRootsAndCapturePathsStayExact() {
        let composed = "/r\u{e9}po", decomposed = "/re\u{301}po"
        precondition(composed == decomposed && !composed.utf8.elementsEqual(decomposed.utf8))
        var input = Input()
        input.root = composed; input.captureRoot = composed
        let accepted = input.locator()!
        var other = input; other.root = decomposed
        precondition(other.locator() == nil, "a canonically equal root cannot borrow another root's capture")
        other.captureRoot = decomposed
        precondition(other.locator() != nil && !accepted.isCurrent(other.locator()),
            "even valid replacement roots compare by Unix byte spelling")
        other = input; other.captureRoot = "/elsewhere"
        precondition(other.locator() == nil, "a retained reader from another project is not a current-project locator")

        let absolute = Input(absolute: true)
        precondition(absolute.locator()?.file?.rawPath == absolute.relativePath,
            "ordinary absolute-path reader identities preserve their project-relative destination")
        var sibling = absolute
        sibling.identity = identity(absolute.identity, path: Array("/project2/".utf8) + absolute.relativePath)
        precondition(sibling.locator() == nil, "lexical root prefixes cannot authorize a sibling project")
        sibling.identity = identity(absolute.identity, path: Array("/project/other.md".utf8))
        precondition(sibling.locator() == nil, "a reader identity must name exactly the selected relative file")

        var slashRoot = Input()
        slashRoot.root = "/"; slashRoot.captureRoot = "/"
        slashRoot.identity = identity(slashRoot.identity, path: [47] + slashRoot.relativePath)
        precondition(slashRoot.locator() != nil, "the filesystem root is joined with exactly one separator")
        var trailing = absolute
        trailing.root = "/project/"; trailing.captureRoot = "/project/"
        precondition(trailing.locator() != nil, "an accepted root spelling is not silently normalized")
    }

    static func nativeFilenamesNeverRoundtripThroughLabels() {
        let raw: [UInt8] = [115, 114, 99, 47, 255, 92, 46, 109, 100]
        let input = Input(path: raw), target = input.locator()!
        precondition(String(bytes: raw, encoding: .utf8) == nil && target.file?.rawPath == raw
            && target.directory?.rawPath == Array("src".utf8),
            "non-UTF8 filenames and Unix backslashes remain original bytes")
        var forged = input
        forged.identity = identity(input.identity, path: Array("src/\\xff\\\\.md".utf8))
        precondition(forged.locator() == nil, "an escaped label cannot replace the reader's raw pathname")

        let first = Array("docs/caf\u{e9}.md".utf8), second = Array("docs/cafe\u{301}.md".utf8)
        var a = Input(path: first), b = a
        b.relativePath = second; b.identity = identity(a.identity, path: second)
        precondition(a.locator()!.file?.rawPath != b.locator()!.file?.rawPath
            && !a.locator()!.isCurrent(b.locator()), "canonically equivalent filenames remain distinct targets")
        a.identity = identity(a.identity, path: second)
        precondition(a.locator() == nil, "canonical string equality cannot replace exact capture-path admission")
    }

    static func invalidAndNarrowerPathsNeverBroadenScope() {
        for raw in [[UInt8](), Array("/absolute.md".utf8), Array("src/../escape.md".utf8),
                    Array("src/./file.md".utf8), Array("src//file.md".utf8), Array("src/".utf8), [97, 0, 98]] {
            precondition(Input(path: raw).locator() == nil,
                "invalid relative identities are rejected before any destination is derived")
        }
        let input = Input()
        for bad in [identity(input.identity, owner: 0), identity(input.identity, file: 0),
                    identity(input.identity, revision: 0)] {
            var invalid = input; invalid.identity = bad
            precondition(invalid.locator() == nil, "unowned source identities cannot become locators")
        }
        var invalidRoot = input; invalidRoot.root = "project"; invalidRoot.captureRoot = "project"
        precondition(invalidRoot.locator() == nil)
        invalidRoot.root = "/bad\0root"; invalidRoot.captureRoot = invalidRoot.root
        precondition(invalidRoot.locator() == nil)
        invalidRoot.root = "/" + String(repeating: "a", count: 16_384); invalidRoot.captureRoot = invalidRoot.root
        precondition(invalidRoot.locator() == nil)

        let boundary = Input(path: Array(repeating: 97, count: 2048)).locator()!
        precondition(boundary.file?.rawPath.count == 2048 && boundary.directory == .project,
            "the exact-file pathname bound remains inclusive")
        let longName = Input(path: Array("src/".utf8) + Array(repeating: 97, count: 2048)).locator()!
        precondition(longName.file == nil && longName.fileDestination == nil
            && longName.directory?.rawPath == Array("src".utf8),
            "a valid reader with an overlong filename can still locate its admissible containing folder")
        let longFolder = Input(path: Array(repeating: 97, count: 2049) + Array("/file.md".utf8)).locator()!
        precondition(longFolder.file == nil && longFolder.directory == nil
            && longFolder.directoryDestination == nil,
            "an oversized containing folder cannot silently broaden to the project root")
        let deepest = Array((Array(repeating: "d", count: 31) + ["file.md"]).joined(separator: "/").utf8)
        let allowed = Input(path: deepest).locator()!
        precondition(allowed.file != nil && allowed.directory?.rawPath.split(separator: 47).count == 31,
            "the 32nd file component and its 31 containing folders remain admitted")
        let tooDeep = Input(path: Array("d/".utf8) + deepest).locator()!
        precondition(tooDeep.file == nil && tooDeep.directory == nil,
            "out-of-profile depth offers no atlas or folder action")
        precondition(Input(path: Array(repeating: 97, count: 16_385)).locator() == nil,
            "even metadata-only locators retain the reader pathname bound")
    }

    static func focusCommitAndBackPreserveTheOriginalFilterAndCamera() throws {
        let input = Input(), locator = input.locator()!, focused = locator.fileDestination!
        let original = try AtlasFolderDestination(directory: AtlasDirectoryFocus(rawPath: Array("src".utf8)),
            fileScope: .custom, customExtensions: "rs,py", anchorRawPath: Array("src/main.rs".utf8),
            cameraAnchor: AtlasParcelCameraAnchor(relativeX: 0.2, relativeY: 0.7, zoomFactor: 3)!)
        precondition(!original.fileScope.includes("zz-late/README.md", custom: original.customExtensions)
            && focused.fileScope.includes("zz-late/README.md", custom: focused.customExtensions),
            "explicit focus must reveal a selected file hidden by the previous type filter")
        var navigation = AtlasFolderNavigation()
        navigation.reset(to: original)
        let canceled = navigation.visit(focused, from: original)
        precondition(navigation.current == original && !navigation.canBack && !navigation.canForward,
            "starting or canceling focus must not change the accepted filter, camera, or history")
        let wrong = locator.directoryDestination!
        precondition(!navigation.commit(canceled, resolved: wrong) && navigation.current == original,
            "a parent-folder result cannot consume a request for the exact file")
        let accepted = navigation.visit(focused, from: original)
        precondition(navigation.commit(accepted, resolved: focused)
            && navigation.current?.fileScope == .all && !navigation.accepts(canceled))
        let back = navigation.back()!
        precondition(back.target == original && navigation.current == focused,
            "Back is staged without prematurely replacing the focused file")
        precondition(navigation.commit(back, resolved: back.target) && navigation.current == original,
            "accepted Back restores the exact former custom filter and camera anchor")
        let forward = navigation.forward()!
        precondition(forward.target == focused && navigation.commit(forward, resolved: forward.target))
        precondition(locator.isCurrent(input.locator()),
            "map history must not mutate the accepted source visit or treat new metadata as captured bytes")

        let folder = locator.directoryDestination!, move = navigation.visit(folder, from: focused)
        precondition(navigation.commit(move, resolved: folder) && navigation.current == folder
            && navigation.back()?.target == focused,
            "file and containing-folder focus remain independently reversible destinations")
    }

    static func main() throws {
        try lateFilesDoNotRequireAtlasMembership()
        currentContextRejectsEveryRetiredDomain()
        rawRootsAndCapturePathsStayExact()
        nativeFilenamesNeverRoundtripThroughLabels()
        invalidAndNarrowerPathsNeverBroadenScope()
        try focusCommitAndBackPreserveTheOriginalFilterAndCamera()
        print("6 retained-reader atlas navigation regression cases passed")
    }
}
