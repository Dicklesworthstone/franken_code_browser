import Foundation

// Production-model integration tests. Compile with AtlasNavigationHistory and
// the same production reader/document sources as the document-navigation suite.
@main @MainActor struct AtlasDocumentHistoryRegressionTests {
    static func require(_ condition: @autoclosure () -> Bool, _ message: String) {
        if !condition() { fatalError(message) }
    }
    static func settle(_ model: AtlasDocumentPreviewModel) async {
        for _ in 0..<4096 {
            if !model.busy { return }
            await Task.yield()
        }
        fatalError("fixture operation did not finish")
    }
    @MainActor final class Fixture {
        let identity = AtlasReaderIdentity(owner: 1, file: 2, revision: 3, capturedBytes: 1024,
            pathBytes: Array("/p/README.md".utf8), encoding: "utf8", displayPath: "README.md")
        var model: AtlasDocumentPreviewModel!
        var generation: UInt64 = 0
        var failWindow = false
        var holdWindow = false
        var releasedWindow = false
        var sourceAnchor: UInt64?
        var pending: (UInt64, CheckedContinuation<AtlasReaderDocumentResult, Error>)?
        init() {
            model = AtlasDocumentPreviewModel(supported: true, execute: { [weak self] request in
                guard let self else { throw AtlasReaderError.unavailable }
                return try await self.execute(request)
            }, cancel: {}, revoke: {})
            model.bind(identity: identity, sourceOffset: 0)
        }
        func page(_ first: UInt64) -> AtlasReaderDocumentPage {
            let inventory = AtlasReaderDocumentInventory(identity: identity, generation: generation, width: 96,
                totalLines: 256, totalHeadings: 0, renderedBytes: 1024, sourceBase: 3)
            let row = AtlasReaderDocumentLine(id: first, text: "row",
                rendered: .init(start: first * 4, end: first * 4 + 3),
                enclosingOriginal: .init(start: first + 3, end: first + 6))
            return .init(id: UUID(), inventory: inventory, first: first, lines: [row],
                next: first + 64 < 256 ? first + 64 : nil, wholeDocumentVisible: false)
        }
        func execute(_ request: AtlasReaderDocumentRequest) async throws -> AtlasReaderDocumentResult {
            switch request {
            case .prepare:
                generation += 1
                let page = self.page(0)
                return .prepared(page, .init(id: UUID(), inventory: page.inventory, first: 0, rows: [], next: nil))
            case .window(let first):
                if failWindow { throw AtlasReaderError.unavailable }
                if holdWindow {
                    let result: AtlasReaderDocumentResult = try await withCheckedThrowingContinuation { pending = (first, $0) }
                    releasedWindow = true
                    return result
                }
                return .page(page(first))
            case .fromSource(let offset):
                sourceAnchor = offset
                return .page(page(64))
            default: throw AtlasReaderError.invalidInput
            }
        }
        func ready() async {
            model.prepare()
            await AtlasDocumentHistoryRegressionTests.settle(model)
            AtlasDocumentHistoryRegressionTests.require(model.canNavigate, "prepared model")
        }
        func go(_ row: UInt64) async {
            model.lineInput = String(row + 1)
            model.goToLine()
            await AtlasDocumentHistoryRegressionTests.settle(model)
        }
    }
    static func roundTripUsesTheRealModel() async {
        let f = Fixture(); await f.ready()
        await f.go(64); await f.go(128)
        f.model.back(); await settle(f.model)
        require(f.model.page?.first == 64 && f.model.canBack && f.model.canForward, "back commits both directions")
        f.model.forward(); await settle(f.model)
        require(f.model.page?.first == 128 && !f.model.canForward, "forward restores visited page")
        f.model.next(); await settle(f.model)
        require(f.model.page?.first == 192, "Next page is not Forward history")
    }
    static func failedBranchPreservesFuture() async {
        let f = Fixture(); await f.ready()
        await f.go(64); await f.go(128)
        f.model.back(); await settle(f.model)
        f.failWindow = true
        await f.go(9)
        require(f.model.page?.first == 64 && f.model.canForward && !f.model.notice.isEmpty,
                "failed new destination must not discard forward history")
        f.failWindow = false
        await f.go(9)
        require(f.model.page?.first == 9 && !f.model.canForward, "successful branch discards superseded future")
        f.model.back(); await settle(f.model)
        require(f.model.page?.first == 64, "branch predecessor remains")
    }
    static func canceledForwardDoesNotMoveTheCursor() async {
        let f = Fixture(); await f.ready()
        await f.go(64)
        f.model.back(); await settle(f.model)
        f.holdWindow = true
        f.model.forward()
        for _ in 0..<4096 {
            if f.pending != nil { break }
            await Task.yield()
        }
        guard let (first, continuation) = f.pending else { fatalError("missing pending forward") }
        f.pending = nil
        f.model.cancel()
        continuation.resume(returning: .page(f.page(first)))
        for _ in 0..<4096 {
            if f.releasedWindow { break }
            await Task.yield()
        }
        require(f.releasedWindow, "delayed worker response was actually delivered")
        for _ in 0..<64 { await Task.yield() }
        require(f.model.page?.first == 0 && f.model.canForward, "late canceled page cannot consume history")
        f.holdWindow = false
        f.model.forward(); await settle(f.model)
        require(f.model.page?.first == 64, "forward remains retryable")
    }
    static func reflowAndCloseRetireHistory() async {
        let f = Fixture(); await f.ready()
        await f.go(64)
        f.model.back(); await settle(f.model)
        require(f.model.canForward, "positive control")
        f.model.prepare(); await settle(f.model)
        require(f.model.page?.inventory.generation == 2 && !f.model.canBack && !f.model.canForward,
                "flow rows are generation-local")
        await f.go(64)
        f.model.close()
        require(f.model.page == nil && !f.model.canBack && !f.model.canForward, "closed reader has no travel targets")
    }
    static func sourceAndFlowCoordinatesRemainDistinct() async {
        let f = Fixture(); await f.ready()
        await f.go(128)
        f.model.bind(identity: f.identity, sourceOffset: 77)
        require(f.model.page?.first == 128 && f.model.canBack, "source paging does not move the preview")
        f.model.fromSource(); await settle(f.model)
        require(f.sourceAnchor == 77 && f.model.page?.first == 64, "engine resolves source bytes to flow rows")
        f.model.back(); await settle(f.model)
        f.model.forward(); await settle(f.model)
        require(f.model.page?.first == 64, "history stores resolved flow row, never source byte 77")
    }
    static func main() async {
        await roundTripUsesTheRealModel()
        await failedBranchPreservesFuture()
        await canceledForwardDoesNotMoveTheCursor()
        await reflowAndCloseRetireHistory()
        await sourceAndFlowCoordinatesRemainDistinct()
        print("5 document history regression cases passed")
    }
}
