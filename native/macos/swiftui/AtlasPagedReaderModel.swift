import Foundation
import Observation

/// Presentation state for one independently opened retained capture. It never
/// treats workspace coordinates as native positions or promotes a page into
/// atlas capture evidence. Only committed navigation changes the current page/history.
@Observable @MainActor final class AtlasPagedReaderModel {
    let documentPreview: AtlasDocumentPreviewModel
    private(set) var page: AtlasReaderPage?
    /// Presentation-only UTF-8 of this logical window, not original file bytes.
    /// Never inserted into the atlas/source capture tables or searched as a file.
    private(set) var source: AtlasSource?
    private(set) var navigation = UUID()
    private(set) var busy = false
    private(set) var isRefreshing = false
    private(set) var refreshedFromDisk = false
    private(set) var pinning = false
    private(set) var notice = ""
    var offsetInput = "0"
    var lineInput = "1" {
        didSet { if positionWork && !oldValue.utf8.elementsEqual(lineInput.utf8) { cancel() } }
    }
    var columnInput = "1" {
        didSet { if positionWork && !oldValue.utf8.elementsEqual(columnInput.utf8) { cancel() } }
    }
    var columnKind: AtlasReaderColumnKind = .unicodeScalar {
        didSet { if positionWork && oldValue != columnKind { cancel() } }
    }
    var fileQuery = "" {
        didSet {
            // Revoke synchronously with the edit, not in a later SwiftUI
            // onChange that could cancel a newly submitted Return/Find action.
            if !oldValue.utf8.elementsEqual(fileQuery.utf8) { clearFileSearch() }
        }
    }
    private(set) var findReport: AtlasReaderFindReport?
    private(set) var selectedHitIndex: Int?
    private var selectedRange: NSRange?
    @ObservationIgnored private var queryWork = false
    @ObservationIgnored private var outlineWork = false
    @ObservationIgnored private var referenceWork = false
    @ObservationIgnored private var positionWork = false
    private(set) var outlinePage: AtlasOutlinePage?
    private(set) var hasOutline = false
    private(set) var outlineRowsAuthorized = false
    private var outlineHistory: [AtlasOutlinePageRequest] = []
    var outlineNeedle = "" {
        didSet { if !oldValue.utf8.elementsEqual(outlineNeedle.utf8) { clearOutlineFilter() } }
    }
    var outlineMode: AtlasOutlineNameMode = .contains {
        didSet { if oldValue != outlineMode { clearOutlineFilter() } }
    }
    var outlineLanguage: AtlasOutlineLanguage = .automatic {
        didSet { if oldValue != outlineLanguage { clearOutlineFilter(reset: true) } }
    }
    var referenceName = "" {
        didSet {
            if !oldValue.utf8.elementsEqual(referenceName.utf8) { clearReferenceSearch() }
        }
    }
    private(set) var referencePage: AtlasReferencePage?
    private(set) var referenceRowsAuthorized = false
    private var referenceHistory: [AtlasReferencePageRequest] = []
    private typealias SourceHistory = AtlasNavigationHistory<AtlasReaderRequest>
    private var history = SourceHistory()
    @ObservationIgnored private let capturedTarget: AtlasSearchCapturedHit?
    @ObservationIgnored private let suppliedSource: AtlasSuppliedReaderSource?
    @ObservationIgnored private let liveTransport: AtlasReaderTransport
    @ObservationIgnored private let searchTransport: AtlasReaderSearchTransport?
    @ObservationIgnored private let outlineTransport: AtlasReaderOutlineTransport?
    @ObservationIgnored private let documentTransport: AtlasReaderDocumentTransport?
    @ObservationIgnored private var refreshInput: AtlasReaderInput?
    @ObservationIgnored private var refreshAccessLease: AnyObject?
    @ObservationIgnored private let coordinator: AtlasReaderCoordinator
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var operation = UUID()
    @ObservationIgnored private var pinOperation: UUID?
    @ObservationIgnored private var pendingPin: AtlasPagedReaderModel?
    private var forkedCapture = false
    private var retainedOnly = false

    init(transport: AtlasReaderTransport, search: AtlasReaderSearchTransport? = nil,
         outline: AtlasReaderOutlineTransport? = nil,
         document: AtlasReaderDocumentTransport? = nil,
         captured: AtlasSearchCapturedHit? = nil,
         supplied: AtlasSuppliedReaderSource? = nil) {
        capturedTarget = captured
        suppliedSource = supplied
        liveTransport = transport
        searchTransport = search
        outlineTransport = outline
        documentTransport = document
        let initialTransport = captured.map { transport.importing($0) }
            ?? supplied.map { transport.importing($0) } ?? transport
        let coordinator = AtlasReaderCoordinator(transport: initialTransport, search: search,
            outline: outline, document: document)
        self.coordinator = coordinator
        documentPreview = AtlasDocumentPreviewModel(supported: document != nil,
            execute: { try await coordinator.document($0) },
            cancel: { coordinator.cancel() }, revoke: { coordinator.revokeDocument() })
    }
    deinit { task?.cancel() }

    var canGoBack: Bool { history.canBack && !busy }
    var canGoForward: Bool { history.canForward && !busy }
    var canGoNext: Bool { page?.nextOffset != nil && !busy }
    var canRefresh: Bool { page != nil && refreshInput != nil && !busy && !documentPreview.busy }
    var canPin: Bool { page != nil && !busy && !pinning && !documentPreview.busy && coordinator.canFork }
    var canKeep: Bool {
        guard canPin, let page else { return false }
        return page.identity.capturedBytes <= AtlasReaderLimits.transferBytes
    }
    var canCompare: Bool { page != nil && !busy && !pinning && !documentPreview.busy && coordinator.supportsComparison }
    var retainedRegistryDomain: UUID? { coordinator.retainedRegistryDomain }
    var retainedCaptureSHA256: String? { coordinator.retainedCaptureSHA256 }
    var canFind: Bool { page != nil && coordinator.supportsFind }
    var canGoToPosition: Bool { page != nil && coordinator.supportsPosition }
    var findIsCurrent: Bool { findReport?.needle.utf8.elementsEqual(fileQuery.utf8) == true }
    var canMoveHit: Bool { findIsCurrent && findReport?.hits.isEmpty == false && !busy }
    var nativeSelectionRange: NSRange? {
        if let position = page?.position { return position.nativeCaretRange }
        if findIsCurrent, page?.selection?.target.generation == findReport?.generation,
           page?.selection != nil { return selectedRange }
        if outlineIsCurrent, let selection = page?.symbolSelection,
           selection.target.pageID == outlinePage?.id { return selectedRange }
        if referencesAreCurrent, let selection = page?.referenceSelection,
           selection.target.pageID == referencePage?.id { return selectedRange }
        return nil
    }
    var matchHex: String? {
        guard findIsCurrent, let selection = page?.selection,
              selection.target.generation == findReport?.generation else { return nil }
        return selection.target.originalHex
    }

    var canBuildOutline: Bool { page != nil && coordinator.supportsOutline }
    var outlineIsCurrent: Bool {
        guard outlineRowsAuthorized, let outlinePage else { return false }
        return outlinePage.request.needle.utf8.elementsEqual(outlineNeedle.utf8)
            && (outlineNeedle.isEmpty || outlinePage.request.mode == outlineMode)
    }
    var canOutlineBack: Bool { !outlineHistory.isEmpty && !busy }
    var symbolHex: String? {
        guard outlineIsCurrent, let selection = page?.symbolSelection,
              selection.target.pageID == outlinePage?.id else { return nil }
        return selection.originalHex
    }
    var symbolSelectionLabel: String? {
        guard symbolHex != nil, let symbol = page?.symbolSelection?.target.symbol else { return nil }
        return symbol.nameRange == nil ? "Declaration evidence (no exact name span)" : "Exact identifier span"
    }
    var canFindReferences: Bool {
        guard let page else { return false }
        return coordinator.supportsReferences && page.identity.capturedBytes <= AtlasReferencePage.sourceLimit
    }
    var referencesAreCurrent: Bool {
        guard referenceRowsAuthorized, let referencePage else { return false }
        return referencePage.inventory.name.utf8.elementsEqual(referenceName.utf8)
            && page?.identity.matches(referencePage.inventory.identity) == true
    }
    var canReferenceBack: Bool { referencesAreCurrent && !referenceHistory.isEmpty && !busy }
    var referenceHex: String? {
        guard referencesAreCurrent, let selection = page?.referenceSelection,
              selection.target.pageID == referencePage?.id else { return nil }
        return selection.originalHex
    }
    var referenceSelectionLabel: String? {
        guard referenceHex != nil, let target = page?.referenceSelection?.target else { return nil }
        return "Identifier occurrence \(target.occurrence.id) · LF line \(target.occurrence.line)"
    }

    var coverage: String {
        guard let page else { return "Retained capture limit: \(AtlasReaderLimits.captureBytes / 1024 / 1024) MiB. Oversized sources are refused, not truncated." }
        var messages = ["Logical page; layout is local to this window."]
        if page.identity.capturedBytes > AtlasReaderLimits.transferBytes {
            messages.append("Comparison and reading-desk transfer are limited to 4 MiB per source. Paging, Find and pinning use this retained capture.")
        }
        if let highlight = page.highlight { messages.append(highlight.summary) }
        if refreshedFromDisk {
            messages.append("Explicitly refreshed disk capture; older search, outline and preview targets were retired. File observation is not an atomic filesystem snapshot.")
        } else if forkedCapture {
            messages.append("Pinned exact retained bytes; no live-file reread. Search, outline, preview and navigation belong to this reader independently.")
        } else if capturedTarget != nil {
            messages.append("Source imported from the retained workspace search; no live-file reread.")
        } else if suppliedSource != nil {
            messages.append("Source imported from the complete text already displayed; no live-file reread. Refresh from disk explicitly opens current bytes.")
        } else if retainedOnly {
            messages.append("Exact retained source from the reading desk. Its saved label grants no disk access; navigation, Find, Outline and Markdown use this independent capture.")
        }
        if let label = symbolSelectionLabel { messages.append("\(label); heuristic candidate, not semantic resolution.") }
        if referenceHex != nil { messages.append("Whole-token text occurrence; comments and literals are included. No compiler reference resolution is claimed.") }
        if matchHex != nil { messages.append("Selection belongs to this retained file query, not a workspace search capture.") }
        if page.rangeLimited { messages.append("Requested lines exceed the page byte limit. Next continues through the retained bytes.") }
        if page.boundariesAdjusted { messages.append("Page boundaries adjusted to complete source scalars or CRLF.") }
        if page.hasReplacements { messages.append("Display contains replacement characters. Original hex preserves the captured bytes.") }
        if let limitation = page.position?.caretLimitation { messages.append(limitation.message) }
        return messages.joined(separator: " ")
    }

    /// Pin the accepted source window into an independently owned reader. This
    /// does not navigate, cancel a query, or replace any state in the parent.
    /// The returned model is ready to display: its host must not call open()
    /// again. No parent hit/outline/document IDs cross to the fresh reader.
    func fork() async throws -> AtlasPagedReaderModel {
        try Task.checkCancellation()
        guard canPin, let original = page else { throw AtlasReaderError.unavailable }
        let ticket = UUID()
        let sourceNavigation = navigation
        let allowedRefresh = refreshInput
        let accessLease = refreshAccessLease
        let child = AtlasPagedReaderModel(transport: liveTransport, search: searchTransport,
            outline: outlineTransport, document: documentTransport)
        pinOperation = ticket
        pendingPin = child
        pinning = true
        defer {
            if pinOperation == ticket {
                pendingPin = nil
                pinOperation = nil
                pinning = false
            }
        }
        do {
            let copied = try await child.coordinator.fork(from: coordinator, page: original)
            try Task.checkCancellation()
            guard pinOperation == ticket, navigation == sourceNavigation,
                  page?.identity.matches(original.identity) == true else { throw AtlasReaderError.canceled }
            child.refreshInput = allowedRefresh
            child.refreshAccessLease = accessLease
            child.forkedCapture = true
            child.retainedOnly = retainedOnly
            child.install(copied, request: copied.position?.request ?? .window(offset: copied.start,
                bytes: max(4, min(AtlasReaderLimits.maximumPageBytes, copied.end - copied.start))))
            child.notice = copied.position == nil
                ? "Pinned the exact captured source at this byte window. Find, Outline and Markdown use independent selections."
                : "Pinned the exact captured source at this logical position. Find, Outline and Markdown use independent selections."
            return child
        } catch {
            child.close()
            if error is CancellationError { throw AtlasReaderError.canceled }
            throw error
        }
    }

    func open(root: String, path: String, accessLease: AnyObject?) {
        close()
        let input: AtlasReaderInput
        do {
            guard capturedTarget == nil || suppliedSource == nil else { throw AtlasReaderError.invalidInput }
            let liveInput = try AtlasReaderInput(root: root, path: path)
            if let target = capturedTarget {
                guard target.root.utf8.elementsEqual(root.utf8), target.path.utf8.elementsEqual(path.utf8),
                      !target.mode.isTextual || coordinator.supportsFind else { throw AtlasReaderError.invalidInput }
                input = try AtlasReaderInput(captured: target)
                fileQuery = target.mode.isTextual ? target.needle : ""
            } else if let supplied = suppliedSource {
                guard supplied.matches(root: root, path: path) else { throw AtlasReaderError.invalidInput }
                guard liveTransport.openBytes != nil else { throw AtlasReaderError.unavailable }
                input = supplied.input
            } else {
                input = liveInput
            }
            // Remember only the explicitly opened project/path and its current
            // lease. Captured metadata alone never grants a refresh destination.
            refreshInput = liveInput
            refreshAccessLease = accessLease
        } catch let error as AtlasReaderError { notice = error.message; return }
        catch { notice = AtlasReaderError.invalidInput.message; return }
        let ticket = begin(query: capturedTarget != nil)
        notice = capturedTarget != nil ? "Opening the exact retained search capture…"
            : suppliedSource != nil ? "Opening the complete source already displayed…"
            : "Capturing source for bounded page navigation…"
        task = Task {
            do {
                let opened = try await coordinator.open(input: input, accessLease: accessLease)
                guard operation == ticket, !Task.isCancelled else { return }
                if let target = capturedTarget {
                    if target.mode.isTextual {
                        // Literal hits and expression anchor terms receive
                        // this reader's own exact-spelling query IDs.
                        let selected = try await AtlasCapturedReader.activate(target, identity: opened.identity,
                            find: { try await coordinator.find($0) },
                            read: { try await coordinator.read(.hit($0)) })
                        guard operation == ticket, !Task.isCancelled else { return }
                        findReport = selected.report
                        selectedHitIndex = selected.index
                        install(selected.page, request: .hit(selected.target))
                    } else {
                        let request = try AtlasCapturedReader.structuralRequest(target, identity: opened.identity)
                        let selected = try await coordinator.read(request)
                        guard operation == ticket, !Task.isCancelled else { return }
                        install(selected, request: request)
                    }
                    finish(ticket)
                    notice = target.mode.isTextual ? "Opened the exact search capture without rereading the live file."
                        : "Opened retained \(target.mode == .headings ? "heading" : "declaration") evidence at original bytes \(target.start)–\(target.end). No live-file reread."
                } else {
                    install(opened, request: .firstPage)
                    finish(ticket)
                    if suppliedSource != nil {
                        notice = "Opened the exact displayed source without rereading the live file."
                    }
                }
            } catch {
                // Failed initial activation has no publishable page. Retire
                // its independently imported reader rather than retaining a
                // hidden capture or falling back to current disk contents.
                if operation == ticket, page == nil {
                    coordinator.close()
                    refreshInput = nil; refreshAccessLease = nil
                }
                fail(error, ticket: ticket)
            }
        }
    }

    /// Initialize a new model from retained service data without creating a
    /// live-path refresh capability. The host owns the ready model on success;
    /// ordinary view appearance must not call open(root:path:) afterward.
    func openRetained(label: String, offset: UInt64, registryDomain: UUID,
        initialize: @escaping @Sendable (UInt64) throws -> String?) async throws {
        guard label.utf8.count <= 16_384 else { throw AtlasReaderError.invalidInput }
        try await openRetained(pathBytes: Array(label.utf8), offset: offset,
            registryDomain: registryDomain, initialize: initialize)
    }
    func openRetained(pathBytes: [UInt8], offset: UInt64, registryDomain: UUID,
        initialize: @escaping @Sendable (UInt64) throws -> String?) async throws {
        guard !pathBytes.isEmpty, pathBytes.count <= 16_384, !pathBytes.contains(0) else {
            throw AtlasReaderError.invalidInput
        }
        try await openRetained(offset: offset, registryDomain: registryDomain) { handle in
            try AtlasReaderInfo.decodeRetainedInitialization(initialize(handle), handle: handle, pathBytes: pathBytes)
        }
    }
    /// Adopt source identity returned by a separately authorized shared service.
    /// This permits logical link resolution without opening a caller-made path.
    func openRetained(offset: UInt64, registryDomain: UUID,
        initialize: @escaping @Sendable (UInt64) throws -> AtlasReaderInfo) async throws {
        try Task.checkCancellation()
        guard offset <= AtlasReaderLimits.captureBytes else { throw AtlasReaderError.unavailable }
        try await openRetained(registryDomain: registryDomain) { handle in
            AtlasReaderRetainedInitialization(info: try initialize(handle), initialOriginalOffset: offset)
        }
    }
    func openRetained(registryDomain: UUID,
        initialize: @escaping @Sendable (UInt64) throws -> AtlasReaderRetainedInitialization) async throws {
        guard page == nil, !busy, capturedTarget == nil, suppliedSource == nil else {
            throw AtlasReaderError.unavailable
        }
        let ticket = begin()
        notice = "Opening the exact retained capture…"
        do {
            let opened = try await coordinator.openRetained(registryDomain: registryDomain, initialize: initialize)
            try Task.checkCancellation()
            guard operation == ticket else { throw AtlasReaderError.canceled }
            retainedOnly = true
            install(opened.page, request: opened.request)
            finish(ticket)
            notice = "Opened the exact retained source at its saved byte location. Keep a page in the desk explicitly to update its saved position."
        } catch {
            if operation == ticket {
                coordinator.close()
                fail(error, ticket: ticket)
            }
            throw error
        }
    }

    /// Explicit user intent to observe the current disk file. Keep the accepted
    /// page, query, outline, preview and history until a candidate's first page
    /// validates. Changed bytes begin a fresh history rather than silently
    /// resolving any old exact anchor against a different capture.
    func refreshFromDisk() {
        guard canRefresh, let input = refreshInput else { return }
        let accessLease = refreshAccessLease
        let ticket = begin()
        isRefreshing = true
        notice = "Capturing current disk bytes… The retained source remains unchanged until refresh succeeds."
        task = Task {
            do {
                let replacement = try await coordinator.stageOpen(input: input,
                    accessLease: accessLease, using: liveTransport)
                guard operation == ticket, !Task.isCancelled else { return }
                // No await between session commit and presentation install.
                // Source navigation, cancellation and close all revoke both
                // operation tickets before a suspended refresh can return.
                let next = try coordinator.commitOpen(replacement)
                clearFileSearch()
                clearOutlineFilter(reset: true)
                clearReferenceSearch()
                documentPreview.close()
                lineInput = "1"
                columnInput = "1"
                install(next, request: .firstPage)
                refreshedFromDisk = true
                finish(ticket)
                notice = "Refreshed from disk at the first page. Previous exact targets and navigation history were cleared; rerun Find, Outline or Markdown for this capture."
            } catch {
                guard operation == ticket, !Task.isCancelled else { return }
                fail(error, ticket: ticket)
                notice = "Refresh failed. The previous retained source is unchanged. " + notice
            }
        }
    }

    func next() {
        guard let offset = page?.nextOffset else { return }
        navigate(.window(offset: offset, bytes: AtlasReaderLimits.pageBytes))
    }
    func back() {
        guard canGoBack, let move = history.back() else { return }
        navigate(move.target, historyMove: move)
    }
    func forward() {
        guard canGoForward, let move = history.forward() else { return }
        navigate(move.target, historyMove: move)
    }
    func goToByte() {
        guard let offset = Self.number(offsetInput) else { notice = AtlasReaderError.invalidInput.message; return }
        navigate(.window(offset: offset, bytes: AtlasReaderLimits.pageBytes))
    }
    func goToLine() {
        guard let line = Self.number(lineInput), line > 0 else { notice = AtlasReaderError.invalidInput.message; return }
        navigate(.lines(first: line, count: 64, bytes: AtlasReaderLimits.pageBytes))
    }
    func goToPosition() {
        guard canGoToPosition else { notice = AtlasReaderError.unavailable.message; return }
        guard let line = Self.number(lineInput), line > 0,
              let column = Self.number(columnInput), column > 0 else {
            notice = AtlasReaderError.invalidInput.message
            return
        }
        navigate(.position(line: line, column: column, kind: columnKind, bytes: AtlasReaderLimits.pageBytes))
    }

    /// Keep the accepted source/grant alive while another explicit native
    /// service imports it. The receiving service owns any committed effects;
    /// this helper only qualifies which exact retained source was supplied.
    func withRetainedReader<Value: Sendable>(_ work: @escaping @Sendable (UInt64) throws -> Value) async throws -> Value {
        guard !busy, !pinning, !documentPreview.busy, let original = page else { throw AtlasReaderError.unavailable }
        let result = try await coordinator.withRetainedReader(work)
        try Task.checkCancellation()
        guard page?.identity.matches(original.identity) == true else { throw AtlasReaderError.canceled }
        return result
    }

    func compare(with other: AtlasPagedReaderModel,
                 limits: AtlasReaderComparisonLimits = .init()) async throws -> AtlasReaderComparisonReport {
        guard canCompare, other.canCompare, self !== other, let before = page, let after = other.page else {
            throw AtlasReaderError.unavailable
        }
        let report = try await coordinator.compare(with: other.coordinator, limits: limits)
        try Task.checkCancellation()
        guard report.matches(before: page?.identity, after: other.page?.identity),
              page?.identity.matches(before.identity) == true,
              other.page?.identity.matches(after.identity) == true else { throw AtlasReaderError.canceled }
        return report
    }

    /// A comparison span is a byte anchor, not a query hit or decoded selection.
    /// The regular reader may widen its page to complete a scalar/CRLF; its
    /// existing page/copy UI continues to state those original-byte boundaries.
    @discardableResult func goToComparedRange(_ range: AtlasReaderComparisonReport.Range,
                                              identity: AtlasReaderIdentity) -> Bool {
        guard !busy, let page, page.identity.matches(identity), range.start <= range.end,
              range.end <= identity.capturedBytes else { return false }
        navigate(.window(offset: range.start, bytes: AtlasReaderLimits.pageBytes))
        return true
    }

    func find() {
        guard canFind else { return }
        let needle = fileQuery
        do { try AtlasReaderFindReport.validateNeedle(needle) }
        catch { notice = AtlasReaderError.invalidInput.message; return }
        clearFileSearch()
        let ticket = begin(query: true)
        notice = "Searching the retained file…"
        task = Task {
            do {
                let report = try await coordinator.find(needle)
                guard operation == ticket, !Task.isCancelled, fileQuery.utf8.elementsEqual(needle.utf8) else { return }
                findReport = report
                finish(ticket)
                notice = report.summary
                if !report.hits.isEmpty { moveHit(backwards: false) }
            } catch { fail(error, ticket: ticket) }
        }
    }
    func clearFileSearch() {
        if queryWork { cancel() }
        findReport = nil; selectedHitIndex = nil
        if page?.selection != nil { selectedRange = nil }
        coordinator.clearFind()
    }
    func moveHit(backwards: Bool) {
        guard canMoveHit, let report = findReport else { return }
        let count = report.hits.count
        let index: Int
        if let selected = selectedHitIndex {
            index = backwards ? (selected == 0 ? count - 1 : selected - 1) : (selected == count - 1 ? 0 : selected + 1)
        } else { index = backwards ? count - 1 : 0 }
        guard let target = report.target(at: index) else { return }
        navigate(.hit(target), hitIndex: index)
    }

    /// Extract once, independently of literal search. Unsupported or oversized
    /// source stays readable; engine diagnostics remain visible in the browser.
    func buildOutline() {
        guard canBuildOutline else { return }
        clearOutlineFilter(reset: true)
        let language = outlineLanguage
        let ticket = begin(outline: true)
        notice = "Extracting source outline…"
        task = Task {
            do {
                let page = try await coordinator.prepareOutline(language: language)
                guard operation == ticket, !Task.isCancelled, outlineLanguage == language else { return }
                outlinePage = page
                hasOutline = true
                outlineRowsAuthorized = true
                finish(ticket)
                notice = page.summary
                if !outlineNeedle.isEmpty { filterOutline() }
            } catch { fail(error, ticket: ticket) }
        }
    }

    func filterOutline() {
        guard hasOutline else { return }
        loadOutlinePage(AtlasOutlinePageRequest(needle: outlineNeedle, mode: outlineMode, start: 0, limit: 128), filtering: true)
    }
    func nextOutlinePage() {
        guard let page = outlinePage, let next = page.nextOffset else { return }
        loadOutlinePage(AtlasOutlinePageRequest(needle: page.request.needle,
            mode: page.request.mode, start: next, limit: 128))
    }
    func previousOutlinePage() {
        guard let request = outlineHistory.last else { return }
        loadOutlinePage(request, goingBack: true)
    }
    func openSymbol(_ target: AtlasOutlineTarget) {
        guard outlineIsCurrent, outlinePage?.target(id: target.symbol.id) == target else { return }
        navigate(.symbol(target))
    }

    /// An outline candidate supplies a name, never inferred binding authority.
    /// Validate its original page token before starting an independent query.
    func findReferences(for target: AtlasOutlineTarget) {
        guard outlineIsCurrent, outlinePage?.target(id: target.symbol.id) == target,
              canFindReferences else { return }
        referenceName = target.symbol.name
        findReferences()
    }
    func findReferences() {
        guard canFindReferences else { return }
        let name = referenceName
        do { try AtlasReferencePage.validateName(name) }
        catch { notice = "Enter one identifier, up to 1,024 UTF-8 bytes."; return }
        referenceRowsAuthorized = false
        referenceHistory = []
        let ticket = begin(references: true)
        notice = "Finding whole-identifier text occurrences in this capture…"
        task = Task {
            do {
                let result = try await coordinator.prepareReferences(name)
                guard operation == ticket, !Task.isCancelled,
                      referenceName.utf8.elementsEqual(name.utf8) else { return }
                referencePage = result
                referenceRowsAuthorized = true
                finish(ticket)
                notice = result.summary
            } catch { fail(error, ticket: ticket) }
        }
    }
    func clearReferenceSearch() {
        if referenceWork { cancel() }
        referenceRowsAuthorized = false
        referencePage = nil
        referenceHistory = []
        if page?.referenceSelection != nil { selectedRange = nil }
        coordinator.resetReferences()
    }
    func nextReferencePage() {
        guard referencesAreCurrent, let page = referencePage, let next = page.nextOffset else { return }
        loadReferencePage(.init(start: next, limit: 128))
    }
    func previousReferencePage() {
        guard referencesAreCurrent, let request = referenceHistory.last else { return }
        loadReferencePage(request, goingBack: true)
    }
    func openReference(_ target: AtlasReferenceTarget) {
        guard referencesAreCurrent, referencePage?.target(id: target.occurrence.id) == target else { return }
        navigate(.reference(target))
    }
    private func loadReferencePage(_ request: AtlasReferencePageRequest, goingBack: Bool = false) {
        do { try request.validate() }
        catch { notice = AtlasReaderError.invalidInput.message; return }
        let previous = referencePage?.request
        referenceRowsAuthorized = false
        let ticket = begin(references: true)
        notice = "Loading retained occurrence page…"
        task = Task {
            do {
                let result = try await coordinator.referencePage(request)
                guard operation == ticket, !Task.isCancelled else { return }
                if goingBack { _ = referenceHistory.popLast() }
                else if let previous, previous != request {
                    referenceHistory.append(previous)
                    if referenceHistory.count > 64 { referenceHistory.removeFirst() }
                }
                referencePage = result
                referenceRowsAuthorized = true
                finish(ticket)
                notice = result.summary
            } catch { fail(error, ticket: ticket) }
        }
    }

    /// Query edits invalidate the UI rows before a new asynchronous filter can
    /// return. They never remove a literal hit's independent activation/copy.
    private func clearOutlineFilter(reset: Bool = false) {
        if outlineWork { cancel() }
        outlinePage = nil
        outlineHistory = []
        outlineRowsAuthorized = false
        if page?.symbolSelection != nil { selectedRange = nil }
        if reset { coordinator.resetOutline(); hasOutline = false }
        else { coordinator.revokeOutlinePage() }
    }
    private func loadOutlinePage(_ request: AtlasOutlinePageRequest,
        goingBack: Bool = false, filtering: Bool = false) {
        do { try request.validate() }
        catch { notice = AtlasReaderError.invalidInput.message; return }
        let previous = outlinePage?.request
        let ticket = begin(outline: true)
        outlineRowsAuthorized = false
        notice = "Filtering retained outline…"
        task = Task {
            do {
                let result = try await coordinator.outlineSymbols(request)
                guard operation == ticket, !Task.isCancelled else { return }
                if goingBack { _ = outlineHistory.popLast() }
                else if filtering { outlineHistory = [] }
                else if let previous, previous != request {
                    outlineHistory.append(previous)
                    if outlineHistory.count > 64 { outlineHistory.removeFirst() }
                }
                outlinePage = result
                outlineRowsAuthorized = true
                finish(ticket)
                notice = result.summary
            } catch { fail(error, ticket: ticket) }
        }
    }

    /// Explicit cancellation keeps a successfully installed page. Cancellation
    /// during initial capture instead releases its handle after work drains.
    func cancel() {
        cancelPin()
        let wasRefreshing = isRefreshing
        documentPreview.cancel(silent: true)
        operation = UUID()
        task?.cancel(); task = nil
        coordinator.cancel()
        if page == nil {
            coordinator.close()
            refreshInput = nil; refreshAccessLease = nil
        }
        busy = false; isRefreshing = false
        queryWork = false
        outlineWork = false
        referenceWork = false
        positionWork = false
        notice = wasRefreshing ? "Refresh canceled. The previous retained source is unchanged."
            : (page == nil ? "Source opening canceled." : "Navigation canceled. The current retained page is unchanged.")
    }
    func close() {
        cancelPin()
        documentPreview.close()
        operation = UUID()
        task?.cancel(); task = nil
        coordinator.close()
        refreshInput = nil; refreshAccessLease = nil
        isRefreshing = false; refreshedFromDisk = false; forkedCapture = false
        retainedOnly = false
        page = nil; source = nil; history.reset()
        findReport = nil; selectedHitIndex = nil; selectedRange = nil
        queryWork = false; outlineWork = false; referenceWork = false; positionWork = false
        fileQuery = ""
        outlinePage = nil; hasOutline = false; outlineRowsAuthorized = false
        outlineHistory = []; outlineNeedle = ""; outlineMode = .contains; outlineLanguage = .automatic
        referencePage = nil; referenceRowsAuthorized = false; referenceHistory = []; referenceName = ""
        busy = false; notice = ""
        offsetInput = "0"; lineInput = "1"; columnInput = "1"; columnKind = .unicodeScalar
        navigation = UUID()
    }

    private func cancelPin() {
        pinOperation = nil
        let child = pendingPin
        pendingPin = nil
        pinning = false
        child?.close()
    }

    private func navigate(_ request: AtlasReaderRequest, historyMove: SourceHistory.Move? = nil, hitIndex: Int? = nil) {
        guard let previous = page else { return }
        do { try request.validate(capturedBytes: previous.identity.capturedBytes) }
        catch { notice = AtlasReaderError.invalidInput.message; return }
        let move = historyMove ?? history.visit(request)
        let isSymbol: Bool
        if case .symbol = request { isSymbol = true } else { isSymbol = false }
        let isPosition: Bool
        if case .position = request { isPosition = true } else { isPosition = false }
        let isReference: Bool
        if case .reference = request { isReference = true } else { isReference = false }
        let ticket = begin(query: hitIndex != nil, outline: isSymbol, references: isReference, position: isPosition)
        notice = "Reading retained source…"
        task = Task {
            do {
                let next = try await coordinator.read(request)
                guard operation == ticket, !Task.isCancelled else { return }
                guard install(next, request: request, moving: move) else { throw AtlasReaderError.invalidResponse }
                if let hitIndex { selectedHitIndex = hitIndex }
                finish(ticket)
            } catch { fail(error, ticket: ticket) }
        }
    }
    private func begin(query: Bool = false, outline: Bool = false, references: Bool = false, position: Bool = false) -> UUID {
        cancelPin()
        documentPreview.cancel(silent: true)
        task?.cancel()
        coordinator.cancel()
        operation = UUID()
        busy = true
        isRefreshing = false
        queryWork = query
        outlineWork = outline
        referenceWork = references
        positionWork = position
        return operation
    }
    @discardableResult private func install(_ page: AtlasReaderPage, request: AtlasReaderRequest,
                                           moving: SourceHistory.Move? = nil) -> Bool {
        // Both directions restore plain source pages, never an old query's hit
        // outline or occurrence target. Exact selection/copy authority must be re-earned.
        let position: AtlasReaderRequest
        switch request {
        case .hit, .symbol, .reference:
            position = .window(offset: page.start, bytes: max(4, min(AtlasReaderLimits.maximumPageBytes, page.end - page.start)))
        default: position = request
        }
        if let moving {
            guard history.commit(moving, resolved: position) else { return false }
        } else {
            history.reset(to: position)
        }
        // Input updates below describe this accepted navigation; they must not
        // revoke it as though the user edited a still-pending location request.
        positionWork = false
        self.page = page
        documentPreview.bind(identity: page.identity, sourceOffset: page.start)
        let source = AtlasSource(path: page.identity.displayPath, text: page.text)
        self.source = source
        selectedRange = page.selection.flatMap { source.utf16Range(byteStart: $0.utf8Start, byteEnd: $0.utf8End) }
            ?? page.symbolSelection.flatMap { source.utf16Range(byteStart: $0.utf8Start, byteEnd: $0.utf8End) }
            ?? page.referenceSelection.flatMap { source.utf16Range(byteStart: $0.utf8Start, byteEnd: $0.utf8End) }
        navigation = UUID()
        offsetInput = String(page.start)
        if let position = page.position {
            lineInput = String(position.line)
            columnInput = String(position.column)
            columnKind = position.kind
        } else if let line = page.firstPhysicalLine { lineInput = String(line) }
        notice = ""
        return true
    }
    private func finish(_ ticket: UUID) {
        guard operation == ticket else { return }
        busy = false; isRefreshing = false; task = nil; queryWork = false; outlineWork = false; referenceWork = false; positionWork = false
    }
    private func fail(_ error: Error, ticket: UUID) {
        guard operation == ticket, !Task.isCancelled else { return }
        notice = (error as? AtlasReaderError)?.message ?? AtlasReaderError.unavailable.message
        finish(ticket)
    }
    private static func number(_ text: String) -> UInt64? {
        let value = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let number = UInt64(value), String(number) == value else { return nil }
        return number
    }
}
