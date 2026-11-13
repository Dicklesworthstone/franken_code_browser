import Foundation
import Dispatch

/// Fixed C-boundary receipts; all admission, transaction, page/document
/// decoding, refresh, cancellation and retirement use production models.
private final class FileBoundary: @unchecked Sendable {
    private let lock = NSLock()
    private var next: UInt64 = 40
    private var sources: [UInt64: DocumentBoundary] = [:]
    private var paths: [String] = []
    private var closed = 0
    private var gate: DocumentCallGate?
    private var refuseWindow = false
    var openedPaths: [String] { lock.withLock { paths } }
    var closes: Int { lock.withLock { closed } }
    func blockOpen() -> DocumentCallGate {
        let gate = DocumentCallGate(); lock.withLock { self.gate = gate }; return gate
    }
    func failWindow() { lock.withLock { refuseWindow = true } }
    private func source(_ owner: UInt64) -> DocumentBoundary { lock.withLock { sources[owner]! } }
    private func create() -> UInt64 {
        lock.withLock {
            let owner = next; next += 1
            sources[owner] = DocumentBoundary(owner: owner)
            return owner
        }
    }
    private func open(_ owner: UInt64, path: String, limit: UInt64) -> String? {
        precondition(!Thread.isMainThread && limit == AtlasReaderLimits.captureBytes)
        let blocked = lock.withLock {
            paths.append(path); let blocked = gate; gate = nil; return blocked
        }
        blocked?.wait()
        return source(owner).reader.open(owner, path, limit)
    }
    var reader: AtlasReaderTransport {
        AtlasReaderTransport(create: { [self] in create() },
            open: { [self] in open($0, path: $1, limit: $2) },
            window: { [self] owner, offset, bytes in
                let refused = lock.withLock { let refused = refuseWindow; refuseWindow = false; return refused }
                return refused ? nil : source(owner).reader.window(owner, offset, bytes)
            },
            lines: { _, _, _, _ in nil }, cancel: { _ in true },
            close: { [self] owner in
                let retired = source(owner).reader.close(owner)
                lock.withLock { sources[owner] = nil; closed += 1 }
                return retired
            }, retirementFailed: { preconditionFailure("selected-file retirement failed") })
    }
    private func display(_ owner: UInt64, command: String, generation: UInt64, width: UInt64,
                         anchor: UInt64? = nil) -> String {
        var result = DocumentWireFixture.header(command, owner: owner)
        let bounds: [String: Double] = ["x": 0, "y": 0, "width": 64, "height": 64]
        result["document_generation"] = String(generation)
        result["display_width_points"] = String(width)
        result["rendering"] = "bundled-frankenmarkdown-display"
        result["native_shaped"] = false; result["glyph_shaped"] = true
        result["source_mapping"] = "enclosing-ast-blocks-not-glyph-exact"
        result["source_ranges_qualified"] = true; result["total_display_items"] = "1"
        result["unresolved_images"] = "1"; result["display_bounds"] = bounds
        result["fonts"] = [[String: String]](); result["first_display_item"] = "0"
        result["display_items"] = [[
            "item_index": "0", "kind": "image", "bounds": bounds,
            "enclosing_original_range": DocumentWireFixture.range(3, UInt64(DocumentWireFixture.raw.count)),
            "request_id": "17", "destination": "assets/red.png", "alt_text": "Red pixel", "is_resolved": false
        ]] as [[String: Any]]
        result["next_display_item"] = NSNull()
        if let anchor {
            result["original_anchor"] = String(anchor)
            if anchor == UInt64(DocumentWireFixture.raw.count) {
                result["first_display_item"] = "1"
                result["display_items"] = [[String: Any]]()
            }
        }
        return DocumentWireFixture.json(result)
    }
    var document: AtlasReaderDocumentTransport {
        var transport = AtlasReaderDocumentTransport(
            prepare: { [self] in source($0).document.prepare($0, $1, $2) },
            window: { [self] in source($0).document.window($0, $1, $2, $3) },
            headings: { [self] in source($0).document.headings($0, $1, $2, $3) },
            heading: { [self] in source($0).document.heading($0, $1, $2, $3) },
            fromSource: { [self] in source($0).document.fromSource($0, $1, $2, $3) },
            source: { [self] in source($0).document.source($0, $1, $2, $3, $4) },
            copy: { [self] in source($0).document.copy($0, $1, $2, $3, $4, $5) })
        transport.prepareDisplay = { [self] owner, generation, width in
            display(owner, command: "document-display-prepare", generation: generation, width: width)
        }
        transport.displayPage = { [self] owner, generation, _, _ in
            display(owner, command: "document-display-page", generation: generation, width: 640)
        }
        transport.displayFromSource = { [self] owner, generation, offset, _ in
            display(owner, command: "document-display-from-source", generation: generation, width: 640, anchor: offset)
        }
        transport.displayFont = { _, _, _, _, _ in preconditionFailure("image fixture requested font") }
        transport.imagesAuthorize = { _, _, _, _ in
            preconditionFailure("selected file silently authorized its parent directory")
        }
        transport.imagePrepare = { _, _, _, _ in preconditionFailure("selected file read a sibling image") }
        transport.imageChunk = { _, _, _, _, _, _ in preconditionFailure("selected file read image bytes") }
        return transport
    }
}
private final class FileGrant {}

@MainActor @main struct AtlasSelectedFileReaderTests {
    static func until(_ condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(5)
        while !condition(), Date() < deadline { try await Task.sleep(nanoseconds: 1_000_000) }
        precondition(condition(), "selected-file reader fixture did not drain")
    }
    static func main() async throws {
        try await fileOnlyRefresh()
        try await failedFirstPage()
        try await canceledOpenRetainsGrantUntilDrain()
        print("SelectedFileReader: exact capture, file-only Markdown/refresh, failed first page, cancellation and grant retirement passed")
    }
    static func input() throws -> AtlasReaderInput { try .init(root: "/project", path: "README.md") }

    static func fileOnlyRefresh() async throws {
        let wire = FileBoundary(), model = AtlasPagedReaderModel(transport: wire.reader, document: wire.document)
        var grant: FileGrant? = FileGrant(); weak var held = grant
        try await model.openSelectedFile(input: input(), accessLease: grant)
        grant = nil
        precondition(model.page?.originalHex == DocumentWireFixture.hex(DocumentWireFixture.raw))
        precondition(model.canRefresh && held != nil && model.documentPreview.richSupported)
        for pass in 0..<2 {
            model.documentPreview.widthInput = "80"
            model.documentPreview.prepare()
            try await until { !model.documentPreview.busy && model.documentPreview.displayPage != nil }
            precondition(model.documentPreview.displayPage?.images.isEmpty == true)
            precondition(model.documentPreview.displayPage?.imageNotice?.contains("folder access") == true)
            precondition(wire.openedPaths == Array(repeating: DocumentWireFixture.path, count: pass + 1))
            if pass == 0 {
                model.refreshFromDisk()
                try await until { !model.busy && model.refreshedFromDisk }
                precondition(held != nil && model.canRefresh)
            }
        }
        // The file-only initializer cannot replace an accepted model in place.
        let original = model.page!.identity
        do {
            try await model.openSelectedFile(input: input(), accessLease: nil)
            preconditionFailure("second initializer replaced accepted source")
        } catch is AtlasReaderError {}
        precondition(model.page?.identity.matches(original) == true && wire.openedPaths.count == 2)
        model.close()
        try await until { held == nil && wire.closes == 2 }
    }
    static func failedFirstPage() async throws {
        let wire = FileBoundary(), model = AtlasPagedReaderModel(transport: wire.reader)
        wire.failWindow()
        var grant: FileGrant? = FileGrant(); weak var held = grant
        do {
            try await model.openSelectedFile(input: input(), accessLease: grant)
            preconditionFailure("invalid first page published")
        } catch is AtlasReaderError {}
        grant = nil
        precondition(model.page == nil && !model.canRefresh)
        try await until { held == nil && wire.closes == 1 }
    }
    static func canceledOpenRetainsGrantUntilDrain() async throws {
        for closing in [false, true] {
            let wire = FileBoundary(), model = AtlasPagedReaderModel(transport: wire.reader)
            let gate = wire.blockOpen()
            var grant: FileGrant? = FileGrant(); weak var held = grant
            var operation: Task<Void, Error>? = Task { [grant] in
                try await model.openSelectedFile(input: input(), accessLease: grant)
            }
            try await until { gate.hasEntered }
            grant = nil
            if closing { model.close() } else { operation?.cancel() }
            precondition(held != nil && wire.closes == 0)
            gate.release()
            do { try await operation!.value; preconditionFailure("canceled capture published") }
            catch {}
            operation = nil
            precondition(model.page == nil && !model.canRefresh)
            try await until { wire.closes == 1 && held == nil }
        }
    }
}
