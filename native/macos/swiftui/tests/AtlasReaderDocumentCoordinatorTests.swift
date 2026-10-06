// Compile with AtlasReader{,Search,Outline,Document,Coordinator}.swift,
// AtlasProjectIO.swift, the production AtlasSearchCancellation declaration,
// and tests/AtlasReaderDocumentFixture.swift. No Rust or Apple UI is linked.
import Foundation

@main struct AtlasReaderDocumentCoordinatorTests {
    @MainActor static var checks = 0
    @MainActor static func check(_ condition: @autoclosure () -> Bool, _ message: String) {
        checks += 1; precondition(condition(), message)
    }
    @MainActor static func rejected(_ body: () async throws -> Void) async {
        do { try await body(); preconditionFailure("stale or invalid request accepted") }
        catch is AtlasReaderError { checks += 1 }
        catch { preconditionFailure("unexpected error \(error)") }
    }
    @MainActor static func until(_ condition: () -> Bool) async throws {
        for _ in 0..<2000 {
            if condition() { return }
            try await Task.sleep(for: .milliseconds(2))
        }
        preconditionFailure("timed out awaiting production queue")
    }
    @MainActor static func main() async throws {
        let wire = DocumentBoundary()
        let coordinator = AtlasReaderCoordinator(transport: wire.reader, document: wire.document)
        let input = try AtlasReaderInput(root: "/project", path: "README.md")
        let source = try await coordinator.open(input: input)
        check(coordinator.supportsDocument && source.identity.owner == 7, "document capability or source owner lost")
        guard case .prepared(let page, let headings) = try await coordinator.document(.prepare(width: 100)) else { fatalError() }
        check(page.lines.count == 64 && page.next == 64 && headings.next == 64, "bounded first page")
        let target = page.target(at: 1)!
        for mode in [AtlasReaderDocumentCopyMode.renderedText, .enclosingMarkdown] {
            guard case .copy(let copied) = try await coordinator.document(.copy(target, mode)) else { fatalError() }
            check(copied.mode == mode && copied.target == target, "copy domains lost")
        }
        guard case .source(let revealed) = try await coordinator.document(.source(target)) else { fatalError() }
        check(revealed.selectionUTF8.start == DocumentWireFixture.start - 3, "BOM source mapping lost")
        check(wire.snapshot.opens == 1, "preview reopened source")
        let calls = wire.snapshot.calls
        await rejected { _ = try await coordinator.document(.prepare(width: 513)) }
        check(wire.snapshot.calls == calls, "invalid input reached foreign worker")
        _ = try await coordinator.document(.copy(target, .renderedText))
        guard case .page(let last) = try await coordinator.document(.window(first: 64)) else { fatalError() }
        check(last.lines.count == 6 && last.next == nil, "last page completeness")
        await rejected { _ = try await coordinator.document(.source(target)) }
        guard case .headings(let lastHeadings) = try await coordinator.document(.headings(first: 64)) else { fatalError() }
        check(lastHeadings.rows.count == 1 && lastHeadings.next == nil, "heading pagination")
        await rejected { _ = try await coordinator.document(.heading(headings.target(slug: "café")!)) }
        _ = try await coordinator.document(.heading(lastHeadings.target(slug: "heading-64")!))
        guard case .page(let anchored) = try await coordinator.document(.fromSource(offset: DocumentWireFixture.start)) else { fatalError() }
        check(anchored.inventory.identity.matches(source.identity), "preview/source identities differ")
        let beforeReflow = anchored.target(at: 1)!
        let reflowGate = wire.blockNext("document-prepare")
        let reflow = Task { try await coordinator.document(.prepare(width: 80)) }
        try await until { reflowGate.hasEntered }
        await rejected { _ = try await coordinator.document(.copy(beforeReflow, .renderedText)) }
        coordinator.cancel()
        reflowGate.release()
        await rejected { _ = try await reflow.value }
        await rejected { _ = try await coordinator.document(.window(first: 0)) }
        guard case .prepared(let recovered, _) = try await coordinator.document(.prepare(width: 90)) else { fatalError() }
        check(recovered.inventory.generation == 3, "canceled reflow reused generation")
        let recoveredTarget = recovered.target(at: 1)!
        wire.corruptNext()
        await rejected { _ = try await coordinator.document(.copy(recoveredTarget, .renderedText)) }
        _ = try await coordinator.read(.firstPage)
        _ = try await coordinator.document(.source(recoveredTarget))
        let copyGate = wire.blockNext("document-copy")
        let oldCopy = Task { try await coordinator.document(.copy(recoveredTarget, .renderedText)) }
        try await until { copyGate.hasEntered }
        let replacement = Task { try await coordinator.document(.window(first: 64)) }
        await Task.yield()
        copyGate.release()
        await rejected { _ = try await oldCopy.value }
        guard case .page(let replacementPage) = try await replacement.value else { fatalError() }
        check(replacementPage.first == 64 && wire.snapshot.peak == 1, "foreign calls overlapped")
        let closeGate = wire.blockNext("document-source")
        let closing = Task { try await coordinator.document(.source(replacementPage.target(at: 64)!)) }
        try await until { closeGate.hasEntered }
        coordinator.close()
        check(wire.snapshot.closes == 0, "capture retired during foreign source call")
        closeGate.release()
        await rejected { _ = try await closing.value }
        try await until { wire.snapshot.closes == 1 }
        check(wire.snapshot.opens == 1, "source was recaptured by document work")
        let otherWire = DocumentBoundary(owner: 8)
        let other = AtlasReaderCoordinator(transport: otherWire.reader, document: otherWire.document)
        _ = try await other.open(input: input)
        _ = try await other.document(.prepare(width: 100))
        await rejected { _ = try await other.document(.source(target)) }
        other.close()
        let plainWire = DocumentBoundary(owner: 9)
        let plain = AtlasReaderCoordinator(transport: plainWire.reader)
        _ = try await plain.open(input: input)
        check(!plain.supportsDocument, "source-only host acquired document capability")
        await rejected { _ = try await plain.document(.prepare(width: 100)) }
        plain.close()
        print("\(checks) retained-document coordinator checks passed")
    }
}
