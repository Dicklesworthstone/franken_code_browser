import Foundation

/// The native host consumes FrankenMarkdown's logical flow. It never parses
/// Markdown or invents heading slugs. Optional local images require an explicit
/// current-root capability or a still-open saved-archive provider. Document path
/// labels never confer access.
struct AtlasReaderDocumentTransport: Sendable {
    let prepare: @Sendable (UInt64, UInt64, UInt64) -> String?
    let window: @Sendable (UInt64, UInt64, UInt64, UInt64) -> String?
    let headings: @Sendable (UInt64, UInt64, UInt64, UInt64) -> String?
    let heading: @Sendable (UInt64, UInt64, String, UInt64) -> String?
    let fromSource: @Sendable (UInt64, UInt64, UInt64, UInt64) -> String?
    let source: @Sendable (UInt64, UInt64, UInt64, UInt64, UInt64) -> String?
    let copy: @Sendable (UInt64, UInt64, UInt64, UInt64, UInt8) -> String?
    // Optional for embedders and existing source-only fixture transports.
    var prepareLinks: (@Sendable (UInt64, UInt64, UInt64) -> String?)? = nil
    var links: (@Sendable (UInt64, UInt64, UInt64, UInt64) -> String?)? = nil
    var link: (@Sendable (UInt64, UInt64, UInt64, UInt64) -> String?)? = nil
    var prepareDisplay: (@Sendable (UInt64, UInt64, UInt64) -> String?)? = nil
    var displayPage: (@Sendable (UInt64, UInt64, UInt64, UInt64) -> String?)? = nil
    var displayFont: (@Sendable (UInt64, UInt64, UInt64, UInt64, UInt64) -> String?)? = nil
    var displayFromSource: (@Sendable (UInt64, UInt64, UInt64, UInt64) -> String?)? = nil
    var imagesAuthorize: (@Sendable (UInt64, UInt64, String, String) -> String?)? = nil
    var imagePrepare: (@Sendable (UInt64, UInt64, UInt64, UInt64) -> String?)? = nil
    var imageChunk: (@Sendable (UInt64, UInt64, UInt64, UInt64, UInt64, UInt64) -> String?)? = nil
    // Frozen for this reader at the verified saved-member handoff. A separate
    // revocation gate prevents acquisition after archive close/replacement.
    var savedImages: AtlasSavedDocumentImageProvider? = nil
}

/// Archive identity is independent of native paths and current-root grants.
/// These values qualify only image replies from an explicitly opened archive.
struct AtlasSavedDocumentImageSource: Sendable, Equatable {
    let archiveOwner: UInt64
    let archiveDigest: String
    let sourceMember: UInt64
    let members: UInt64
    init(archiveOwner: UInt64, archiveDigest: String, sourceMember: UInt64, members: UInt64) throws {
        guard archiveOwner > 0, sourceMember < members, members <= 65_536,
              archiveDigest.utf8.count == 64,
              archiveDigest.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else {
            throw AtlasReaderError.invalidInput
        }
        self.archiveOwner = archiveOwner; self.archiveDigest = archiveDigest
        self.sourceMember = sourceMember; self.members = members
    }
}

struct AtlasSavedDocumentImageProvider: Sendable {
    let source: AtlasSavedDocumentImageSource
    let isAvailable: @Sendable () -> Bool
    let authorize: @Sendable (UInt64, UInt64) -> String?
    let prepare: @Sendable (UInt64, UInt64, UInt64, UInt64) -> String?
}

/// Supplied only by an explicit current project open, alongside that reader's
/// native access lease. Saved/retained labels and ordinary document requests
/// cannot construct it implicitly. Rust also verifies the current source bytes
/// before this grants any image read.
struct AtlasDocumentImageSource: Sendable, Equatable {
    let root: String
    let relativePath: String
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.root.utf8.elementsEqual(rhs.root.utf8) && lhs.relativePath.utf8.elementsEqual(rhs.relativePath.utf8)
    }
    init(root: String, relativePath: String) throws {
        _ = try AtlasReaderInput(root: root, path: relativePath)
        guard root.hasPrefix("/") else { throw AtlasReaderError.invalidInput }
        self.root = root; self.relativePath = relativePath
    }
}

enum AtlasReaderDocumentLimits {
    // Match DocumentReadOptions::default(), not the larger whole-source reader.
    static let sourceBytes: UInt64 = 64 * 1024
    static let flowLines: UInt64 = 8192
    static let flowItems: UInt64 = 8192
    static let blocks: UInt64 = 4096
    static let pageLines: UInt64 = 64
    static let context: UInt64 = 2048
    static let pageTextBytes: UInt64 = 256 * 1024
    static let links: UInt64 = 4096
    static let destinationBytes = 4096
}

struct AtlasDocumentByteRange: Equatable, Sendable, Decodable {
    let start: UInt64
    let end: UInt64
    var length: UInt64 { end - start }
    init(start: UInt64, end: UInt64) { self.start = start; self.end = end }
    init(from decoder: Decoder) throws {
        let value = try AtlasReaderWire.Range(from: decoder).values()
        start = value.start; end = value.end
    }
    func valid(upTo limit: UInt64, nonempty: Bool = false) -> Bool {
        start <= end && end <= limit && (!nonempty || start < end)
    }
}

/// Validate the service's once-decoded source-line fragment without deriving
/// byte offsets, decoding a URL, or treating a source line as a Markdown heading.
/// Only the captured-source service can establish the original-byte range.
struct AtlasSourceLineFragment: Equatable, Sendable {
    let first: UInt64
    let last: UInt64

    init?(_ slug: String?) {
        guard let slug, slug.utf8.count <= 43 else { return nil }
        let parts = slug.split(separator: "-", omittingEmptySubsequences: false)
        guard (1...2).contains(parts.count) else { return nil }
        func line(_ part: Substring) -> UInt64? {
            let bytes = part.utf8
            guard bytes.first == 76, bytes.count > 1,
                  let firstDigit = bytes.dropFirst().first, (49...57).contains(firstDigit),
                  bytes.dropFirst().allSatisfy({ (48...57).contains($0) }) else { return nil }
            return UInt64(part.dropFirst())
        }
        guard let first = line(parts[0]), let last = line(parts[parts.count - 1]), last >= first else { return nil }
        self.first = first; self.last = last
    }

    var label: String { first == last ? "source line \(first)" : "source lines \(first)–\(last)" }

    static func admitsSourcePath(_ bytes: [UInt8]) -> Bool {
        let name = bytes.split(separator: 47, omittingEmptySubsequences: false).last ?? []
        guard let dot = name.lastIndex(of: 46) else { return true }
        let ext = name[name.index(after: dot)...].map { (65...90).contains($0) ? $0 + 32 : $0 }
        return ext != Array("md".utf8) && ext != Array("markdown".utf8)
    }

    static func admitsIdentity(_ identity: AtlasReaderIdentity) -> Bool {
        admitsSourcePath(identity.pathBytes) && identity.capturedBytes <= AtlasReaderLimits.captureBytes &&
            ["utf8", "utf16le", "utf16be"].contains(identity.encoding)
    }

    static func admits(_ range: AtlasDocumentByteRange, in identity: AtlasReaderIdentity) -> Bool {
        guard admitsIdentity(identity),
              range.valid(upTo: identity.capturedBytes, nonempty: true) else { return false }
        switch identity.encoding {
        case "utf8": return true
        // An incomplete final UTF-16 code unit is still original source
        // content. The reader labels its replacement decoding separately.
        case "utf16le", "utf16be":
            return range.start % 2 == 0 && (range.end % 2 == 0 || range.end == identity.capturedBytes)
        default: return false
        }
    }
}

struct AtlasReaderDocumentInventory: Sendable {
    let identity: AtlasReaderIdentity
    let generation: UInt64
    let width: UInt64
    let totalLines: UInt64
    let totalHeadings: UInt64
    let renderedBytes: UInt64
    let sourceBase: UInt64
    func matches(_ other: Self) -> Bool {
        identity.matches(other.identity) && generation == other.generation && width == other.width
            && totalLines == other.totalLines && totalHeadings == other.totalHeadings
            && renderedBytes == other.renderedBytes && sourceBase == other.sourceBase
    }
}

struct AtlasReaderDocumentLine: Identifiable, Sendable, Equatable {
    let id: UInt64
    let text: String
    let rendered: AtlasDocumentByteRange
    let enclosingOriginal: AtlasDocumentByteRange?
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.id == rhs.id && lhs.text.utf8.elementsEqual(rhs.text.utf8)
            && lhs.rendered == rhs.rendered && lhs.enclosingOriginal == rhs.enclosingOriginal
    }
}

/// A flow row is not a physical source line. A target carries the original
/// page token so a delayed click cannot bind the same row number after reflow.
struct AtlasReaderDocumentTarget: Sendable, Equatable {
    let inventory: AtlasReaderDocumentInventory
    let pageID: UUID
    let line: AtlasReaderDocumentLine
    // Upstream row text trims layout trailing whitespace. Select only the
    // displayed UTF-8 prefix, not those padding bytes or the following newline.
    var rendered: AtlasDocumentByteRange {
        .init(start: line.rendered.start, end: line.rendered.start + UInt64(line.text.utf8.count))
    }
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.inventory.matches(rhs.inventory) && lhs.pageID == rhs.pageID && lhs.line == rhs.line
    }
}

struct AtlasReaderDocumentPage: Sendable {
    let id: UUID
    let inventory: AtlasReaderDocumentInventory
    let first: UInt64
    let lines: [AtlasReaderDocumentLine]
    let next: UInt64?
    let wholeDocumentVisible: Bool
    func target(at index: UInt64) -> AtlasReaderDocumentTarget? {
        guard let line = lines.first(where: { $0.id == index }), !line.text.isEmpty,
              line.enclosingOriginal != nil else { return nil }
        return .init(inventory: inventory, pageID: id, line: line)
    }
}

struct AtlasReaderDocumentHeading: Identifiable, Sendable, Equatable {
    let slug: String
    let title: String
    let renderedOffset: UInt64
    let original: AtlasDocumentByteRange
    // Swift String equality merges canonically equivalent spellings. Canonical
    // upstream slug bytes, not that equivalence or a displayed title, are IDs.
    var id: Data { Data(slug.utf8) }
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.slug.utf8.elementsEqual(rhs.slug.utf8) && lhs.title.utf8.elementsEqual(rhs.title.utf8)
            && lhs.renderedOffset == rhs.renderedOffset && lhs.original == rhs.original
    }
}
struct AtlasReaderDocumentHeadingTarget: Sendable, Equatable {
    let inventory: AtlasReaderDocumentInventory
    let pageID: UUID
    let heading: AtlasReaderDocumentHeading
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.inventory.matches(rhs.inventory) && lhs.pageID == rhs.pageID && lhs.heading == rhs.heading
    }
}
struct AtlasReaderDocumentHeadings: Sendable {
    let id: UUID
    let inventory: AtlasReaderDocumentInventory
    let first: UInt64
    let rows: [AtlasReaderDocumentHeading]
    let next: UInt64?
    func target(slug: String) -> AtlasReaderDocumentHeadingTarget? {
        rows.first(where: { $0.slug.utf8.elementsEqual(slug.utf8) }).map {
            .init(inventory: inventory, pageID: id, heading: $0)
        }
    }
}

/// These are engine classifications, not permission to open a filesystem path
/// or external URL. Occurrence IDs are local to this exact document generation.
enum AtlasReaderDocumentLinkKind: String, Sendable, Decodable {
    case heading = "same-document-heading", start = "same-document-start"
    case local = "local-reference", external, unsupported
    var navigatesWithinCapture: Bool { self == .heading || self == .start }
}
struct AtlasReaderDocumentLink: Identifiable, Sendable, Equatable {
    let id: UInt64
    let blockIndex: UInt64
    let evidence: AtlasDocumentByteRange
    let destination: String
    let display: String
    let kind: AtlasReaderDocumentLinkKind
    let fragment: String?
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.id == rhs.id && lhs.blockIndex == rhs.blockIndex && lhs.evidence == rhs.evidence
            && lhs.destination.utf8.elementsEqual(rhs.destination.utf8)
            && lhs.display.utf8.elementsEqual(rhs.display.utf8) && lhs.kind == rhs.kind
            && lhs.fragment.map({ Data($0.utf8) }) == rhs.fragment.map({ Data($0.utf8) })
    }
}
struct AtlasReaderDocumentLinkTarget: Sendable, Equatable {
    let inventory: AtlasReaderDocumentInventory
    let pageID: UUID
    let link: AtlasReaderDocumentLink
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.inventory.matches(rhs.inventory) && lhs.pageID == rhs.pageID && lhs.link == rhs.link
    }
}
struct AtlasReaderDocumentLinks: Sendable {
    let id: UUID
    let inventory: AtlasReaderDocumentInventory
    let first: UInt64
    let rows: [AtlasReaderDocumentLink]
    let next: UInt64?
    let total: UInt64
    let seen: UInt64
    let diagnostics: UInt64
    let truncated: Bool
    let complete: Bool
    func target(id: UInt64) -> AtlasReaderDocumentLinkTarget? {
        rows.first(where: { $0.id == id }).map { .init(inventory: inventory, pageID: self.id, link: $0) }
    }
    func matchesInventory(_ other: Self) -> Bool {
        inventory.matches(other.inventory) && total == other.total && seen == other.seen
            && diagnostics == other.diagnostics && truncated == other.truncated && complete == other.complete
    }
}
struct AtlasReaderDocumentLinkActivation: Sendable {
    let target: AtlasReaderDocumentLinkTarget
    // A missing page is a verified classification with no navigation effect.
    let page: AtlasReaderDocumentPage?
}

enum AtlasReaderDocumentCopyMode: UInt8, Sendable { case renderedText = 0, enclosingMarkdown = 1 }
struct AtlasReaderDocumentCopy: Sendable {
    let target: AtlasReaderDocumentTarget
    let enclosingOriginal: AtlasDocumentByteRange
    let mode: AtlasReaderDocumentCopyMode
    let value: String
}
struct AtlasReaderDocumentSource: Sendable {
    let target: AtlasReaderDocumentTarget
    let visible: AtlasDocumentByteRange
    let text: String
    let originalHex: String
    let enclosingOriginal: AtlasDocumentByteRange
    let selectionUTF8: AtlasDocumentByteRange
    let selectedHex: String
}

enum AtlasReaderDocumentRequest: Sendable {
    case prepare(width: UInt64)
    case window(first: UInt64)
    case headings(first: UInt64)
    case heading(AtlasReaderDocumentHeadingTarget)
    case fromSource(offset: UInt64)
    case source(AtlasReaderDocumentTarget)
    case copy(AtlasReaderDocumentTarget, AtlasReaderDocumentCopyMode)
    case prepareLinks
    case links(first: UInt64)
    case link(AtlasReaderDocumentLinkTarget)
    case prepareDisplay(width: UInt64)
    case displayPage(first: UInt64)
    case displayFromSource(offset: UInt64)
    var command: String {
        switch self {
        case .prepare: "document-prepare"
        case .window: "document-window"
        case .headings: "document-headings"
        case .heading: "document-heading"
        case .fromSource: "document-from-source"
        case .source: "document-source"
        case .copy: "document-copy"
        case .prepareLinks: "document-links-prepare"
        case .links: "document-links"
        case .link: "document-link"
        case .prepareDisplay: "document-display-prepare"
        case .displayPage: "document-display-page"
        case .displayFromSource: "document-display-from-source"
        }
    }
}

enum AtlasReaderDocumentResult: Sendable {
    case prepared(AtlasReaderDocumentPage, AtlasReaderDocumentHeadings)
    case page(AtlasReaderDocumentPage)
    case headings(AtlasReaderDocumentHeadings)
    case source(AtlasReaderDocumentSource)
    case copy(AtlasReaderDocumentCopy)
    case links(AtlasReaderDocumentLinks)
    case link(AtlasReaderDocumentLinkActivation)
    case display(AtlasDocumentDisplayPage, fonts: [AtlasDocumentDisplayFont]?)
}

/// Immutable worker descriptor. Only native marshaling and validation happen
/// here; the Rust reader owns all source/flow transformations and their budgets.
struct AtlasReaderDocumentJob: Sendable {
    let id: UUID
    let info: AtlasReaderInfo
    let generation: UInt64
    let inventory: AtlasReaderDocumentInventory?
    let request: AtlasReaderDocumentRequest
    var displayInventory: AtlasDocumentDisplayInventory? = nil
    var imageSource: AtlasDocumentImageSource? = nil

    func execute(_ transport: AtlasReaderDocumentTransport,
                 canceled: () -> Bool = { false }) throws -> AtlasReaderDocumentResult {
        let handle = info.identity.owner, count = AtlasReaderDocumentLimits.pageLines
        let json: String?
        if canceled() { throw AtlasReaderError.canceled }
        switch request {
        case .prepare(let width): json = transport.prepare(handle, generation, width)
        case .window(let first): json = transport.window(handle, generation, first, count)
        case .headings(let first): json = transport.headings(handle, generation, first, count)
        case .heading(let target): json = transport.heading(handle, generation, target.heading.slug, count)
        case .fromSource(let offset): json = transport.fromSource(handle, generation, offset, count)
        case .source(let target):
            json = transport.source(handle, generation, target.rendered.start, target.rendered.end, AtlasReaderDocumentLimits.context)
        case .copy(let target, let mode):
            json = transport.copy(handle, generation, target.rendered.start, target.rendered.end, mode.rawValue)
        case .prepareLinks:
            guard let prepare = transport.prepareLinks else { throw AtlasReaderError.unavailable }
            json = prepare(handle, generation, AtlasReaderDocumentLimits.links)
        case .links(let first):
            guard let links = transport.links else { throw AtlasReaderError.unavailable }
            json = links(handle, generation, first, count)
        case .link(let target):
            guard let link = transport.link else { throw AtlasReaderError.unavailable }
            json = link(handle, generation, target.link.id, count)
        case .prepareDisplay(let width):
            guard let prepare = transport.prepareDisplay, let font = transport.displayFont,
                  let inventory else { throw AtlasReaderError.unavailable }
            let page = try AtlasDocumentDisplayWire.page(prepare(handle, generation, width),
                document: inventory, command: request.command, first: 0, width: width)
            var fonts: [AtlasDocumentDisplayFont] = []
            for spec in page.inventory.fonts {
                var bytes = Data(); bytes.reserveCapacity(Int(spec.bytes))
                while UInt64(bytes.count) < spec.bytes {
                    if canceled() { throw AtlasReaderError.canceled }
                    let offset = UInt64(bytes.count)
                    bytes.append(try AtlasDocumentDisplayWire.fontChunk(
                        font(handle, generation, spec.index, offset, AtlasDocumentDisplayLimits.fontChunk),
                        inventory: page.inventory, spec: spec, offset: offset))
                }
                if canceled() { throw AtlasReaderError.canceled }
                fonts.append(try AtlasDocumentDisplayFont(index: spec.index, data: bytes))
            }
            try page.validateFonts(fonts)
            if canceled() { throw AtlasReaderError.canceled }
            return .display(try loadImages(page, transport: transport, canceled: canceled), fonts: fonts)
        case .displayPage(let first):
            guard let load = transport.displayPage, let inventory, let displayInventory else {
                throw AtlasReaderError.unavailable
            }
            let page = try AtlasDocumentDisplayWire.page(
                load(handle, generation, first, AtlasDocumentDisplayLimits.pageItems),
                document: inventory, command: request.command, first: first, previous: displayInventory)
            if canceled() { throw AtlasReaderError.canceled }
            return .display(try loadImages(page, transport: transport, canceled: canceled), fonts: nil)
        case .displayFromSource(let offset):
            guard let load = transport.displayFromSource, let inventory, let displayInventory else {
                throw AtlasReaderError.unavailable
            }
            let page = try AtlasDocumentDisplayWire.page(
                load(handle, generation, offset, AtlasDocumentDisplayLimits.pageItems),
                document: inventory, command: request.command, first: nil, sourceAnchor: offset,
                previous: displayInventory)
            if canceled() { throw AtlasReaderError.canceled }
            return .display(try loadImages(page, transport: transport, canceled: canceled), fonts: nil)
        }
        if canceled() { throw AtlasReaderError.canceled }
        return try decode(json)
    }

    /// Runs wholly on the existing bounded reader worker. Parser-owned image
    /// references select assets; native code never resolves destinations or
    /// measures a replacement layout. A refused optional image leaves its
    /// upstream slot available with alt text.
    private func loadImages(_ page: AtlasDocumentDisplayPage,
                            transport: AtlasReaderDocumentTransport,
                            canceled: () -> Bool) throws -> AtlasDocumentDisplayPage {
        if canceled() { throw AtlasReaderError.canceled }
        guard !page.imageTargets.isEmpty else { return page }
        let saved = transport.savedImages
        guard imageSource == nil || saved == nil else { throw AtlasReaderError.invalidResponse }
        guard imageSource != nil || saved != nil else {
            return try page.withImages([], notice: "Local images need current project folder access or an opened saved archive.")
        }
        guard let prepare = saved?.prepare ?? transport.imagePrepare, let chunk = transport.imageChunk,
              saved != nil || transport.imagesAuthorize != nil else {
            return try page.withImages([], notice: "Local image loading is unavailable in this host.")
        }
        let revokedNotice = "This saved archive was closed or replaced. Already loaded pages remain readable; open a new saved reader to load more images."
        func available() -> Bool { saved?.isAvailable() ?? true }
        guard available() else { return try page.withImages([], notice: revokedNotice) }
#if canImport(ImageIO) && canImport(CoreGraphics) && canImport(CryptoKit)
        let handle = info.identity.owner
        let authorization: AtlasDocumentImageAuthorization
        do {
            if let saved {
                let json = saved.authorize(handle, generation)
                if canceled() { throw AtlasReaderError.canceled }
                guard available() else { return try page.withImages([], notice: revokedNotice) }
                authorization = try AtlasDocumentDisplayWire.savedImageAuthorization(json,
                    document: page.inventory.document, source: saved.source)
            } else {
                guard let source = imageSource, let authorize = transport.imagesAuthorize else {
                    throw AtlasReaderError.invalidResponse
                }
                authorization = try AtlasDocumentDisplayWire.imageAuthorization(
                    authorize(handle, generation, source.root, source.relativePath),
                    document: page.inventory.document, root: source.root, sourceRelative: source.relativePath)
            }
        } catch let error as AtlasReaderError {
            if canceled() { throw AtlasReaderError.canceled }
            guard available() else { return try page.withImages([], notice: revokedNotice) }
            if case .engine = error {
                return try page.withImages([], notice: saved == nil
                    ? "Local images were not loaded because the current source or folder access could not be verified."
                    : "Saved images remain placeholders because the captured source or archive access could not be verified.")
            }
            throw error
        }
        if canceled() { throw AtlasReaderError.canceled }
        guard available() else { return try page.withImages([], notice: revokedNotice) }
        guard authorization.displayWidth == page.inventory.width,
              info.captureSHA256 == nil || info.captureSHA256 == authorization.captureSHA256 else {
            throw AtlasReaderError.invalidResponse
        }
        var images: [AtlasDocumentDisplayImage] = []
        var retainedBytes = 0
        var refused = page.imageTargets.count > AtlasDocumentImageLimits.pageImages
        for target in page.imageTargets.prefix(AtlasDocumentImageLimits.pageImages) {
            if canceled() { throw AtlasReaderError.canceled }
            guard available() else { return try page.withImages([], notice: revokedNotice) }
            if retainedBytes == AtlasDocumentImageLimits.pageBytes { refused = true; break }
            do {
                let json = prepare(handle, generation, target.itemIndex, target.requestID)
                if canceled() { throw AtlasReaderError.canceled }
                guard available() else { return try page.withImages([], notice: revokedNotice) }
                let receipt = try AtlasDocumentDisplayWire.imagePrepared(json,
                    target: target, authorization: authorization)
                var bytes = Data()
                bytes.reserveCapacity(Int(receipt.totalBytes))
                bytes.append(receipt.firstChunk)
                while UInt64(bytes.count) < receipt.totalBytes {
                    if canceled() { throw AtlasReaderError.canceled }
                    guard available() else { return try page.withImages([], notice: revokedNotice) }
                    let offset = UInt64(bytes.count)
                    let next = try AtlasDocumentDisplayWire.imageChunk(
                        chunk(handle, generation, target.itemIndex, target.requestID, offset, 64 * 1024),
                        receipt: receipt, offset: offset)
                    guard !next.isEmpty else { throw AtlasReaderError.invalidResponse }
                    bytes.append(next)
                }
                try AtlasDocumentDisplayWire.verifyImage(bytes, receipt: receipt)
                if canceled() { throw AtlasReaderError.canceled }
                guard available() else { return try page.withImages([], notice: revokedNotice) }
                let image = try AtlasDocumentImageDecoder.decode(bytes, target: target,
                    remainingBytes: AtlasDocumentImageLimits.pageBytes - retainedBytes, canceled: canceled)
                guard image.format == receipt.format else { throw AtlasReaderError.invalidResponse }
                retainedBytes += image.byteCount
                images.append(image)
            } catch let error as AtlasReaderError {
                if canceled() { throw AtlasReaderError.canceled }
                guard available() else { return try page.withImages([], notice: revokedNotice) }
                // Policy refusals leave placeholders. Malformed/cross-generation
                // receipts are rejected before any decoded image is published.
                if case .engine = error { refused = true; continue }
                throw error
            } catch is AtlasDocumentImageError {
                if canceled() { throw AtlasReaderError.canceled }
                refused = true
            }
        }
        if canceled() { throw AtlasReaderError.canceled }
        guard available() else { return try page.withImages([], notice: revokedNotice) }
        return try page.withImages(images, notice: refused ? (saved == nil
            ? "Some images remain placeholders: only bounded local PNG/JPEG files are available."
            : "Some images remain placeholders: only captured PNG/JPEG assets within the saved-image limits can be displayed.") : nil)
#else
        // Portable wire consumers never claim Apple image decoding and never
        // read assets they cannot display.
        _ = prepare; _ = chunk
        return try page.withImages([], notice: "Image decoding is unavailable in this host.")
#endif
    }

    func decode(_ json: String?) throws -> AtlasReaderDocumentResult {
        let data = try AtlasReaderWire.admit(json, handle: info.identity.owner)
        do {
            let wire = try JSONDecoder().decode(AtlasReaderDocumentWire.self, from: data)
            guard let header = wire.header else { throw AtlasReaderError.invalidResponse }
            let identity = try header.identity(handle: info.identity.owner)
            guard identity.matches(info.identity), header.command == request.command else {
                throw AtlasReaderError.invalidResponse
            }
            switch request {
            case .copy(let target, let mode): return .copy(try wire.copy(target, mode: mode))
            case .source(let target): return .source(try wire.source(target))
            default: break
            }
            let basis = try wire.inventory(identity: identity, generation: generation)
            if let inventory, !inventory.matches(basis) { throw AtlasReaderError.invalidResponse }
            switch request {
            case .prepare(let width):
                guard basis.width == width else { throw AtlasReaderError.invalidResponse }
                return .prepared(try wire.page(basis, expectedFirst: 0), try wire.headings(basis, first: 0))
            case .window(let first): return .page(try wire.page(basis, expectedFirst: first))
            case .headings(let first): return .headings(try wire.headings(basis, first: first))
            case .heading(let target):
                guard try wire.text("heading_slug").utf8.elementsEqual(target.heading.slug.utf8),
                      try wire.range("heading_original_range") == target.heading.original else {
                    throw AtlasReaderError.invalidResponse
                }
                let page = try wire.page(basis)
                // The upstream heading selects the first row whose rendered end
                // is after its offset. Empty EOF is possible for synthetic flow.
                if let first = page.lines.first {
                    guard first.rendered.end > target.heading.renderedOffset else { throw AtlasReaderError.invalidResponse }
                }
                return .page(page)
            case .fromSource(let offset):
                guard try wire.number("original_anchor") == offset else { throw AtlasReaderError.invalidResponse }
                return .page(try wire.page(basis))
            case .prepareLinks: return .links(try wire.links(basis, first: 0))
            case .links(let first): return .links(try wire.links(basis, first: first))
            case .link(let target): return .link(try wire.link(basis, target: target))
            case .source, .copy: throw AtlasReaderError.invalidResponse
            case .prepareDisplay, .displayPage, .displayFromSource: throw AtlasReaderError.invalidResponse
            }
        } catch let error as AtlasReaderError { throw error }
        catch { throw AtlasReaderError.invalidResponse }
    }
}

/// Admission belongs to the main actor, foreign work to the existing reader
/// lane. Search/outline counters and captures are never borrowed or reset here.
@MainActor final class AtlasReaderDocumentState {
    private var generation: UInt64 = 0
    private var active: UUID?
    private(set) var inventory: AtlasReaderDocumentInventory?
    private(set) var page: AtlasReaderDocumentPage?
    private(set) var headings: AtlasReaderDocumentHeadings?
    private(set) var links: AtlasReaderDocumentLinks?
    private(set) var displayInventory: AtlasDocumentDisplayInventory?

    func admit(_ request: AtlasReaderDocumentRequest, info: AtlasReaderInfo) throws -> AtlasReaderDocumentJob {
        let next: UInt64
        switch request {
        case .prepare(let width):
            guard (4...512).contains(width) else { throw AtlasReaderError.invalidInput }
            let (value, overflow) = generation.addingReportingOverflow(1)
            guard !overflow else { throw AtlasReaderError.unavailable }
            next = value
        default:
            guard let inventory, inventory.identity.matches(info.identity) else { throw AtlasReaderError.unavailable }
            next = inventory.generation
            switch request {
            case .window(let first):
                guard first <= inventory.totalLines else { throw AtlasReaderError.invalidInput }
            case .headings(let first):
                guard first <= inventory.totalHeadings else { throw AtlasReaderError.invalidInput }
            case .heading(let target):
                guard headings?.target(slug: target.heading.slug) == target else { throw AtlasReaderError.invalidInput }
            case .fromSource(let offset):
                guard offset >= inventory.sourceBase, offset <= info.identity.capturedBytes else { throw AtlasReaderError.invalidInput }
            case .source(let target), .copy(let target, _):
                guard page?.target(at: target.line.id) == target else { throw AtlasReaderError.invalidInput }
            case .links(let first):
                guard let links, first <= links.total else { throw AtlasReaderError.invalidInput }
            case .link(let target):
                guard links?.target(id: target.link.id) == target else { throw AtlasReaderError.invalidInput }
            case .prepareDisplay(let width):
                guard (160...4096).contains(width),
                      displayInventory == nil || displayInventory?.width == width else {
                    throw AtlasReaderError.invalidInput
                }
            case .displayPage(let first):
                guard let displayInventory, displayInventory.document.matches(inventory),
                      first <= displayInventory.totalItems else { throw AtlasReaderError.invalidInput }
            case .displayFromSource(let offset):
                guard let displayInventory, displayInventory.document.matches(inventory),
                      displayInventory.qualifiedSources, offset >= inventory.sourceBase,
                      offset <= info.identity.capturedBytes else { throw AtlasReaderError.invalidInput }
            case .prepareLinks, .prepare: break
            }
        }
        if case .prepare = request {
            generation = next
            // A foreign reflow may succeed before a canceled/invalid response.
            // Retire native authority now; an old visible preview is read-only.
            revoke()
        }
        if case .prepareLinks = request {
            // Foreign parsing may commit before its delivery is canceled.
            // Retire the prior link-list token before entering that operation.
            links = nil
        }
        var job = AtlasReaderDocumentJob(id: UUID(), info: info, generation: next, inventory: inventory, request: request)
        job.displayInventory = displayInventory
        active = job.id
        return job
    }

    func accept(_ result: AtlasReaderDocumentResult, job: AtlasReaderDocumentJob) throws {
        guard active == job.id else { throw AtlasReaderError.canceled }
        if let expected = job.inventory, inventory?.matches(expected) != true { throw AtlasReaderError.canceled }
        switch result {
        case .prepared(let page, let headings): inventory = page.inventory; self.page = page; self.headings = headings
        case .page(let page): self.page = page
        case .headings(let headings): self.headings = headings
        case .links(let links):
            if let previous = self.links, !previous.matchesInventory(links) { throw AtlasReaderError.invalidResponse }
            self.links = links
        case .link(let activation):
            guard links?.target(id: activation.target.link.id) == activation.target else { throw AtlasReaderError.canceled }
            if let page = activation.page { self.page = page }
        case .source(let source):
            guard page?.target(at: source.target.line.id) == source.target else { throw AtlasReaderError.canceled }
        case .copy(let copy):
            guard page?.target(at: copy.target.line.id) == copy.target else { throw AtlasReaderError.canceled }
        case .display(let page, _):
            guard inventory?.matches(page.inventory.document) == true else { throw AtlasReaderError.canceled }
            if let previous = displayInventory, !previous.matches(page.inventory) { throw AtlasReaderError.invalidResponse }
            displayInventory = page.inventory
        }
        active = nil
    }
    func abandon(_ job: AtlasReaderDocumentJob) { if active == job.id { active = nil } }
    func revoke() { active = nil; inventory = nil; page = nil; headings = nil; links = nil; displayInventory = nil }
    func close() { revoke(); generation = 0 }
}

/// Keep arrays bounded during decoding, not only after JSON materialization.
/// This wrapper never leaves the worker or becomes presentation state.
private struct AtlasReaderDocumentWire: Decodable {
    struct Key: CodingKey {
        let stringValue: String
        var intValue: Int? { nil }
        init(_ value: String) { stringValue = value }
        init?(stringValue: String) { self.init(stringValue) }
        init?(intValue: Int) { return nil }
    }
    let header: AtlasReaderWire.Header?
    let fields: KeyedDecodingContainer<Key>
    init(from decoder: Decoder) throws {
        header = try .init(from: decoder)
        fields = try decoder.container(keyedBy: Key.self)
    }
    func text(_ key: String) throws -> String { try fields.decode(String.self, forKey: Key(key)) }
    func number(_ key: String) throws -> UInt64 {
        guard let value = AtlasReaderWire.integer(try text(key)) else { throw AtlasReaderError.invalidResponse }
        return value
    }
    func optionalNumber(_ key: String) throws -> UInt64? {
        guard let value = try fields.decodeIfPresent(String.self, forKey: Key(key)) else { return nil }
        guard let number = AtlasReaderWire.integer(value) else { throw AtlasReaderError.invalidResponse }
        return number
    }
    func flag(_ key: String) throws -> Bool { try fields.decode(Bool.self, forKey: Key(key)) }
    func range(_ key: String) throws -> AtlasDocumentByteRange { try fields.decode(AtlasDocumentByteRange.self, forKey: Key(key)) }
    func absent(_ keys: [String]) throws -> Bool {
        for key in keys where fields.contains(Key(key)) {
            if try !fields.decodeNil(forKey: Key(key)) { return false }
        }
        return true
    }
    func array<T: Decodable>(_ key: String) throws -> [T] {
        var container = try fields.nestedUnkeyedContainer(forKey: Key(key))
        if let count = container.count, count > Int(AtlasReaderDocumentLimits.pageLines) { throw AtlasReaderError.invalidResponse }
        var values: [T] = []
        while !container.isAtEnd {
            guard values.count < Int(AtlasReaderDocumentLimits.pageLines) else { throw AtlasReaderError.invalidResponse }
            values.append(try container.decode(T.self))
        }
        return values
    }
    func inventory(identity: AtlasReaderIdentity, generation: UInt64) throws -> AtlasReaderDocumentInventory {
        guard generation > 0, try number("document_generation") == generation,
              identity.encoding == "utf8", identity.capturedBytes <= AtlasReaderDocumentLimits.sourceBytes,
              try flag("document_ready"), try flag("layout_complete"), try !flag("native_shaped"),
              try text("rendering") == "logical-frankenmarkdown-flow",
              try text("source_mapping") == "enclosing-regions-not-glyph-exact" else { throw AtlasReaderError.invalidResponse }
        let width = try number("width_columns"), lines = try number("total_flow_lines")
        let headings = try number("total_headings"), bytes = try number("rendered_utf8_bytes")
        let base = try number("parser_source_base")
        guard (4...512).contains(width), lines <= AtlasReaderDocumentLimits.flowLines,
              headings <= AtlasReaderDocumentLimits.blocks, bytes <= 64 * 1024 * 1024,
              (base == 0 || base == 3), base <= identity.capturedBytes else { throw AtlasReaderError.invalidResponse }
        return .init(identity: identity, generation: generation, width: width, totalLines: lines,
            totalHeadings: headings, renderedBytes: bytes, sourceBase: base)
    }
    func next(_ key: String, first: UInt64, count: Int, total: UInt64) throws -> UInt64? {
        guard first <= total, UInt64(count) == min(AtlasReaderDocumentLimits.pageLines, total - first) else {
            throw AtlasReaderError.invalidResponse
        }
        let next = try optionalNumber(key), end = first + UInt64(count)
        guard next == (end < total ? end : nil) else { throw AtlasReaderError.invalidResponse }
        return next
    }
    func page(_ inventory: AtlasReaderDocumentInventory, expectedFirst: UInt64? = nil) throws -> AtlasReaderDocumentPage {
        struct Row: Decodable {
            let flow_line, text: String
            let rendered_utf8_range: AtlasDocumentByteRange
            let enclosing_original_range: AtlasDocumentByteRange?
        }
        let first = try number("first_flow_line")
        if let expectedFirst, first != expectedFirst { throw AtlasReaderError.invalidResponse }
        let rows: [Row] = try array("flow_lines")
        let next = try next("next_flow_line", first: first, count: rows.count, total: inventory.totalLines)
        let whole = try flag("whole_document_visible")
        guard whole == (first == 0 && first + UInt64(rows.count) == inventory.totalLines) else { throw AtlasReaderError.invalidResponse }
        var previousEnd: UInt64 = 0, textBytes: UInt64 = 0
        let lines = try rows.enumerated().map { index, row -> AtlasReaderDocumentLine in
            let range = row.rendered_utf8_range, count = UInt64(row.text.utf8.count)
            textBytes += count
            guard AtlasReaderWire.integer(row.flow_line) == first + UInt64(index),
                  range.valid(upTo: inventory.renderedBytes), range.start >= previousEnd,
                  count <= range.length, textBytes <= AtlasReaderDocumentLimits.pageTextBytes else {
                throw AtlasReaderError.invalidResponse
            }
            previousEnd = range.end
            if let original = row.enclosing_original_range {
                guard original.valid(upTo: inventory.identity.capturedBytes, nonempty: true), original.start >= inventory.sourceBase else {
                    throw AtlasReaderError.invalidResponse
                }
            }
            return .init(id: first + UInt64(index), text: row.text, rendered: range, enclosingOriginal: row.enclosing_original_range)
        }
        return .init(id: UUID(), inventory: inventory, first: first, lines: lines, next: next, wholeDocumentVisible: whole)
    }
    func headings(_ inventory: AtlasReaderDocumentInventory, first: UInt64) throws -> AtlasReaderDocumentHeadings {
        struct Row: Decodable {
            let slug, title, rendered_utf8_offset: String
            let original_range: AtlasDocumentByteRange
        }
        let rows: [Row] = try array("headings")
        let next = try next("next_heading", first: first, count: rows.count, total: inventory.totalHeadings)
        var slugs = Set<Data>()
        let headings = try rows.map { row -> AtlasReaderDocumentHeading in
            guard !row.slug.isEmpty, row.slug.utf8.count <= 4096, !row.slug.utf8.contains(0),
                  row.title.utf8.count <= Int(AtlasReaderDocumentLimits.sourceBytes),
                  slugs.insert(Data(row.slug.utf8)).inserted,
                  let offset = AtlasReaderWire.integer(row.rendered_utf8_offset), offset <= inventory.renderedBytes,
                  row.original_range.valid(upTo: inventory.identity.capturedBytes, nonempty: true),
                  row.original_range.start >= inventory.sourceBase else { throw AtlasReaderError.invalidResponse }
            return .init(slug: row.slug, title: row.title, renderedOffset: offset, original: row.original_range)
        }
        return .init(id: UUID(), inventory: inventory, first: first, rows: headings, next: next)
    }
    private struct LinkRow: Decodable {
        let link_id, block_index, destination, destination_display: String
        let evidence_range: AtlasDocumentByteRange
        let kind: AtlasReaderDocumentLinkKind
        let fragment_slug: String?
        func value(inventory: AtlasReaderDocumentInventory) throws -> AtlasReaderDocumentLink {
            guard let id = AtlasReaderWire.integer(link_id), id > 0,
                  let block = AtlasReaderWire.integer(block_index), block < 131_072,
                  evidence_range == AtlasDocumentByteRange(start: 0, end: inventory.identity.capturedBytes),
                  destination.utf8.count <= AtlasReaderDocumentLimits.destinationBytes,
                  destination_display.utf8.count <= AtlasReaderDocumentLimits.destinationBytes * 8 else {
                throw AtlasReaderError.invalidResponse
            }
            if let fragment_slug {
                guard !fragment_slug.isEmpty,
                      fragment_slug.utf8.count <= AtlasReaderDocumentLimits.destinationBytes,
                      !fragment_slug.utf8.contains(0) else { throw AtlasReaderError.invalidResponse }
            }
            guard kind != .heading || fragment_slug != nil else { throw AtlasReaderError.invalidResponse }
            return .init(id: id, blockIndex: block, evidence: evidence_range,
                destination: destination, display: destination_display, kind: kind, fragment: fragment_slug)
        }
    }
    private func linkSummary() throws -> (total: UInt64, seen: UInt64, diagnostics: UInt64, truncated: Bool, complete: Bool) {
        guard try flag("links_ready"), try text("evidence_precision") == "whole-captured-markdown-document" else {
            throw AtlasReaderError.invalidResponse
        }
        let total = try number("retained_links"), seen = try number("links_seen")
        let diagnostics = try number("parser_diagnostics"), truncated = try flag("links_truncated")
        let complete = try flag("links_complete")
        guard total <= AtlasReaderDocumentLimits.links, total <= seen, seen <= 131_072,
              diagnostics <= 131_072, !complete || (!truncated && total == seen && diagnostics == 0) else {
            throw AtlasReaderError.invalidResponse
        }
        return (total, seen, diagnostics, truncated, complete)
    }
    func links(_ inventory: AtlasReaderDocumentInventory, first: UInt64) throws -> AtlasReaderDocumentLinks {
        let summary = try linkSummary()
        guard try number("first_link") == first else { throw AtlasReaderError.invalidResponse }
        let wire: [LinkRow] = try array("links")
        let next = try next("next_link", first: first, count: wire.count, total: summary.total)
        var prior: UInt64 = 0
        let rows = try wire.map { row -> AtlasReaderDocumentLink in
            let link = try row.value(inventory: inventory)
            guard link.id > prior, link.id <= summary.seen else { throw AtlasReaderError.invalidResponse }
            prior = link.id
            return link
        }
        return .init(id: UUID(), inventory: inventory, first: first, rows: rows, next: next,
            total: summary.total, seen: summary.seen, diagnostics: summary.diagnostics,
            truncated: summary.truncated, complete: summary.complete)
    }
    func link(_ inventory: AtlasReaderDocumentInventory,
              target: AtlasReaderDocumentLinkTarget) throws -> AtlasReaderDocumentLinkActivation {
        let summary = try linkSummary()
        let row = try fields.decode(LinkRow.self, forKey: Key("link")).value(inventory: inventory)
        guard target.inventory.matches(inventory), row == target.link, row.id <= summary.seen else {
            throw AtlasReaderError.invalidResponse
        }
        let activated = try flag("activated")
        guard activated == row.kind.navigatesWithinCapture else { throw AtlasReaderError.invalidResponse }
        guard activated else {
            guard try absent(["flow_lines", "first_flow_line", "next_flow_line", "whole_document_visible",
                              "heading_slug", "heading_original_range"]) else { throw AtlasReaderError.invalidResponse }
            return .init(target: target, page: nil)
        }
        let page = try self.page(inventory, expectedFirst: row.kind == .start ? 0 : nil)
        if row.kind == .heading {
            guard let fragment = row.fragment, try text("heading_slug").utf8.elementsEqual(fragment.utf8),
                  try range("heading_original_range").valid(upTo: inventory.identity.capturedBytes, nonempty: true),
                  try range("heading_original_range").start >= inventory.sourceBase else {
                throw AtlasReaderError.invalidResponse
            }
        } else if try !absent(["heading_slug", "heading_original_range"]) {
            throw AtlasReaderError.invalidResponse
        }
        return .init(target: target, page: page)
    }
    func selection(_ target: AtlasReaderDocumentTarget) throws {
        guard try number("document_generation") == target.inventory.generation,
              try text("selection_namespace") == "document",
              try text("source_mapping") == "enclosing-regions-not-glyph-exact",
              try range("rendered_utf8_range") == target.rendered,
              try absent(["query_generation", "outline_generation", "symbol_id", "declaration_line", "semantic_resolution",
                          "reference_generation", "reference_id", "reference_line", "includes_comments_and_literals", "compiler_resolved"]) else {
            throw AtlasReaderError.invalidResponse
        }
    }
    func copy(_ target: AtlasReaderDocumentTarget, mode: AtlasReaderDocumentCopyMode) throws -> AtlasReaderDocumentCopy {
        try selection(target)
        let original = try range("enclosing_original_range")
        guard original.valid(upTo: target.inventory.identity.capturedBytes, nonempty: true),
              original.start >= target.inventory.sourceBase, original.length <= AtlasReaderLimits.maximumPageBytes else {
            throw AtlasReaderError.invalidResponse
        }
        let value: String
        switch mode {
        case .renderedText:
            value = try text("text")
            guard try text("copy_domain") == "rendered-text-utf8", try absent(["original_hex"]),
                  value.utf8.elementsEqual(target.line.text.utf8) else { throw AtlasReaderError.invalidResponse }
        case .enclosingMarkdown:
            value = try text("original_hex")
            guard try text("copy_domain") == "enclosing-original-markdown", try absent(["text"]),
                  AtlasReaderWire.validHex(value, bytes: original.length) else { throw AtlasReaderError.invalidResponse }
        }
        return .init(target: target, enclosingOriginal: original, mode: mode, value: value)
    }
    func source(_ target: AtlasReaderDocumentTarget) throws -> AtlasReaderDocumentSource {
        // This nested value is a document selection, not a standalone response.
        let selected = try fields.decode(Selection.self, forKey: Key("selection"))
        try selected.fields.selection(target)
        let raw = try selected.fields.range("original_range"), utf8 = try selected.fields.range("window_utf8_range")
        let hex = try selected.fields.text("original_hex")
        let visible = try range("visible_range"), requested = try range("requested_original_range")
        let sourceLength = target.inventory.identity.capturedBytes
        let text = try text("text"), pageHex = try self.text("original_hex")
        let padding = AtlasReaderDocumentLimits.context + 4
        guard raw.valid(upTo: sourceLength, nonempty: true), raw.start >= target.inventory.sourceBase,
              visible.valid(upTo: sourceLength), requested.valid(upTo: sourceLength),
              requested.start == raw.start - min(padding, raw.start),
              requested.end == raw.end + min(padding, sourceLength - raw.end),
              visible.start <= raw.start, visible.end >= raw.end,
              visible.start >= requested.start - min(8, requested.start),
              visible.start <= min(sourceLength, requested.start + 8),
              visible.end >= requested.end - min(8, requested.end),
              visible.end <= min(sourceLength, requested.end + 8),
              try !flag("range_limited"), try flag("boundaries_adjusted") == (visible != requested),
              try !flag("has_replacements"), try absent(["first_physical_line"]),
              try self.text("text_kind") == "logical-captured-text-not-shaped",
              UInt64(text.utf8.count) <= AtlasReaderDocumentLimits.pageTextBytes,
              utf8.valid(upTo: UInt64(text.utf8.count), nonempty: true),
              AtlasReaderWire.validHex(pageHex, bytes: visible.length),
              AtlasReaderWire.validHex(hex, bytes: raw.length) else { throw AtlasReaderError.invalidResponse }
        if let next = try optionalNumber("next_offset"), next != visible.end { throw AtlasReaderError.invalidResponse }
        let pageBytes = Array(pageHex.utf8), decoded = Array(text.utf8)
        guard pageBytes[Int((raw.start - visible.start) * 2)..<Int((raw.end - visible.start) * 2)].elementsEqual(hex.utf8),
              String(bytes: decoded[Int(utf8.start)..<Int(utf8.end)], encoding: .utf8) != nil,
              decoded[Int(utf8.start)..<Int(utf8.end)].elementsEqual(Self.hexBytes(hex)) else {
            throw AtlasReaderError.invalidResponse
        }
        return .init(target: target, visible: visible, text: text, originalHex: pageHex,
            enclosingOriginal: raw, selectionUTF8: utf8, selectedHex: hex)
    }
    private static func hexBytes(_ validatedHex: String) -> [UInt8] {
        let bytes = Array(validatedHex.utf8)
        func nibble(_ b: UInt8) -> UInt8 { b <= 57 ? b - 48 : b - 87 }
        return stride(from: 0, to: bytes.count, by: 2).map { nibble(bytes[$0]) * 16 + nibble(bytes[$0 + 1]) }
    }
    private struct Selection: Decodable {
        let fields: AtlasReaderDocumentWire
        init(from decoder: Decoder) throws {
            // The nested object has no source header. Reuse only keyed field
            // validation, with the outer response's already-verified header.
            let c = try decoder.container(keyedBy: Key.self)
            fields = AtlasReaderDocumentWire(fields: c)
        }
    }
    private init(fields: KeyedDecodingContainer<Key>) {
        // Nested selections have no identity header. Full responses require
        // that header in init(from:); only selection fields use this initializer.
        self.fields = fields
        header = nil
    }
}
