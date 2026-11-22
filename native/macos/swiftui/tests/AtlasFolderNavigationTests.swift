import Foundation

@main struct AtlasFolderNavigationTests {
    static func place(_ directory: String = "", fileScope: AtlasFileScope = .all,
        customExtensions: String = "", anchor: AtlasParcelCameraAnchor? = nil) -> AtlasFolderDestination {
        let raw = Array(directory.utf8)
        return try! AtlasFolderDestination(directory: AtlasDirectoryFocus(rawPath: raw),
            fileScope: fileScope, customExtensions: customExtensions,
            anchorRawPath: anchor.map { _ in raw + (raw.isEmpty ? [] : [47]) + Array("file.rs".utf8) },
            cameraAnchor: anchor)
    }

    static func reject(_ message: String, _ body: () throws -> Void) {
        do { try body(); preconditionFailure(message) } catch { }
    }

    static func visit(_ destination: AtlasFolderDestination, _ navigation: inout AtlasFolderNavigation) {
        precondition(navigation.commit(navigation.visit(destination), resolved: destination))
    }

    static func transactionalOriginAndRoundTrip() {
        var navigation = AtlasFolderNavigation()
        let root = place(), a = place("a"), b = place("b")
        let changedRoot = place(anchor: AtlasParcelCameraAnchor(relativeX: 0.25, relativeY: 0.75, zoomFactor: 3)!)
        navigation.reset(to: root)
        let move = navigation.visit(a, from: changedRoot)
        precondition(navigation.current == root && !navigation.canBack && !navigation.canForward,
            "preparing a catalog cannot change the current camera origin")
        precondition(navigation.commit(move, resolved: a))
        precondition(navigation.back()?.target == changedRoot, "accepted departure remembers the explored origin")
        visit(b, &navigation)
        let back = navigation.back()!
        precondition(navigation.commit(back, resolved: back.target))
        precondition(navigation.current == a && navigation.back()?.target == changedRoot
            && navigation.forward()?.target == b, "folder history retains both directions")
        let forward = navigation.forward()!
        precondition(navigation.commit(forward, resolved: forward.target))
        precondition(navigation.current == b && navigation.retainedCount == 2)
    }

    static func failureCancellationAndBranching() {
        var navigation = AtlasFolderNavigation()
        let root = place(), a = place("a"), b = place("b"), branch = place("other")
        for destination in [root, a, b] { visit(destination, &navigation) }
        let updatedB = place("b", anchor: AtlasParcelCameraAnchor(relativeX: 0.6, relativeY: 0.2, zoomFactor: 4)!)
        let canceled = navigation.back(from: updatedB)!
        precondition(navigation.current == b && navigation.retainedCount == 2 && !navigation.canForward,
            "a canceled back request does not update current or consume its target")
        precondition(navigation.back()?.target == a && navigation.accepts(canceled))
        let back = navigation.back(from: updatedB)!
        precondition(navigation.commit(back, resolved: a))
        precondition(navigation.forward()?.target == updatedB && !navigation.accepts(canceled))
        _ = navigation.visit(branch)
        precondition(navigation.forward()?.target == updatedB, "a failed new branch preserves Forward")
        visit(branch, &navigation)
        precondition(!navigation.canForward && navigation.back()?.target == a)
    }

    static func proposalIsolationAndRefresh() throws {
        var navigation = AtlasFolderNavigation(), other = AtlasFolderNavigation()
        let root = place(), a = place("a")
        for destination in [root, a] { visit(destination, &navigation); visit(destination, &other) }
        let move = navigation.back()!
        precondition(!other.commit(move, resolved: root), "another window cannot accept this proposal")
        _ = try navigation.addLandmark(label: "Folder A", destination: a)
        navigation.rebaseCurrent(a)
        precondition(!navigation.accepts(move) && !navigation.commit(move, resolved: root),
            "even a lookalike refresh invalidates old proposals")
        precondition(navigation.back()?.target == root && navigation.landmarks.count == 1,
            "same-root refresh preserves semantic routes and landmarks")
        let fresh = navigation.back()!
        precondition(navigation.commit(fresh, resolved: root))
        precondition(!navigation.commit(fresh, resolved: root), "a proposal commits only once")
        let beforeClose = navigation.forward()!
        navigation.reset(to: root)
        precondition(navigation.current == root && !navigation.canBack && !navigation.canForward
            && navigation.landmarks.isEmpty && !navigation.accepts(beforeClose),
            "root/grant/policy replacement retires the whole session domain")
    }

    static func acceptedScopeAndMissingAnchor() {
        var navigation = AtlasFolderNavigation()
        visit(place(), &navigation)
        let saved = place("src", fileScope: .rust,
            anchor: AtlasParcelCameraAnchor(relativeX: 0.4, relativeY: 0.8, zoomFactor: 2)!)
        let move = navigation.visit(saved)
        precondition(!navigation.commit(move, resolved: place("other", fileScope: .rust)),
            "a different accepted directory cannot consume this request")
        precondition(!navigation.commit(move, resolved: place("src", fileScope: .all)),
            "a competing filter cannot consume this request")
        let overview = place("src", fileScope: .rust)
        precondition(navigation.commit(move, resolved: overview),
            "an absent parcel resolves to the accepted directory overview")
        precondition(navigation.current?.anchorRawPath == nil && navigation.current?.cameraAnchor == nil)
    }

    static func rawIdentityAndValidation() throws {
        let directory = try AtlasDirectoryFocus(rawPath: [0xfe, 47, 92])
        let raw = directory.rawPath + [47, 0xff, 46, 114, 115]
        let anchor = AtlasParcelCameraAnchor(relativeX: -2, relativeY: 3, zoomFactor: 0.8)!
        let exact = try AtlasFolderDestination(directory: directory, fileScope: .all,
            customExtensions: "", anchorRawPath: raw, cameraAnchor: anchor)
        precondition(exact.anchorRawPath == raw && exact.directory.rawPath == [0xfe, 47, 92],
            "non-UTF8 names and Unix backslashes remain original bytes")
        reject("anchor must belong to directory") {
            _ = try AtlasFolderDestination(directory: directory, fileScope: .all, customExtensions: "",
                anchorRawPath: Array("other/file.rs".utf8), cameraAnchor: anchor)
        }
        reject("path prefix is not a component boundary") {
            _ = try AtlasFolderDestination(directory: AtlasDirectoryFocus(rawPath: Array("src".utf8)),
                fileScope: .all, customExtensions: "", anchorRawPath: Array("src2/file.rs".utf8), cameraAnchor: anchor)
        }
        for rawPath in [[UInt8](), Array("../file".utf8), Array("/file".utf8), [97, 0, 98]] {
            reject("invalid relative file identity") {
                _ = try AtlasFolderDestination(directory: .project, fileScope: .all, customExtensions: "",
                    anchorRawPath: rawPath, cameraAnchor: anchor)
            }
        }
        reject("path-only anchors have no camera semantics") {
            _ = try AtlasFolderDestination(directory: .project, fileScope: .all, customExtensions: "", anchorRawPath: raw)
        }
        reject("camera-only anchors have no file identity") {
            _ = try AtlasFolderDestination(directory: .project, fileScope: .all, customExtensions: "", cameraAnchor: anchor)
        }
        reject("oversized paths cannot enter bounded history") {
            _ = try AtlasFolderDestination(directory: .project, fileScope: .all, customExtensions: "",
                anchorRawPath: Array(repeating: 97, count: 16_385), cameraAnchor: anchor)
        }
        let composed = place(customExtensions: "\u{e9}"), decomposed = place(customExtensions: "e\u{301}")
        precondition(composed != decomposed
            && !composed.matchesScope(fileScope: .all, customExtensions: "e\u{301}"),
            "raw custom-text identity cannot collapse Unicode-normalized lookalikes")
        reject("dormant custom text also has a byte bound") {
            _ = try AtlasFolderDestination(directory: .project, fileScope: .all,
                customExtensions: String(repeating: "a", count: 1025))
        }
        reject("custom filters use the production admission predicate") {
            _ = try AtlasFolderDestination(directory: .project, fileScope: .custom, customExtensions: "é")
        }
    }

    static func boundedNamedLandmarks() throws {
        var navigation = AtlasFolderNavigation(landmarkCapacity: Int.max)
        let destination = place("src")
        for index in 0..<32 { _ = try navigation.addLandmark(label: "Place \(index)", destination: destination) }
        precondition(navigation.landmarks.count == 32 && Set(navigation.landmarks.map(\.id)).count == 32)
        reject("landmark admission cannot exceed its hard cap") {
            _ = try navigation.addLandmark(label: "Overflow", destination: destination)
        }
        let first = navigation.landmarks[0]
        try navigation.renameLandmark(id: first.id, label: "  New name  ")
        precondition(navigation.landmarks[0].id == first.id && navigation.landmarks[0].label == "New name"
            && navigation.landmarks[0].destination == destination, "rename retains the semantic place")
        precondition(navigation.removeLandmark(id: first.id) && !navigation.removeLandmark(id: first.id))
        let labelAtLimit = String(repeating: "é", count: 64)
        _ = try navigation.addLandmark(label: labelAtLimit, destination: destination)
        for label in ["", "   ", "a\nb", "name\u{202e}", String(repeating: "é", count: 65)] {
            reject("invalid or oversized labels cannot replace accepted ones") {
                try navigation.renameLandmark(id: navigation.landmarks[0].id, label: label)
            }
        }
        reject("renaming a missing landmark cannot recreate it") {
            try navigation.renameLandmark(id: first.id, label: "Gone")
        }
        var disabled = AtlasFolderNavigation(landmarkCapacity: -1)
        reject("zero landmark capacity is respected") {
            _ = try disabled.addLandmark(label: "No capacity", destination: destination)
        }
    }

    static func boundedHistoryAndNoOpVisit() {
        var navigation = AtlasFolderNavigation(historyCapacity: 3)
        for index in 0..<100 {
            visit(place("folder\(index)"), &navigation)
            precondition(navigation.retainedCount <= 3)
        }
        for _ in 0..<3 {
            let move = navigation.back()!
            precondition(navigation.commit(move, resolved: move.target))
        }
        precondition(!navigation.canBack && navigation.retainedCount == 3)
        let current = navigation.current!, next = navigation.forward()?.target
        visit(current, &navigation)
        precondition(navigation.forward()?.target == next && navigation.retainedCount == 3,
            "opening the same saved place does not destroy Forward")
    }

    static func main() throws {
        transactionalOriginAndRoundTrip()
        failureCancellationAndBranching()
        try proposalIsolationAndRefresh()
        acceptedScopeAndMissingAnchor()
        try rawIdentityAndValidation()
        try boundedNamedLandmarks()
        boundedHistoryAndNoOpVisit()
        print("7 folder navigation regression groups passed")
    }
}
