import Foundation

private final class EditorGrant {}
@MainActor private final class EditorGate {
    private var reply: CheckedContinuation<Void, Never>?
    private(set) var entered = false
    func wait() async {
        await withCheckedContinuation { reply = $0; entered = true }
    }
    func release() { let held = reply; reply = nil; held?.resume() }
}

/// Exercise production authority/lifecycle and the exact structured URL pair
/// supplied to the OS adapter. This does not launch an editor or qualify AppKit.
@MainActor @main struct AtlasEditorHandoffTests {
    static func until(_ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(5)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 1_000_000) }
        precondition(condition(), "Editor action did not drain")
    }
    static func main() async throws {
        try exactNativePaths()
        try await configuredStructuredPayload()
        try await cancellationBeforeEntry()
        try await staleChoiceAndBoundedDrain()
        try await submittedActionKeepsBothGrants()
        try await failedSubmissionCanRetry()
        print("Editor handoff: exact URL payloads, explicit configuration, stale authority, bounded cancellation and grant lifetimes passed")
    }
    static func app(_ name: String = "Chosen Editor") throws -> AtlasEditorApplication {
        try .init(selected: URL(fileURLWithPath: "/Applications/\(name).app", isDirectory: true))
    }
    static func exactNativePaths() throws {
        let path = "/repo/-file;$(touch x) 'quoted' %2F #question?.rs"
        let url = try AtlasEditorPath.file(path)
        precondition(url.path.utf8.elementsEqual(path.utf8), "Punctuation must stay one literal document URL")
        let composed = "/repo/caf\u{e9}.rs", decomposed = "/repo/cafe\u{301}.rs"
        let a = try AtlasEditorPath.file(composed), b = try AtlasEditorPath.file(decomposed)
        precondition(a.path.utf8.elementsEqual(composed.utf8) && b.path.utf8.elementsEqual(decomposed.utf8)
            && !a.path.utf8.elementsEqual(b.path.utf8), "Canonical equivalence is not native path identity")
        for invalid in ["", "/", "relative.rs", "file:///repo/a.rs", "/repo/../a", "/repo/./a",
                        "/repo//a", "/repo/a/", "/repo/a\0b", "/repo/a\nb", "/repo/a\rb",
                        "/repo/\u{85}a", "/repo/\u{2028}a", "/repo/\u{202e}a", "/repo/\u{2066}a",
                        "/repo/\u{61c}a", "/" + String(repeating: "a", count: 16_384),
                        "/" + Array(repeating: "a", count: 257).joined(separator: "/")] {
            do { _ = try AtlasEditorPath.file(invalid); preconditionFailure("Unsafe source admitted") }
            catch is AtlasEditorError {}
        }
        for invalid in ["https://example.com/Editor.app", "file://other-host/Applications/Editor.app",
                        "file:///Applications/Editor.app?args=unsafe", "file:///Applications/Editor.app#fragment",
                        "file:///Applications/Editor.command"] {
            do { _ = try AtlasEditorApplication(selected: URL(string: invalid)!); preconditionFailure("Unsafe editor admitted") }
            catch is AtlasEditorError {}
        }
    }
    static func configuredStructuredPayload() async throws {
        let configuration = AtlasEditorConfiguration()
        var choices = 0, submitted: [(String, String)] = []
        let source = try AtlasEditorSource(path: "/repo/a b;$(literal)%2e.rs", accessLease: nil, isCurrent: { true })
        let handoff = AtlasEditorHandoff(configuration: configuration,
            choose: { choices += 1; return try app("Editor \(choices)") },
            submit: { application, document in submitted.append((application.path, document.path)) })
        handoff.open(source)
        try await until { !handoff.busy }
        precondition(choices == 1 && submitted.count == 1
            && submitted[0].0 == "/Applications/Editor 1.app"
            && submitted[0].1.utf8.elementsEqual(source.url.path.utf8))
        handoff.open(source)
        try await until { !handoff.busy }
        precondition(choices == 1 && submitted.count == 2, "Only an explicit new choice should replace the configured editor")
        handoff.open(source, chooseAnother: true)
        try await until { !handoff.busy }
        precondition(choices == 2 && submitted.count == 3 && submitted[2].0 == "/Applications/Editor 2.app")
        precondition(handoff.notice.contains("current disk file") && handoff.notice.contains("Retained source bytes are unchanged"))
    }
    static func cancellationBeforeEntry() async throws {
        for cancel in [false, true] {
            var current = true, choices = 0, submissions = 0
            let source = try AtlasEditorSource(path: "/repo/current.rs", accessLease: nil, isCurrent: { current })
            let handoff = AtlasEditorHandoff(configuration: .init(),
                choose: { choices += 1; return try app() }, submit: { _, _ in submissions += 1 })
            handoff.open(source)
            if cancel { handoff.cancel() } else { current = false }
            try await until { !handoff.busy }
            precondition(choices == 0 && submissions == 0,
                "Closing/canceling before Task entry must never show a late application chooser")
        }
    }
    static func staleChoiceAndBoundedDrain() async throws {
        for cancel in [false, true] {
            let gate = EditorGate(), configuration = AtlasEditorConfiguration()
            var current = true, choices = 0, submissions = 0
            var grant: EditorGrant? = EditorGrant(); weak var held = grant
            var source: AtlasEditorSource? = try .init(path: "/repo/old.rs", accessLease: grant, isCurrent: { current })
            let handoff = AtlasEditorHandoff(configuration: configuration,
                choose: { choices += 1; if choices == 1 { await gate.wait() }; return try app() },
                submit: { _, _ in submissions += 1 })
            handoff.open(source!)
            try await until { gate.entered }
            source = nil; grant = nil
            precondition(held != nil, "Chooser must retain the source grant after reader closure")
            if cancel { handoff.cancel() } else { current = false }
            let other = try AtlasEditorSource(path: "/repo/new.rs", accessLease: nil, isCurrent: { true })
            for _ in 0..<20 { handoff.open(other) }
            precondition(handoff.busy && choices == 1 && held != nil,
                "Canceled or stale work must drain before another chooser is admitted")
            gate.release()
            try await until { !handoff.busy && held == nil }
            precondition(submissions == 0 && configuration.application == nil,
                "Late choice must neither launch nor replace the configured editor")
            handoff.open(other)
            try await until { !handoff.busy }
            precondition(choices == 2 && submissions == 1)
        }
    }
    static func submittedActionKeepsBothGrants() async throws {
        let gate = EditorGate(), configuration = AtlasEditorConfiguration()
        var current = true, submissions = 0
        var fileGrant: EditorGrant? = EditorGrant(), appGrant: EditorGrant? = EditorGrant()
        weak var heldFile = fileGrant; weak var heldApp = appGrant
        var source: AtlasEditorSource? = try .init(path: "/repo/current.rs", accessLease: fileGrant, isCurrent: { current })
        configuration.application = try .init(selected: URL(fileURLWithPath: "/Applications/Chosen.app", isDirectory: true), accessLease: appGrant)
        let handoff = AtlasEditorHandoff(configuration: configuration,
            choose: { preconditionFailure("Configured action unexpectedly prompted") },
            submit: { _, _ in submissions += 1; await gate.wait() })
        handoff.open(source!)
        try await until { gate.entered }
        source = nil; fileGrant = nil; appGrant = nil; configuration.forget(); current = false
        handoff.cancel()
        precondition(handoff.phase == .opening && !handoff.canCancel && heldFile != nil && heldApp != nil,
            "An entered OS request cannot be withdrawn or lose either native grant before its completion")
        let other = try AtlasEditorSource(path: "/repo/other.rs", accessLease: nil, isCurrent: { true })
        handoff.open(other)
        precondition(submissions == 1)
        gate.release()
        try await until { !handoff.busy && heldFile == nil && heldApp == nil }
        precondition(submissions == 1 && handoff.notice.isEmpty,
            "Completion must not publish stale status into a replacement reader")
    }
    static func failedSubmissionCanRetry() async throws {
        let configuration = AtlasEditorConfiguration()
        configuration.application = try app()
        var submissions = 0
        let handoff = AtlasEditorHandoff(configuration: configuration,
            choose: { preconditionFailure("Configured action unexpectedly prompted") },
            submit: { _, _ in submissions += 1; if submissions == 1 { throw AtlasEditorError.unavailable } })
        let source = try AtlasEditorSource(path: "/repo/current.rs", accessLease: nil, isCurrent: { true })
        handoff.open(source)
        try await until { !handoff.busy }
        precondition(handoff.notice.contains("could not open"))
        handoff.open(source)
        try await until { !handoff.busy }
        precondition(submissions == 2 && handoff.notice.contains("Sent the current disk file"))
    }
}
