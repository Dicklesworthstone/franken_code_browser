import Foundation

// Foundation-only model regressions. Compile with AtlasNavigationHistory and
// the production AtlasReader, AtlasReaderSearch, AtlasReaderOutline,
// AtlasReaderDocument and AtlasDocumentPreviewModel in the qualification lane.
// The injected operation stands in for the worker, not for the model under test.
@main @MainActor struct AtlasDocumentNavigationRegressionTests {
    static func require(_ condition: @autoclosure () -> Bool, _ message: String) {
        if !condition() { fatalError(message) }
    }
    static func identity(owner: UInt64 = 1, revision: UInt64 = 2,
                         path: String = "/project/notes.md") -> AtlasReaderIdentity {
        .init(owner: owner, file: 3, revision: revision, capturedBytes: 128,
              pathBytes: Array(path.utf8), encoding: "utf8", displayPath: "display only")
    }
    static func drain() async { for _ in 0..<64 { await Task.yield() } }
    static func settle(_ model: AtlasDocumentPreviewModel) async {
        for _ in 0..<4096 {
            if !model.busy { return }
            await Task.yield()
        }
        fatalError("model failed to finish a fixture operation")
    }

    @MainActor final class Fixture {
        let capture = AtlasDocumentNavigationRegressionTests.identity()
        var model: AtlasDocumentPreviewModel!
        var generation: UInt64 = 0
        var holdsSource = false
        var malformedSource = false
        var pending: (AtlasReaderDocumentTarget, CheckedContinuation<AtlasReaderDocumentResult, Error>)?
        var sourceRequests = 0
        var sourceAnchor: UInt64?
        var cancels = 0

        init() {
            model = AtlasDocumentPreviewModel(supported: true, execute: { [weak self] request in
                guard let self else { throw AtlasReaderError.unavailable }
                return try await self.execute(request)
            }, cancel: { [weak self] in self?.cancels += 1 }, revoke: {})
            model.bind(identity: capture, sourceOffset: 0)
        }
        func page(first: UInt64 = 0, width: UInt64 = 96) -> AtlasReaderDocumentPage {
            let inventory = AtlasReaderDocumentInventory(identity: capture, generation: generation,
                width: width, totalLines: 128, totalHeadings: 0, renderedBytes: 512, sourceBase: 3)
            // The three domains intentionally differ: rendered start 100,
            // source start 64 (BOM included), context-local selection start 1.
            let line = AtlasReaderDocumentLine(id: first, text: "hello",
                rendered: .init(start: 100, end: 105), enclosingOriginal: .init(start: 64, end: 74))
            return .init(id: UUID(), inventory: inventory, first: first, lines: [line],
                next: first == 0 ? 64 : nil, wholeDocumentVisible: false)
        }
        func source(_ target: AtlasReaderDocumentTarget) -> AtlasReaderDocumentSource {
            .init(target: target, visible: .init(start: 63, end: 75), text: "xabcdefghijy",
                originalHex: "786162636465666768696a79",
                enclosingOriginal: .init(start: malformedSource ? 65 : 64, end: 74),
                selectionUTF8: .init(start: 1, end: 11), selectedHex: "6162636465666768696a")
        }
        func execute(_ request: AtlasReaderDocumentRequest) async throws -> AtlasReaderDocumentResult {
            switch request {
            case .prepare(let width):
                generation += 1
                let page = self.page(width: width)
                return .prepared(page, .init(id: UUID(), inventory: page.inventory, first: 0, rows: [], next: nil))
            case .source(let target):
                sourceRequests += 1
                if holdsSource {
                    return try await withCheckedThrowingContinuation { continuation in
                        AtlasDocumentNavigationRegressionTests.require(pending == nil, "fixture admits only one held response")
                        pending = (target, continuation)
                    }
                }
                return .source(source(target))
            case .window(let first): return .page(page(first: first))
            case .fromSource(let offset):
                sourceAnchor = offset
                return .page(page(first: 64))
            default: throw AtlasReaderError.invalidInput
            }
        }
        func ready() async -> AtlasReaderDocumentTarget {
            model.prepare()
            await AtlasDocumentNavigationRegressionTests.settle(model)
            guard let target = model.page?.target(at: 0) else { fatalError("prepare must publish a mapped target") }
            return target
        }
        func waitForPending() async {
            for _ in 0..<4096 {
                if pending != nil { return }
                await Task.yield()
            }
            fatalError("fixture did not receive source request")
        }
        func release() {
            guard let (target, continuation) = pending else { fatalError("no held source operation") }
            pending = nil
            continuation.resume(returning: .source(source(target)))
        }
    }

    static func originalByteAnchorAndReentrantDelivery() async {
        let fixture = Fixture()
        let target = await fixture.ready()
        var delivered = 0
        fixture.model.showSource(target) { received in
            require(received == target, "deliver the exact page-qualified row")
            require(!fixture.model.busy && fixture.model.canNavigate, "commit and finish before host callback")
            require(fixture.model.sourceOffset(for: received, reader: fixture.capture) == 64,
                    "never use flow offsets, UTF-16, local selection offsets or subtract the BOM")
            delivered += 1
            // Source navigation starts by canceling preview work. A delivered
            // response must no longer look in-flight to the shared coordinator.
            let cancels = fixture.cancels
            fixture.model.cancel(silent: true)
            require(fixture.cancels == cancels, "finished source mapping must not cancel newly started reader work")
            fixture.model.bind(identity: fixture.capture, sourceOffset: 64)
        }
        await settle(fixture.model)
        require(delivered == 1, "exactly one explicit activation")
        require(fixture.model.page?.id == target.pageID, "source paging preserves the preview's independent position")
        fixture.model.fromSource()
        await settle(fixture.model)
        require(fixture.sourceAnchor == 64, "source-to-preview uses the latest committed original-byte anchor")
    }

    static func contextOnlyDoesNotActivate() async {
        let fixture = Fixture()
        let target = await fixture.ready()
        require(fixture.model.sourceOffset(for: target, reader: fixture.capture) == nil,
                "a flow row alone is not a verified source mapping")
        fixture.model.showSource(target)
        await settle(fixture.model)
        require(fixture.sourceRequests == 1, "context uses the shared document operation")
        require(fixture.model.sourceOffset(for: target, reader: fixture.capture) == 64, "explicit context becomes navigable")
    }

    static func mismatchedReaderCannotUseTheOffset() async {
        let fixture = Fixture()
        let target = await fixture.ready()
        fixture.model.showSource(target)
        await settle(fixture.model)
        for other in [identity(owner: 9), identity(revision: 9), identity(path: "/other/notes.md")] {
            require(fixture.model.sourceOffset(for: target, reader: other) == nil,
                    "owner, revision and raw path identity must match the reader")
        }
        let other = AtlasReaderIdentity(owner: 1, file: 3, revision: 2, capturedBytes: 129,
            pathBytes: fixture.capture.pathBytes, encoding: "utf8", displayPath: "display only")
        require(fixture.model.sourceOffset(for: target, reader: other) == nil, "capture size also belongs to identity")
    }

    static func canceledMappingCannotNavigateLater() async {
        let fixture = Fixture()
        let target = await fixture.ready()
        fixture.holdsSource = true
        var delivered = 0
        fixture.model.showSource(target) { _ in delivered += 1 }
        await fixture.waitForPending()
        fixture.model.cancel()
        fixture.release()
        await drain()
        require(delivered == 0 && fixture.model.source == nil, "cancel revokes delayed activation")
        require(fixture.model.page?.id == target.pageID, "cancel retains accepted preview")
    }

    static func reflowInvalidatesPendingAndAcceptedMappings() async {
        let fixture = Fixture()
        let target = await fixture.ready()
        fixture.model.showSource(target)
        await settle(fixture.model)
        require(fixture.model.sourceOffset(for: target, reader: fixture.capture) == 64, "positive control")
        fixture.holdsSource = true
        var delivered = 0
        fixture.model.showSource(target) { _ in delivered += 1 }
        await fixture.waitForPending()
        fixture.model.prepare()
        require(fixture.model.sourceOffset(for: target, reader: fixture.capture) == nil, "reflow revokes synchronously")
        await settle(fixture.model)
        let replacement = fixture.model.page?.id
        fixture.release()
        await drain()
        require(delivered == 0 && fixture.model.source == nil, "old generation must not activate")
        require(replacement != target.pageID && fixture.model.page?.id == replacement, "new flow survives old response")
    }

    static func readerReplacementInvalidatesPendingMapping() async {
        let fixture = Fixture()
        let target = await fixture.ready()
        fixture.holdsSource = true
        var delivered = 0
        fixture.model.showSource(target) { _ in delivered += 1 }
        await fixture.waitForPending()
        fixture.model.bind(identity: identity(revision: 10), sourceOffset: 0)
        fixture.release()
        await drain()
        require(delivered == 0 && fixture.model.page == nil && fixture.model.source == nil,
                "old reader cannot publish into replacement capture")
    }

    static func wrongEnclosingRegionIsRejected() async {
        let fixture = Fixture()
        let target = await fixture.ready()
        fixture.malformedSource = true
        var delivered = 0
        fixture.model.showSource(target) { _ in delivered += 1 }
        await settle(fixture.model)
        require(delivered == 0 && fixture.model.source == nil, "source response must agree with upstream row provenance")
        require(!fixture.model.notice.isEmpty, "mapping failure is visible")
    }

    static func previewPageReplacementInvalidatesContextTarget() async {
        let fixture = Fixture()
        let target = await fixture.ready()
        fixture.model.showSource(target)
        await settle(fixture.model)
        fixture.model.next()
        await settle(fixture.model)
        require(fixture.model.sourceOffset(for: target, reader: fixture.capture) == nil, "page-local IDs never survive paging")
        let count = fixture.sourceRequests
        fixture.model.showSource(target) { _ in fatalError("stale row activated") }
        require(fixture.sourceRequests == count, "stale row does not enqueue source work")
    }

    static func callbackMayCloseTheReader() async {
        let fixture = Fixture()
        let target = await fixture.ready()
        var delivered = 0
        fixture.model.showSource(target) { _ in delivered += 1; fixture.model.close() }
        await settle(fixture.model)
        require(delivered == 1 && fixture.model.page == nil && fixture.model.source == nil,
                "completion must not reinstall state after reentrant close")
        require(!fixture.model.busy && fixture.model.notice.isEmpty, "closed state stays closed")
    }

    static func main() async {
        await originalByteAnchorAndReentrantDelivery()
        await contextOnlyDoesNotActivate()
        await mismatchedReaderCannotUseTheOffset()
        await canceledMappingCannotNavigateLater()
        await reflowInvalidatesPendingAndAcceptedMappings()
        await readerReplacementInvalidatesPendingMapping()
        await wrongEnclosingRegionIsRejected()
        await previewPageReplacementInvalidatesContextTarget()
        await callbackMayCloseTheReader()
        print("9 document navigation regression cases passed")
    }
}
