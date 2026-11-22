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
        try await documentFind(input)
        try await documentLinks(input)
        try await documentImages(input)
        try await utf16Documents(input)
        print("\(checks) retained-document coordinator checks passed")
    }

    @MainActor static func utf16Documents(_ input: AtlasReaderInput) async throws {
        for (index, encoding) in ["utf16le", "utf16be"].enumerated() {
            let boundary = DocumentBoundary(owner: UInt64(70 + index), encoding: encoding)
            let coordinator = AtlasReaderCoordinator(transport: boundary.reader, document: boundary.document)
            let source = try await coordinator.open(input: input)
            check(source.identity.encoding == encoding && source.start == 2,
                  "UTF-16 source opens with a BOM-excluding decoded page")
            guard case .prepared(let page, let headings) = try await coordinator.document(.prepare(width: 100)) else { fatalError() }
            check(page.inventory.sourceBase == 2 && page.inventory.identity.matches(source.identity),
                  "Preview source coordinates keep the original captured encoding")
            let target = page.target(at: 1)!
            guard case .source(let selected) = try await coordinator.document(.source(target)) else { fatalError() }
            check(selected.enclosingOriginal == target.line.enclosingOriginal && selected.visible.start == 2,
                  "Preview-to-source handoff preserves the original UTF-16 region")
            guard case .copy(let copied) = try await coordinator.document(.copy(target, .enclosingMarkdown)) else { fatalError() }
            let codec: String.Encoding = encoding == "utf16le" ? .utf16LittleEndian : .utf16BigEndian
            check(copied.value == DocumentWireFixture.hex("A **bold** line.\n".data(using: codec)!),
                  "The C transport keeps original Markdown copy distinct from rendered text")
            _ = try await coordinator.document(.heading(headings.target(slug: "café")!))
            guard case .matches(let matches) = try await coordinator.document(.find(needle: "Café", first: 0)) else { fatalError() }
            check(matches.inventory.identity.encoding == encoding && matches.total == 130,
                  "Rendered Find uses the same UTF-16-backed retained document")
            guard case .links(let links) = try await coordinator.document(.prepareLinks) else { fatalError() }
            _ = try await coordinator.document(.link(links.target(id: 1)!))
            guard case .page(let returned) = try await coordinator.document(.fromSource(offset: selected.enclosingOriginal.start)) else { fatalError() }
            let old = returned.target(at: 1)!
            let gate = boundary.blockNext("document-source")
            let pending = Task { try await coordinator.document(.source(old)) }
            try await until { gate.hasEntered }
            coordinator.close()
            check(boundary.snapshot.closes == 0, "UTF-16 capture retired while source handoff was in flight")
            gate.release()
            await rejected { _ = try await pending.value }
            try await until { boundary.snapshot.closes == 1 }
            check(boundary.snapshot.opens == 1 && boundary.snapshot.peak == 1,
                  "UTF-16 preview work neither recaptures the file nor overlaps foreign calls")
        }
    }

    @MainActor static func documentImages(_ input: AtlasReaderInput) async throws {
        let capability = try AtlasDocumentImageSource(root: "/project", relativePath: "README.md")
        for (root, path) in [("relative", "README.md"), ("/project", "../README.md"),
                             ("/project", "docs//README.md"), ("/project", "/README.md")] {
            do {
                _ = try AtlasDocumentImageSource(root: root, relativePath: path)
                preconditionFailure("Invalid image source capability")
            } catch is AtlasReaderError { checks += 1 }
        }

        // A reader label that looks like a real file supplies no image grant.
        let inertWire = DocumentImageBoundary(owner: 21)
        let inert = AtlasReaderCoordinator(transport: inertWire.source.reader, document: inertWire.document)
        _ = try await inert.open(input: input)
        _ = try await inert.document(.prepare(width: 100))
        guard case .display(let inertPage, _) = try await inert.document(.prepareDisplay(width: 640)) else { fatalError() }
        check(inertWire.authorizations == 0 && inertPage.images.isEmpty,
              "Document label silently supplied a current-root capability")
        check(inertPage.imageNotice?.contains("folder access") == true, "Missing grant was not explained")
        inert.close()

        let wire = DocumentImageBoundary(owner: 22)
        let coordinator = AtlasReaderCoordinator(transport: wire.source.reader, document: wire.document)
        var lease: DocumentImageTestLease? = DocumentImageTestLease()
        weak var held = lease
        _ = try await coordinator.open(input: input, accessLease: lease, imageSource: capability)
        lease = nil
        _ = try await coordinator.document(.prepare(width: 100))
        guard case .display(let page, _) = try await coordinator.document(.prepareDisplay(width: 640)) else { fatalError() }
        check(page.images.isEmpty && page.items.count == 1, "Optional image refusal discarded the display")
#if canImport(ImageIO) && canImport(CoreGraphics) && canImport(CryptoKit)
        check(wire.authorizations == 1 && wire.receivedSource == capability,
              "Current root/path did not reach the image worker exactly")
        check(page.imageNotice?.contains("verified") == true, "Source verification refusal was hidden")

        // Task cancellation itself withdraws an image job even without an
        // explicit model cancel callback; the native grant stays until drain.
        let canceledGate = wire.blockNext()
        let canceled = Task { try await coordinator.document(.displayPage(first: 0)) }
        try await until { canceledGate.hasEntered }
        canceled.cancel()
        canceledGate.release()
        await rejected { _ = try await canceled.value }

        let closingGate = wire.blockNext()
        let closing = Task { try await coordinator.document(.displayPage(first: 0)) }
        try await until { closingGate.hasEntered }
        coordinator.close()
        check(held != nil && wire.source.snapshot.closes == 0,
              "Root access or reader retired while image authorization was active")
        closingGate.release()
        await rejected { _ = try await closing.value }
        try await until { held == nil && wire.source.snapshot.closes == 1 }
#else
        check(wire.authorizations == 0, "A host without a native decoder read image assets")
        coordinator.close()
        try await until { held == nil && wire.source.snapshot.closes == 1 }
#endif
    }

    @MainActor static func documentFind(_ input: AtlasReaderInput) async throws {
        let wire = DocumentBoundary(owner: 13)
        let coordinator = AtlasReaderCoordinator(transport: wire.reader, document: wire.document)
        _ = try await coordinator.open(input: input)
        guard case .prepared(let initial, _) = try await coordinator.document(.prepare(width: 100)) else { fatalError() }
        guard case .matches(let matches) = try await coordinator.document(.find(needle: "Café", first: 0)) else { fatalError() }
        check(matches.needle.utf8.elementsEqual("Café".utf8) && matches.rows.count == 128 && matches.total == 130,
              "Five-argument Find transport lost its exact query, bounded count, or inventory")
        _ = try await coordinator.document(.copy(initial.target(at: 0)!, .renderedText))
        let first = matches.target(id: 1)!
        guard case .page(let opened) = try await coordinator.document(.findMatch(first)) else { fatalError() }
        check(opened.first == first.match.firstLine && opened.inventory.matches(initial.inventory),
              "Find context used a different retained document")
        guard case .matches(let last) = try await coordinator.document(.find(needle: "Café", first: 128)) else { fatalError() }
        check(last.rows.count == 2 && last.next == nil, "Find continuation lost its complete short final page")
        let before = wire.snapshot.calls
        await rejected { _ = try await coordinator.document(.findMatch(first)) }
        await rejected { _ = try await coordinator.document(.find(needle: "", first: 0)) }
        await rejected { _ = try await coordinator.document(.find(needle: "Café", first: 131)) }
        check(wire.snapshot.calls == before, "Stale Find token or invalid query reached foreign work")
        let gate = wire.blockNext("document-find")
        let canceled = Task { try await coordinator.document(.find(needle: "old", first: 0)) }
        try await until { gate.hasEntered }
        coordinator.cancel()
        let replacement = Task { try await coordinator.document(.find(needle: "new", first: 0)) }
        await Task.yield()
        gate.release()
        await rejected { _ = try await canceled.value }
        guard case .matches(let replacementMatches) = try await replacement.value else { fatalError() }
        check(replacementMatches.needle == "new" && wire.snapshot.opens == 1 && wire.snapshot.peak == 1,
              "Canceled Find published or reopened/overlapped retained source")
        let target = replacementMatches.target(id: 1)!
        _ = try await coordinator.document(.prepare(width: 80))
        let afterReflow = wire.snapshot.calls
        await rejected { _ = try await coordinator.document(.findMatch(target)) }
        check(wire.snapshot.calls == afterReflow, "Reflow allowed a stale rendered-match activation")
        let closeGate = wire.blockNext("document-find")
        let closing = Task { try await coordinator.document(.find(needle: "closing", first: 0)) }
        try await until { closeGate.hasEntered }
        coordinator.close()
        check(wire.snapshot.closes == 0, "Find released the reader lease during its foreign scan")
        closeGate.release()
        await rejected { _ = try await closing.value }
        try await until { wire.snapshot.closes == 1 }

        let legacyWire = DocumentBoundary(owner: 14)
        var transport = legacyWire.document
        transport.find = nil
        let legacy = AtlasReaderCoordinator(transport: legacyWire.reader, document: transport)
        _ = try await legacy.open(input: input)
        guard case .prepared(let retained, _) = try await legacy.document(.prepare(width: 100)) else { fatalError() }
        let calls = legacyWire.snapshot.calls
        await rejected { _ = try await legacy.document(.find(needle: "line", first: 0)) }
        check(legacyWire.snapshot.calls == calls, "An absent optional Find transport performed foreign work")
        _ = try await legacy.document(.copy(retained.target(at: 0)!, .renderedText))
        legacy.close()
    }

    @MainActor static func documentLinks(_ input: AtlasReaderInput) async throws {
        let wire = DocumentBoundary(owner: 10)
        let coordinator = AtlasReaderCoordinator(transport: wire.reader, document: wire.document)
        _ = try await coordinator.open(input: input)
        _ = try await coordinator.document(.prepare(width: 100))
        guard case .links(let links) = try await coordinator.document(.prepareLinks) else { fatalError() }
        let heading = links.target(id: 1)!, local = links.target(id: 2)!
        guard case .link(let activated) = try await coordinator.document(.link(heading)) else { fatalError() }
        check(activated.page?.first == 64 && activated.target == heading, "link did not navigate retained flow")
        guard case .link(let inert) = try await coordinator.document(.link(local)) else { fatalError() }
        check(inert.page == nil && inert.target == local, "local reference acquired implicit file authority")
        guard case .links(let last) = try await coordinator.document(.links(first: 64)) else { fatalError() }
        check(last.rows.count == 6 && last.next == nil, "link final page dropped or claimed continuation")
        let calls = wire.snapshot.calls
        await rejected { _ = try await coordinator.document(.link(heading)) }
        check(wire.snapshot.calls == calls, "stale link list token reached the foreign worker")
        let latest = last.target(id: 65)!
        let cancelGate = wire.blockNext("document-link")
        let canceled = Task { try await coordinator.document(.link(latest)) }
        try await until { cancelGate.hasEntered }
        coordinator.cancel()
        cancelGate.release()
        await rejected { _ = try await canceled.value }
        _ = try await coordinator.document(.link(latest))
        let closeGate = wire.blockNext("document-links")
        let closing = Task { try await coordinator.document(.links(first: 0)) }
        try await until { closeGate.hasEntered }
        coordinator.close()
        check(wire.snapshot.closes == 0, "reader lease released while link worker was active")
        closeGate.release()
        await rejected { _ = try await closing.value }
        try await until { wire.snapshot.closes == 1 }
        check(wire.snapshot.opens == 1 && wire.snapshot.peak == 1, "link operations reopened or overlapped retained source")

        let legacyWire = DocumentBoundary(owner: 11)
        var legacyTransport = legacyWire.document
        legacyTransport.prepareLinks = nil; legacyTransport.links = nil; legacyTransport.link = nil
        let legacy = AtlasReaderCoordinator(transport: legacyWire.reader, document: legacyTransport)
        _ = try await legacy.open(input: input)
        _ = try await legacy.document(.prepare(width: 100))
        let before = legacyWire.snapshot.calls
        await rejected { _ = try await legacy.document(.prepareLinks) }
        check(legacyWire.snapshot.calls == before, "legacy transport faked a link operation")
        _ = try await legacy.document(.window(first: 0))
        legacy.close()
    }
}


/// This boundary deliberately refuses current-source verification. It tests
/// the production worker/grant/lifecycle path, not image decoding or Markdown.
private final class DocumentImageTestLease {}
private final class DocumentImageBoundary: @unchecked Sendable {
    let source: DocumentBoundary
    private let lock = NSLock()
    private var calls = 0
    private var received: AtlasDocumentImageSource?
    private var gate: DocumentCallGate?
    init(owner: UInt64) { source = DocumentBoundary(owner: owner) }
    var authorizations: Int { lock.withLock { calls } }
    var receivedSource: AtlasDocumentImageSource? { lock.withLock { received } }
    func blockNext() -> DocumentCallGate {
        let next = DocumentCallGate()
        lock.withLock { gate = next }
        return next
    }
    private func display(_ command: String, generation: UInt64, width: UInt64, first: UInt64) -> String {
        var result = DocumentWireFixture.header(command, owner: source.owner)
        let bounds: [String: Double] = ["x": 0, "y": 0, "width": 64, "height": 64]
        result["document_generation"] = String(generation)
        result["display_width_points"] = String(width)
        result["rendering"] = "bundled-frankenmarkdown-display"
        result["native_shaped"] = false; result["glyph_shaped"] = true
        result["source_mapping"] = "enclosing-ast-blocks-not-glyph-exact"
        result["source_ranges_qualified"] = true; result["total_display_items"] = "1"
        result["unresolved_images"] = "1"; result["display_bounds"] = bounds
        result["fonts"] = [[String: String]]()
        result["first_display_item"] = String(first)
        result["display_items"] = first == 0 ? [[
            "item_index": "0", "kind": "image", "bounds": bounds,
            "enclosing_original_range": DocumentWireFixture.range(DocumentWireFixture.start, DocumentWireFixture.end),
            "request_id": "17", "destination": "assets/red.png", "alt_text": "Red pixel", "is_resolved": false
        ]] as [[String: Any]] : [[String: Any]]()
        result["next_display_item"] = NSNull()
        return DocumentWireFixture.json(result)
    }
    var document: AtlasReaderDocumentTransport {
        var transport = source.document
        transport.prepareDisplay = { [self] _, generation, width in
            display("document-display-prepare", generation: generation, width: width, first: 0)
        }
        transport.displayPage = { [self] _, generation, first, _ in
            display("document-display-page", generation: generation, width: 640, first: first)
        }
        transport.displayFont = { _, _, _, _, _ in
            preconditionFailure("Image-only display requested font bytes")
        }
        transport.imagesAuthorize = { [self] _, _, root, relative in
            precondition(!Thread.isMainThread, "Image work reached the interaction thread")
            let source = try! AtlasDocumentImageSource(root: root, relativePath: relative)
            let blocked = lock.withLock {
                calls += 1; received = source
                let blocked = gate; gate = nil; return blocked
            }
            blocked?.wait()
            return DocumentWireFixture.json(["schema": "fcb.reader-session/1", "status": "error",
                "owner": String(self.source.owner), "exit_code": "2",
                "error": ["code": "DOCUMENT_IMAGE_ORIGIN_CHANGED"]])
        }
        transport.imagePrepare = { _, _, _, _ in
            preconditionFailure("Refused source verification allowed image acquisition")
        }
        transport.imageChunk = { _, _, _, _, _, _ in
            preconditionFailure("Refused source verification allowed image chunk delivery")
        }
        return transport
    }
}
