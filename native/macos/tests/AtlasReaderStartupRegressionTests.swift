import Foundation

/// Production startup/model/coordinator/decoder/queue tests. Only native
/// marshaling is injected; this does not qualify Rust decoding or AppKit.
@main @MainActor struct AtlasReaderStartupRegressionTests {
    static func require(_ condition: @autoclosure () -> Bool, _ message: String) {
        if !condition() { fatalError(message) }
    }
    static func wait(_ condition: () -> Bool) async throws {
        for _ in 0..<5000 {
            if condition() { return }
            try await Task.sleep(nanoseconds: 1_000_000)
        }
        fatalError("startup reader did not reach its required state")
    }
    static func settle(_ model: AtlasPagedReaderModel) async throws {
        try await wait { !model.busy }
    }

    static func startupPositionUsesOneExactCaptureAndNativeCoordinates() async throws {
        let fixture = PositionFixture()
        fixture.base.change { $0.text = "a\r\né😀\nthird\n" }
        let name = "e\u{301} source.rs"
        let hex = name.utf8.map { String(format: "%02x", $0) }.joined()
        var startup = AtlasStartupFileRequest(arguments: ["app", "-fcb.root", "/project",
            "-fcb.file-hex", hex, "-fcb.line", "3"])
        guard case .open(let path, let line) = startup.consume(root: "/project") else {
            fatalError("startup request was not admitted")
        }
        let model = AtlasPagedReaderModel(transport: fixture.transport)
        model.open(root: "/project", path: path, accessLease: nil, initialLine: line)
        try await settle(model)
        require(model.page?.position?.line == 3 && model.page?.position?.column == 1,
                "the requested physical line is the first accepted location")
        require(model.page?.position?.originalOffset == 10
                && model.nativeSelectionRange == NSRange(location: 7, length: 0),
                "CRLF, original UTF-8 bytes and native UTF-16 caret are distinct domains")
        require(model.page?.identity.pathBytes == Array(("/project/" + name).utf8),
                "startup preserves the exact filename bytes")
        require(fixture.base.snapshot.opens == [1] && fixture.snapshot.calls == [.init(handle: 1, line: 3, column: 1, kind: 1)],
                "location resolution uses the same capture and explicit scalar column one")
        require(!model.canGoBack && !model.canGoForward && model.lineInput == "3",
                "startup establishes the requested location as the initial history entry")
        require(startup.consume(root: "/project") == .none, "startup is single-use")
        model.close()
    }

    static func missingOrUntrustedPositionKeepsTheCapturedFirstPage() async throws {
        for forgeIdentity in [false, true] {
            let fixture = PositionFixture()
            fixture.base.change { $0.text = "first\nsecond\n" }
            fixture.change { $0.forgeIdentity = forgeIdentity }
            let model = AtlasPagedReaderModel(transport: fixture.transport)
            let requested: UInt64 = forgeIdentity ? 2 : UInt64.max
            model.open(root: "/project", path: "main.rs", accessLease: nil, initialLine: requested)
            try await settle(model)
            require(model.page?.text == "first\nsecond\n" && model.page?.position == nil,
                    "failed location does not publish a guessed or wrong-capture position")
            require(model.notice.contains("Could not open requested line \(requested)")
                    && model.notice.contains("Showing the first page"),
                    "requested location failure cannot look like successful line-one navigation")
            require(fixture.base.snapshot.opens == [1] && model.canRefresh,
                    "the useful capture stays readable without opening the path again")
            model.offsetInput = "6"; model.goToByte(); try await settle(model)
            require(model.page?.start == 6 && model.page?.identity.owner == 1,
                    "ordinary navigation remains available after failed startup location")
            model.close()
        }
    }

    static func unsupportedPositionAndZeroDoNotPretendSuccess() async throws {
        let fixture = AtlasReaderRefreshFixture()
        let model = AtlasPagedReaderModel(transport: fixture.transport)
        model.open(root: "/project", path: "main.rs", accessLease: nil, initialLine: 0)
        require(model.page == nil && !model.busy && fixture.snapshot.opens.isEmpty,
                "invalid one-based location is refused before capture admission")
        model.open(root: "/project", path: "main.rs", accessLease: nil, initialLine: 120)
        try await settle(model)
        require(model.page?.text == "old capture\n" && model.page?.position == nil
                && model.notice.contains("Could not open requested line 120"),
                "a missing position service preserves source with an explicit limitation")
        model.close()
    }

    static func canceledPositionCannotPublishIntoAReplacementReader() async throws {
        let fixture = PositionFixture(), gate = AtlasReaderRefreshFixture.Gate()
        fixture.base.change { $0.text = "first\nsecond\n" }
        fixture.change { $0.gate = gate }
        let model = AtlasPagedReaderModel(transport: fixture.transport)
        model.open(root: "/project", path: "old.rs", accessLease: AtlasReaderRefreshFixture.Lease(fixture.base), initialLine: 2)
        try await wait { gate.didEnter }
        require(model.page == nil && model.busy, "a pending position is not a published first-page success")
        model.cancel()
        require(!model.busy && model.page == nil && model.notice == "Source opening canceled.",
                "cancel synchronously revokes initial publication")
        require(fixture.base.snapshot.releasedLeases == 0, "blocked native call retains root access")
        fixture.base.change { $0.text = "replacement source\n" }
        model.open(root: "/other", path: "new.rs", accessLease: nil)
        gate.release()
        try await settle(model)
        require(model.page?.text == "replacement source\n" && model.page?.position == nil
                && model.page?.identity.pathBytes == Array("/other/new.rs".utf8),
                "late startup position cannot move or replace the newly opened source")
        try await wait { fixture.base.snapshot.closed[1] == 1 && fixture.base.snapshot.releasedLeases == 1 }
        model.close()
    }

    static func historyRestoresStartupLocationAndRefreshDoesNotReplayIt() async throws {
        let fixture = PositionFixture()
        fixture.base.change { $0.text = "first\nsecond\nthird\n" }
        let model = AtlasPagedReaderModel(transport: fixture.transport)
        model.open(root: "/project", path: "main.rs", accessLease: nil, initialLine: 3)
        try await settle(model)
        let identity = model.page?.identity
        model.offsetInput = "0"; model.goToByte(); try await settle(model)
        require(model.canGoBack && model.page?.position == nil, "ordinary navigation leaves the startup location")
        model.back(); try await settle(model)
        require(model.page?.position?.line == 3 && model.page?.identity == identity,
                "history returns to the exact retained startup position")
        fixture.base.change { $0.text = "short replacement\n" }
        model.refreshFromDisk(); try await settle(model)
        require(model.page?.text == "short replacement\n" && model.page?.position == nil
                && !model.canGoBack && model.refreshedFromDisk,
                "explicit recapture starts new history without replaying process arguments")
        model.close()
    }

    static func main() async throws {
        try await startupPositionUsesOneExactCaptureAndNativeCoordinates()
        try await missingOrUntrustedPositionKeepsTheCapturedFirstPage()
        try await unsupportedPositionAndZeroDoNotPretendSuccess()
        try await canceledPositionCannotPublishIntoAReplacementReader()
        try await historyRestoresStartupLocationAndRefreshDoesNotReplayIt()
        print("5 native startup reader regression cases passed")
    }
}

private final class PositionFixture: @unchecked Sendable {
    struct Call: Equatable, Sendable { let handle, line, column, kind: UInt64 }
    struct State {
        var calls: [Call] = []
        var gate: AtlasReaderRefreshFixture.Gate?
        var forgeIdentity = false
    }
    let base = AtlasReaderRefreshFixture()
    private let lock = NSLock()
    private var state = State()
    func change<T>(_ body: (inout State) -> T) -> T {
        lock.lock(); defer { lock.unlock() }
        return body(&state)
    }
    var snapshot: State { change { $0 } }
    var transport: AtlasReaderTransport {
        let baseTransport = base.transport
        return .init(create: baseTransport.create, open: baseTransport.open,
            window: baseTransport.window, lines: baseTransport.lines,
            position: { [self] handle, line, column, kind, bytes in
                position(handle, line: line, column: column, kind: kind, bytes: bytes)
            }, cancel: baseTransport.cancel, close: baseTransport.close,
            retirementFailed: baseTransport.retirementFailed)
    }
    private func position(_ handle: UInt64, line: UInt64, column: UInt64, kind: UInt64, bytes: UInt64) -> String? {
        let (gate, forge) = change { state in
            state.calls.append(.init(handle: handle, line: line, column: column, kind: kind))
            let gate = state.gate; state.gate = nil
            return (gate, state.forgeIdentity)
        }
        gate?.wait() // Deliberately deliver even after cancellation.
        guard let capture = base.snapshot.captures[handle], column == 1, kind == 1, bytes >= 4 else { return nil }
        let raw = Array(capture.text.utf8)
        var starts: [UInt64] = [0]
        for (offset, byte) in raw.enumerated() where byte == 10 && offset + 1 < raw.count {
            starts.append(UInt64(offset + 1))
        }
        guard line > 0, line <= UInt64(starts.count) else { return nil }
        let target = starts[Int(line - 1)]
        let start = target - min(target, (bytes - 4) / 2)
        guard let page = base.transport.window(handle, start, bytes), let data = page.data(using: .utf8),
              var wire = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { return nil }
        wire["command"] = "position"
        wire["selection"] = NSNull()
        wire["position"] = ["line": String(line), "column": String(column), "column_kind": "unicode_scalar",
            "original_offset": String(target), "window_utf8_offset": String(target - start)]
        if forge { wire["source_revision"] = "999" }
        guard let result = try? JSONSerialization.data(withJSONObject: wire, options: [.sortedKeys]) else { return nil }
        return String(decoding: result, as: UTF8.self)
    }
}
