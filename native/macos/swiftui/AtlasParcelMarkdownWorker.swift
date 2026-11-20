import Foundation

/// Complete upstream display output for one immutable atlas source. This value
/// carries no live file grant or reader handle. The existing document job owns
/// wire validation, font loading and all parsing/shaping/layout semantics.
struct AtlasParcelMarkdownPacket: Sendable {
    let pages: [AtlasDocumentDisplayPage]
    let fonts: [AtlasDocumentDisplayFont]
    let captureSHA256: String
}

struct AtlasParcelMarkdownTransport: Sendable {
    struct Supplied: Sendable {
        let json: String?
        let sha256: String
    }
    let create: @Sendable () -> UInt64
    let supply: @Sendable (UInt64, String, Data) -> Supplied?
    let close: @Sendable (UInt64) -> Bool
    let document: AtlasReaderDocumentTransport
    /// Native transport owns this pool for its full lifetime, including failed
    /// close records. Injected transports get an isolated bounded pool.
    var retirement = AtlasParcelMarkdownRetirement()
}

enum AtlasParcelMarkdownWorker {
    static let sourceBytes = 64 * 1024
    static let displayWidth: UInt64 = 640
    static let flowWidth: UInt64 = 80
    static let maximumGlyphs = 262_144
    static let maximumPages = 128

    /// Synchronous work on the already bounded viewport worker. Supplying the
    /// accepted bytes avoids reopening a changed working-tree path. No source
    /// or asset filesystem access is authorized by this operation.
    static func load(path: String, source: String, transport: AtlasParcelMarkdownTransport,
                     canceled: () -> Bool = { false }) throws -> AtlasParcelMarkdownPacket {
        guard !path.isEmpty, path.utf8.count <= 16_384, !path.utf8.contains(0),
              source.utf8.count <= sourceBytes else { throw AtlasReaderError.invalidInput }
        try check(canceled)
        guard let lease = transport.retirement.acquire(create: transport.create, close: transport.close) else {
            throw AtlasReaderError.unavailable
        }
        defer { _ = lease.finish() }
        let result = Result {
            try prepare(path: path, source: source, handle: lease.handle, transport: transport, canceled: canceled)
        }
        // The reader must drain before an immutable packet may leave this
        // worker. A busy close remains owned and charged in the transport's
        // quarantine for a later explicit drain, including on failure/cancel.
        guard lease.finish() else { throw AtlasReaderError.unavailable }
        try check(canceled)
        return try result.get()
    }

    private static func prepare(path: String, source: String, handle: UInt64,
        transport: AtlasParcelMarkdownTransport, canceled: () -> Bool) throws -> AtlasParcelMarkdownPacket {
        try check(canceled)
        guard let supplied = transport.supply(handle, path, Data(source.utf8)) else {
            throw AtlasReaderError.unavailable
        }
        let info = try AtlasReaderInfo.decodeRetainedInitialization(supplied.json,
            handle: handle, pathBytes: Array(path.utf8))
        guard info.origin == "host-supplied", info.identity.encoding == "utf8",
              info.identity.capturedBytes == UInt64(source.utf8.count),
              info.captureSHA256 == supplied.sha256, supplied.sha256.utf8.count == 64,
              supplied.sha256.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else {
            throw AtlasReaderError.invalidResponse
        }
        try check(canceled)
        let flowJob = AtlasReaderDocumentJob(id: UUID(), info: info, generation: 1,
            inventory: nil, request: .prepare(width: flowWidth))
        guard case .prepared(let flow, _) = try flowJob.execute(transport.document, canceled: canceled) else {
            throw AtlasReaderError.invalidResponse
        }
        let displayJob = AtlasReaderDocumentJob(id: UUID(), info: info, generation: 1,
            inventory: flow.inventory, request: .prepareDisplay(width: displayWidth))
        guard case .display(let first, let suppliedFonts) = try displayJob.execute(transport.document, canceled: canceled),
              let fonts = suppliedFonts, first.first == 0,
              first.inventory.qualifiedSources,
              first.inventory.document.matches(flow.inventory) else { throw AtlasReaderError.invalidResponse }
        var pages: [AtlasDocumentDisplayPage] = [], items: UInt64 = 0, glyphs = 0
        var page = first
        while true {
            try check(canceled)
            guard pages.count < maximumPages, page.first == items,
                  page.inventory.matches(first.inventory),
                  UInt64(page.items.count) <= first.inventory.totalItems - items else {
                throw AtlasReaderError.invalidResponse
            }
            try page.validateFonts(fonts)
            for item in page.items {
                if case .text(_, _, _, let run) = item.content {
                    guard run.count <= maximumGlyphs - glyphs else { throw AtlasReaderError.invalidResponse }
                    glyphs += run.count
                }
            }
            items += UInt64(page.items.count)
            pages.append(page)
            guard let next = page.next else {
                guard items == first.inventory.totalItems else { throw AtlasReaderError.invalidResponse }
                break
            }
            guard next == items, next < first.inventory.totalItems, !page.items.isEmpty else {
                throw AtlasReaderError.invalidResponse
            }
            let job = AtlasReaderDocumentJob(id: UUID(), info: info, generation: 1,
                inventory: flow.inventory, request: .displayPage(first: next), displayInventory: first.inventory)
            guard case .display(let loaded, let extraFonts) = try job.execute(transport.document, canceled: canceled),
                  extraFonts == nil else { throw AtlasReaderError.invalidResponse }
            page = loaded
        }
        try check(canceled)
        return .init(pages: pages, fonts: fonts, captureSHA256: supplied.sha256)
    }

    private static func check(_ canceled: () -> Bool) throws {
        if canceled() { throw AtlasReaderError.canceled }
    }
}
