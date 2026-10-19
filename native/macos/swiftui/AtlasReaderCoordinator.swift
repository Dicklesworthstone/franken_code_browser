import Foundation
import Dispatch

/// Injected only at the C-marshaling boundary. The production implementation
/// invokes the retained Rust reader; tests exercise this coordinator and the
/// production bounded I/O queue without linking Apple frameworks or Rust.
struct AtlasReaderTransport: Sendable {
    /// The native boundary hashes the same immutable buffer it submits to Rust.
    /// The response's independently computed capture digest must match before
    /// the supplied reader may publish a page. No live-file fallback exists.
    struct SuppliedResponse: Sendable {
        let json: String?
        let sha256: String
    }
    let create: @Sendable () -> UInt64
    let open: @Sendable (UInt64, String, UInt64) -> String?
    let window: @Sendable (UInt64, UInt64, UInt64) -> String?
    let lines: @Sendable (UInt64, UInt64, UInt64, UInt64) -> String?
    let cancel: @Sendable (UInt64) -> Bool
    let close: @Sendable (UInt64) -> Bool
    let retirementFailed: @Sendable () -> Void
    /// Optional retained-byte transfer. It initializes an empty destination;
    /// it never opens a pathname or grants source/query authority by itself.
    let fork: (@Sendable (UInt64, UInt64) -> String?)?
    let openBytes: (@Sendable (UInt64, String, Data) -> SuppliedResponse?)?
    let comparison: AtlasReaderComparisonTransport?

    init(create: @escaping @Sendable () -> UInt64,
         open: @escaping @Sendable (UInt64, String, UInt64) -> String?,
         window: @escaping @Sendable (UInt64, UInt64, UInt64) -> String?,
         lines: @escaping @Sendable (UInt64, UInt64, UInt64, UInt64) -> String?,
         cancel: @escaping @Sendable (UInt64) -> Bool,
         close: @escaping @Sendable (UInt64) -> Bool,
         retirementFailed: @escaping @Sendable () -> Void,
         fork: (@Sendable (UInt64, UInt64) -> String?)? = nil,
         openBytes: (@Sendable (UInt64, String, Data) -> SuppliedResponse?)? = nil,
         comparison: AtlasReaderComparisonTransport? = nil) {
        self.create = create; self.open = open; self.window = window; self.lines = lines
        self.cancel = cancel; self.close = close; self.retirementFailed = retirementFailed
        self.fork = fork
        self.openBytes = openBytes
        self.comparison = comparison
    }
}

/// An owner-local, single-use proposal, not a source page or a granted handle.
/// Only commitOpen can publish its validated capture. Cancellation, another
/// operation, or closing the coordinator revokes it synchronously.
struct AtlasReaderReplacement: Sendable {
    fileprivate let id: UUID
}

/// This reference is retained, never operated on, by background work. It may
/// hold the native security-scoped root grant for the entire capture lifetime.
private final class AtlasReaderAccessLease: @unchecked Sendable {
    let reference: AnyObject?
    init(_ reference: AnyObject?) { self.reference = reference }
}

/// All fields are immutable; mutable Rust state is protected by its registry.
/// Foreign calls retain this object until return. Its last Swift owner queues
/// retirement rather than deallocating a large Rust capture on the main actor.
private final class AtlasReaderSession: @unchecked Sendable {
    private static let retirement = DispatchQueue(label: "dev.frankencode.browser.reader-retirement", qos: .utility)
    let handle: UInt64
    let transport: AtlasReaderTransport
    private let access: AtlasReaderAccessLease

    init(transport: AtlasReaderTransport, accessLease: AnyObject?) throws {
        let handle = transport.create()
        guard handle != 0 else { throw AtlasReaderError.unavailable }
        self.handle = handle
        self.transport = transport
        access = AtlasReaderAccessLease(accessLease)
    }

    /// Share the native grant lifetime, not the source reader or mutable state.
    /// Rust shares immutable source backing and its reservation; each fork
    /// admits and retires its private reader state independently.
    init(transport: AtlasReaderTransport, sharingAccessFrom source: AtlasReaderSession) throws {
        let handle = transport.create()
        guard handle != 0 else { throw AtlasReaderError.unavailable }
        self.handle = handle
        self.transport = transport
        access = source.access
    }

    func cancel() { _ = transport.cancel(handle) }

    deinit {
        let handle = handle, transport = transport, access = access
        Self.retirement.async {
            withExtendedLifetime(access) {
                // The registry uses try_lock and can briefly refuse a close.
                // At most eight reader handles exist, so this retirement queue
                // cannot retain an unbounded number of source captures. Retry
                // contention for a bounded interval; never claim failed cleanup.
                for attempt in 0..<8 {
                    if transport.close(handle) { return }
                    if attempt < 7 { Thread.sleep(forTimeInterval: 0.001) }
                }
                transport.retirementFailed()
            }
        }
    }
}

/// One accepted capture plus at most one newest replacement proposal in the
/// existing one-active/one-newest I/O lane. In-flight foreign calls retain their
/// own handles/grants through drain; the Rust registry bounds total admission.
@MainActor final class AtlasReaderCoordinator {
    private let transport: AtlasReaderTransport
    private let searchTransport: AtlasReaderSearchTransport?
    private let outlineTransport: AtlasReaderOutlineTransport?
    private let documentTransport: AtlasReaderDocumentTransport?
    private let documentState = AtlasReaderDocumentState()
    private var activeDocumentJob: AtlasReaderDocumentJob?
    private var outlineInventory: AtlasOutlineInventory?
    private var acceptedOutline: AtlasOutlinePage?
    private var lastOutlineGeneration: UInt64 = 0
    private var acceptedFind: AtlasReaderFindReport?
    private var lastFindGeneration: UInt64 = 0
    private var lastComparisonGeneration: UInt64 = 0
    private let io = AtlasProjectIO()
    private var session: AtlasReaderSession?
    private var info: AtlasReaderInfo?
    private var pendingSession: AtlasReaderSession?
    private var stagedCapture: (id: UUID, info: AtlasReaderInfo, page: AtlasReaderPage)?
    private var cancellation: AtlasSearchCancellation?
    private var operation = UUID()

    init(transport: AtlasReaderTransport, search: AtlasReaderSearchTransport? = nil,
         outline: AtlasReaderOutlineTransport? = nil,
         document: AtlasReaderDocumentTransport? = nil) {
        self.transport = transport
        self.searchTransport = search
        self.outlineTransport = outline
        self.documentTransport = document
    }
    var supportsFind: Bool { searchTransport != nil }
    var supportsOutline: Bool { outlineTransport != nil }
    var supportsDocument: Bool { documentTransport != nil }
    var canFork: Bool { session?.transport.fork != nil && info != nil }
    var supportsComparison: Bool { session?.transport.comparison != nil && info?.captureSHA256 != nil }
    var retainedRegistryDomain: UUID? { session?.transport.comparison?.domain }

    /// Open a caller-authorized retained capture into an EMPTY destination in
    /// the same native registry. The initializer runs in the existing reader
    /// worker lane; it never invokes transport.open or supplies a native grant.
    /// Its closure retains the exporting service until the foreign call drains.
    func openRetained(label: String, offset: UInt64, registryDomain: UUID,
        initialize: @escaping @Sendable (UInt64) throws -> String?) async throws -> AtlasReaderPage {
        guard label.utf8.count <= 16_384 else { throw AtlasReaderError.invalidInput }
        return try await openRetained(pathBytes: Array(label.utf8), offset: offset,
            registryDomain: registryDomain, initialize: initialize)
    }
    func openRetained(pathBytes: [UInt8], offset: UInt64, registryDomain: UUID,
        initialize: @escaping @Sendable (UInt64) throws -> String?) async throws -> AtlasReaderPage {
        try Task.checkCancellation()
        guard session == nil, pendingSession == nil, info == nil,
              transport.comparison?.domain == registryDomain,
              !pathBytes.isEmpty, pathBytes.count <= 16_384, !pathBytes.contains(0),
              offset <= AtlasReaderLimits.captureBytes else { throw AtlasReaderError.unavailable }
        let next = try AtlasReaderSession(transport: transport, accessLease: nil)
        cancel()
        pendingSession = next
        let ticket = UUID(), flag = AtlasSearchCancellation()
        operation = ticket; cancellation = flag
        do {
            let result = try await withTaskCancellationHandler(operation: {
                try await io.perform(cancellation: flag, keepingAlive: [next]) {
                    let info = try AtlasReaderInfo.decodeRetainedInitialization(
                        initialize(next.handle), handle: next.handle, pathBytes: pathBytes)
                    guard !flag.isCanceled else { throw AtlasReaderError.canceled }
                    let request = AtlasReaderRequest.window(offset: offset, bytes: AtlasReaderLimits.pageBytes)
                    try request.validate(capturedBytes: info.identity.capturedBytes)
                    let page = try AtlasReaderPage.decode(
                        next.transport.window(next.handle, offset, request.byteLimit), info: info, request: request)
                    return (info, page)
                }
            }, onCancel: { flag.cancel(); next.cancel() })
            try Task.checkCancellation()
            guard pendingSession === next, operation == ticket, !flag.isCanceled else {
                throw AtlasReaderError.canceled
            }
            session = next; info = result.0
            pendingSession = nil; cancellation = nil; operation = UUID()
            return result.1
        } catch {
            if pendingSession === next, operation == ticket {
                next.cancel(); pendingSession = nil; cancellation = nil; operation = UUID()
            }
            throw Self.readerError(error)
        }
    }

    /// Explicit transfer to another native service through the existing worker
    /// lane. The source handle and its grant stay alive until the callback has
    /// drained. A receiver with effects reconciles its outcome after cancellation;
    /// loss of delivery never implies an already committed effect was rolled back.
    func withRetainedReader<Value: Sendable>(_ work: @escaping @Sendable (UInt64) throws -> Value) async throws -> Value {
        try Task.checkCancellation()
        guard let source = session, let original = info else { throw AtlasReaderError.unavailable }
        cancel()
        let ticket = UUID(), flag = AtlasSearchCancellation()
        operation = ticket; cancellation = flag
        do {
            let value = try await withTaskCancellationHandler(operation: {
                try await io.perform(cancellation: flag, keepingAlive: [source]) { try work(source.handle) }
            }, onCancel: { flag.cancel(); source.cancel() })
            try Task.checkCancellation()
            guard session === source, operation == ticket, !flag.isCanceled,
                  info?.identity.matches(original.identity) == true else { throw AtlasReaderError.canceled }
            cancellation = nil
            return value
        } catch {
            if operation == ticket { cancellation = nil }
            throw Self.readerError(error)
        }
    }

    /// Both retained handles are transferred explicitly to a single compatible
    /// registry. Analysis runs off the main actor and keeps BOTH source/grant
    /// lifetimes through return. Replacement, closure or cancellation of either
    /// reader rejects delivery; ordinary query/outline state is not transferred.
    func compare(with other: AtlasReaderCoordinator,
                 limits: AtlasReaderComparisonLimits = .init()) async throws -> AtlasReaderComparisonReport {
        try Task.checkCancellation()
        try limits.validate()
        guard self !== other, let before = session, let after = other.session,
              let beforeInfo = info, let afterInfo = other.info,
              beforeInfo.captureSHA256 != nil, afterInfo.captureSHA256 != nil,
              let comparison = before.transport.comparison,
              comparison.domain == after.transport.comparison?.domain,
              beforeInfo.identity.owner != afterInfo.identity.owner,
              lastComparisonGeneration < UInt64.max else { throw AtlasReaderError.unavailable }
        lastComparisonGeneration += 1
        let generation = lastComparisonGeneration
        cancel()
        let ticket = UUID(), flag = AtlasSearchCancellation()
        operation = ticket; cancellation = flag
        do {
            let result = try await withTaskCancellationHandler(operation: {
                try await io.perform(cancellation: flag, keepingAlive: [before, after]) {
                    try AtlasReaderComparisonReport.decode(comparison.compare(before.handle, after.handle,
                        generation, limits.editDistance, limits.work), before: beforeInfo, after: afterInfo,
                        generation: generation, limits: limits)
                }
            }, onCancel: { flag.cancel(); before.cancel(); after.cancel() })
            try Task.checkCancellation()
            guard session === before, other.session === after, operation == ticket, !flag.isCanceled,
                  info?.identity.matches(beforeInfo.identity) == true,
                  other.info?.identity.matches(afterInfo.identity) == true else { throw AtlasReaderError.canceled }
            cancellation = nil
            return result
        } catch {
            if operation == ticket { cancellation = nil }
            throw Self.readerError(error)
        }
    }

    /// Initialize this empty coordinator from another reader's exact capture.
    /// Only the new reader's queue and cancellation state are touched. Foreign
    /// work retains both sessions and their native access lease through drain.
    /// Source replacement/close during the fork revokes delivery, even when a
    /// canceled foreign operation managed to initialize its destination.
    func fork(from parent: AtlasReaderCoordinator, page original: AtlasReaderPage) async throws -> AtlasReaderPage {
        try Task.checkCancellation()
        guard session == nil, pendingSession == nil, info == nil,
              let source = parent.session, let sourceInfo = parent.info,
              sourceInfo.identity.matches(original.identity), let fork = source.transport.fork,
              original.start <= original.end, original.end <= sourceInfo.identity.capturedBytes,
              AtlasReaderWire.validHex(original.originalHex, bytes: original.end - original.start) else {
            throw AtlasReaderError.unavailable
        }
        let request = AtlasReaderRequest.window(offset: original.start,
            bytes: max(4, min(AtlasReaderLimits.maximumPageBytes, original.end - original.start)))
        try request.validate(capturedBytes: sourceInfo.identity.capturedBytes)
        let next = try AtlasReaderSession(transport: transport, sharingAccessFrom: source)
        pendingSession = next
        let ticket = UUID()
        operation = ticket
        let flag = AtlasSearchCancellation()
        cancellation = flag
        do {
            let result = try await io.perform(cancellation: flag, keepingAlive: [source, next]) {
                let copiedInfo = try AtlasReaderInfo.decode(fork(source.handle, next.handle),
                    handle: next.handle, pathBytes: sourceInfo.identity.pathBytes)
                guard !flag.isCanceled,
                      copiedInfo.identity.owner != sourceInfo.identity.owner,
                      copiedInfo.identity.capturedBytes == sourceInfo.identity.capturedBytes,
                      copiedInfo.identity.encoding == sourceInfo.identity.encoding,
                      copiedInfo.identity.pathBytes == sourceInfo.identity.pathBytes,
                      sourceInfo.captureSHA256 == nil || copiedInfo.captureSHA256 == sourceInfo.captureSHA256,
                      copiedInfo.origin == "host-supplied" else { throw AtlasReaderError.invalidResponse }
                let copiedPage = try AtlasReaderPage.decode(
                    next.transport.window(next.handle, original.start, request.byteLimit),
                    info: copiedInfo, request: request)
                // The independent identity cannot authorize replacement bytes
                // for a page the user already saw. Compare the shared original
                // extent, never its decoded or visually selected text.
                guard copiedPage.start == original.start else { throw AtlasReaderError.invalidResponse }
                let overlap = Int(min(copiedPage.end, original.end) - original.start) * 2
                guard copiedPage.originalHex.utf8.prefix(overlap)
                    .elementsEqual(original.originalHex.utf8.prefix(overlap)) else {
                    throw AtlasReaderError.invalidResponse
                }
                return (copiedInfo, copiedPage)
            }
            try Task.checkCancellation()
            guard pendingSession === next, operation == ticket, !flag.isCanceled,
                  parent.session === source, parent.info?.identity.matches(sourceInfo.identity) == true else {
                throw AtlasReaderError.canceled
            }
            session = next
            info = result.0
            pendingSession = nil
            cancellation = nil
            operation = UUID()
            return result.1
        } catch {
            if pendingSession === next, operation == ticket {
                next.cancel()
                pendingSession = nil
                cancellation = nil
                operation = UUID()
            }
            throw Self.readerError(error)
        }
    }

    deinit {
        cancellation?.cancel()
        session?.cancel()
        pendingSession?.cancel()
    }

    /// Convenience for callers with no separate presentation transaction. An
    /// unsuccessful replacement preserves the previous accepted reader.
    func open(input: AtlasReaderInput, accessLease: AnyObject? = nil) async throws -> AtlasReaderPage {
        let replacement = try await stageOpen(input: input, accessLease: accessLease)
        do {
            try Task.checkCancellation()
            return try commitOpen(replacement)
        } catch {
            if operation == replacement.id { cancel() }
            throw Self.readerError(error)
        }
    }

    /// Capture and validate the first page without replacing the accepted
    /// source, query, outline or document. A native model checks its own task
    /// ticket after awaiting this, then commits and installs without suspending.
    /// An explicit live refresh can supply the original live transport instead
    /// of a search-capture-only adapter; this never happens as a fallback.
    func stageOpen(input: AtlasReaderInput, accessLease: AnyObject? = nil,
                   using replacementTransport: AtlasReaderTransport? = nil) async throws -> AtlasReaderReplacement {
        try Task.checkCancellation()
        // Empty-handle admission performs no filesystem work. A refused new
        // handle must not interrupt an already admitted operation or capture.
        let next = try AtlasReaderSession(transport: replacementTransport ?? transport, accessLease: accessLease)
        cancel()
        pendingSession = next
        let ticket = UUID()
        operation = ticket
        let flag = AtlasSearchCancellation()
        cancellation = flag
        do {
            let result = try await io.perform(cancellation: flag, keepingAlive: [next]) {
                let info = try AtlasReaderInfo.decode(
                    next.transport.open(next.handle, input.fullPath, AtlasReaderLimits.captureBytes),
                    handle: next.handle, path: input.fullPath)
                guard !flag.isCanceled else { throw AtlasReaderError.canceled }
                let request = AtlasReaderRequest.firstPage
                let page = try AtlasReaderPage.decode(
                    next.transport.window(next.handle, 0, request.byteLimit), info: info, request: request)
                return (info, page)
            }
            guard pendingSession === next, operation == ticket, !flag.isCanceled,
                  !Task.isCancelled else { throw AtlasReaderError.canceled }
            stagedCapture = (ticket, result.0, result.1)
            cancellation = nil
            return AtlasReaderReplacement(id: ticket)
        } catch {
            // A late failed attempt cannot discard a newer proposal or the
            // accepted capture. The worker's lease controls actual retirement.
            if pendingSession === next, operation == ticket {
                next.cancel()
                pendingSession = nil
                stagedCapture = nil
                cancellation = nil
                operation = UUID()
            }
            throw Self.readerError(error)
        }
    }

    /// No await and no host callbacks: the caller can install the returned page
    /// in the same main-actor turn. A proposal contains no publishable source,
    /// and replay or delivery to another coordinator cannot replace its reader.
    func commitOpen(_ replacement: AtlasReaderReplacement) throws -> AtlasReaderPage {
        guard let staged = stagedCapture, let next = pendingSession,
              staged.id == replacement.id, operation == replacement.id else {
            throw AtlasReaderError.canceled
        }
        session = next
        info = staged.info
        pendingSession = nil
        stagedCapture = nil
        cancellation = nil
        acceptedFind = nil
        resetOutline()
        documentState.close()
        // Query targets can escape the current capture. Keep their generation
        // counters monotonic for this coordinator instead of recycling one.
        operation = UUID()
        return staged.page
    }

    func read(_ request: AtlasReaderRequest) async throws -> AtlasReaderPage {
        guard let session, let info else { throw AtlasReaderError.unavailable }
        // Reject invalid navigation before canceling an admitted operation.
        try request.validate(capturedBytes: info.identity.capturedBytes)
        try validateSelection(request)
        let outline = outlineTransport
        let search = searchTransport
        cancel()
        let ticket = UUID()
        operation = ticket
        let flag = AtlasSearchCancellation()
        cancellation = flag
        do {
            let page = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                let json: String?
                switch request {
                case .window(let offset, let bytes):
                    json = session.transport.window(session.handle, offset, bytes)
                case .lines(let first, let count, let bytes):
                    json = session.transport.lines(session.handle, first, count, bytes)
                case .hit(let target):
                    guard let search else { throw AtlasReaderError.unavailable }
                    json = search.hit(session.handle, target.generation, target.index, AtlasReaderHitTarget.contextBytes)
                case .symbol(let target):
                    guard let outline else { throw AtlasReaderError.unavailable }
                    json = outline.symbol(session.handle, target.generation, target.symbol.id, AtlasOutlineTarget.contextBytes)
                }
                return try AtlasReaderPage.decode(json, info: info, request: request)
            }
            guard self.session === session, operation == ticket, !flag.isCanceled else { throw AtlasReaderError.canceled }
            // A host may revoke rows while foreign work is draining without
            // canceling unrelated source work. Recheck authority at delivery.
            try validateSelection(request)
            cancellation = nil
            return page
        } catch {
            if self.session === session, operation == ticket { cancellation = nil }
            throw Self.readerError(error)
        }
    }

    private func validateSelection(_ request: AtlasReaderRequest) throws {
        if case .hit(let target) = request {
            guard searchTransport != nil, let acceptedFind,
                  acceptedFind.target(at: Int(target.index)) == target else { throw AtlasReaderError.invalidInput }
        }
        if case .symbol(let target) = request {
            guard outlineTransport != nil, let acceptedOutline,
                  acceptedOutline.target(id: target.symbol.id) == target else {
                throw AtlasReaderError.invalidInput
            }
        }
    }

    /// Search the retained source, not the current disk file or only the visible
    /// page. Attempts consume generations even when canceled or refused later.
    func find(_ needle: String) async throws -> AtlasReaderFindReport {
        guard let session, let info, let search = searchTransport else { throw AtlasReaderError.unavailable }
        try AtlasReaderFindReport.validateNeedle(needle)
        let (generation, overflow) = lastFindGeneration.addingReportingOverflow(1)
        guard !overflow else { throw AtlasReaderError.unavailable }
        lastFindGeneration = generation
        acceptedFind = nil
        cancel()
        let ticket = UUID()
        operation = ticket
        let flag = AtlasSearchCancellation()
        cancellation = flag
        do {
            let report = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                try AtlasReaderFindReport.decode(
                    search.find(session.handle, generation, needle, AtlasReaderFindReport.maxHits, info.identity.capturedBytes),
                    info: info, generation: generation, needle: needle)
            }
            guard self.session === session, operation == ticket, !flag.isCanceled else { throw AtlasReaderError.canceled }
            acceptedFind = report
            cancellation = nil
            return report
        } catch {
            if self.session === session, operation == ticket { cancellation = nil }
            throw Self.readerError(error)
        }
    }

    /// Build the shared extractor's bounded inventory over this same capture.
    /// Search and outline use independent counters, even when both equal one.
    func prepareOutline(language: AtlasOutlineLanguage = .automatic) async throws -> AtlasOutlinePage {
        guard let session, let info, let outline = outlineTransport else { throw AtlasReaderError.unavailable }
        let (generation, overflow) = lastOutlineGeneration.addingReportingOverflow(1)
        guard !overflow else { throw AtlasReaderError.unavailable }
        lastOutlineGeneration = generation
        resetOutline()
        cancel()
        let ticket = UUID()
        operation = ticket
        let flag = AtlasSearchCancellation()
        cancellation = flag
        do {
            let page = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                try AtlasOutlinePage.decode(
                    outline.prepare(session.handle, generation, language.engineName, AtlasOutlinePage.maxItems),
                    info: info, generation: generation, request: .initial, preparing: true, language: language)
            }
            guard self.session === session, operation == ticket, !flag.isCanceled else { throw AtlasReaderError.canceled }
            outlineInventory = page.inventory
            acceptedOutline = page
            cancellation = nil
            return page
        } catch {
            if self.session === session, operation == ticket { cancellation = nil }
            throw Self.readerError(error)
        }
    }

    /// Page/filter already-retained symbols. No additional source capture or
    /// extraction. Failed work cannot authorize rows for the attempted filter.
    func outlineSymbols(_ request: AtlasOutlinePageRequest) async throws -> AtlasOutlinePage {
        try request.validate()
        guard let session, let info, let outline = outlineTransport,
              let inventory = outlineInventory else { throw AtlasReaderError.unavailable }
        acceptedOutline = nil
        cancel()
        let ticket = UUID()
        operation = ticket
        let flag = AtlasSearchCancellation()
        cancellation = flag
        do {
            let page = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                try AtlasOutlinePage.decode(
                    outline.symbols(session.handle, inventory.generation, request.needle,
                        request.mode.code, request.start, request.limit),
                    info: info, generation: inventory.generation, request: request, inventory: inventory)
            }
            guard self.session === session, operation == ticket, !flag.isCanceled,
                  outlineInventory?.matches(inventory) == true else { throw AtlasReaderError.canceled }
            acceptedOutline = page
            cancellation = nil
            return page
        } catch {
            if self.session === session, operation == ticket { cancellation = nil }
            throw Self.readerError(error)
        }
    }

    /// Document parsing, paging, provenance and copy share the retained source
    /// handle and its bounded I/O lane. The optional capability stays inert for
    /// source-only hosts. Admission precedes cancellation so invalid requests
    /// cannot interrupt valid work or discard the currently accepted document.
    func document(_ request: AtlasReaderDocumentRequest) async throws -> AtlasReaderDocumentResult {
        guard let session, let info, let document = documentTransport else {
            throw AtlasReaderError.unavailable
        }
        let job = try documentState.admit(request, info: info)
        cancel()
        activeDocumentJob = job
        let ticket = UUID()
        operation = ticket
        let flag = AtlasSearchCancellation()
        cancellation = flag
        do {
            let result = try await io.perform(cancellation: flag, keepingAlive: [session]) {
                try job.execute(document)
            }
            guard self.session === session, operation == ticket, !flag.isCanceled else {
                throw AtlasReaderError.canceled
            }
            // A reflow, replacement, or independently revoked page cannot lend
            // its authority to a delayed source/copy/heading response.
            try documentState.accept(result, job: job)
            activeDocumentJob = nil
            cancellation = nil
            return result
        } catch {
            documentState.abandon(job)
            if self.session === session, operation == ticket {
                activeDocumentJob = nil
                cancellation = nil
            }
            throw Self.readerError(error)
        }
    }

    /// Retire preview authority synchronously before scheduling a reflow. This
    /// never resets the document generation or borrows search/outline authority.
    func revokeDocument() {
        if activeDocumentJob != nil { cancel() }
        documentState.revoke()
    }

    /// Revoke native rows, not source/search state or the bounded Rust index.
    func revokeOutlinePage() { acceptedOutline = nil }
    func resetOutline() { acceptedOutline = nil; outlineInventory = nil }

    /// Revoke native activation authority without evicting captured source or
    /// interrupting unrelated byte/line navigation. The host cancels active
    /// query work separately when editing its query context.
    func clearFind() { acceptedFind = nil }

    func cancel() {
        if let job = activeDocumentJob { documentState.abandon(job) }
        activeDocumentJob = nil
        operation = UUID()
        cancellation?.cancel()
        cancellation = nil
        if let pendingSession { pendingSession.cancel() }
        else { session?.cancel() }
        pendingSession = nil
        stagedCapture = nil
        io.cancel()
    }

    func close() {
        cancel()
        info = nil
        acceptedFind = nil
        resetOutline()
        // Counters belong to this coordinator, not a capture. Old query
        // targets must not become current after close/open of identical bytes.
        documentState.close()
        session = nil
    }

    private static func readerError(_ error: Error) -> AtlasReaderError {
        if let error = error as? AtlasReaderError { return error }
        if error is CancellationError { return .canceled }
        if let error = error as? AtlasProjectIOError, case .canceled = error { return .canceled }
        return .unavailable
    }
}
