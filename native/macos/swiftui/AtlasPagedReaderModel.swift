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
    var findOptions = AtlasReaderFindOptions() {
        didSet { if oldValue != findOptions { clearFileSearch() } }
    }
    private(set) var findReport: AtlasReaderFindReport?
    private(set) var selectedHitIndex: Int?
    /// A complete count stays valid while paging this same immutable query.
    private var knownFindMatchCount: UInt64?
    private var selectedRange: NSRange?
    private(set) var comparisonSelection: AtlasReaderComparisonSelection?
    private var bracketSelection: AtlasReaderComparisonSelection?
    private(set) var textSelection: AtlasReaderTextSelection?
    private(set) var resolvingTextSelection = false
    private(set) var hasTextSelectionIntent = false
    private(set) var textSelectionNotice = ""
    private struct TextSelectionIntent {
        let id: UUID
        let source: AtlasSource
        let navigation: UUID
        let page: AtlasReaderPage
        let range: NSRange
    }
    @ObservationIgnored private var textSelectionIntent: TextSelectionIntent?
    @ObservationIgnored private var pendingTextSelection: TextSelectionIntent?
    @ObservationIgnored private var textSelectionTask: Task<Void, Never>?
    @ObservationIgnored private var textSelectionLoop: UUID?
    @ObservationIgnored private var queryWork = false
    @ObservationIgnored private var outlineWork = false
    @ObservationIgnored private var referenceWork = false
    @ObservationIgnored private var positionWork = false
    @ObservationIgnored private var delimiterWork = false
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
    @ObservationIgnored private var refreshImageSource: AtlasDocumentImageSource?
    @ObservationIgnored private var refreshAccessLease: AnyObject?
    @ObservationIgnored private let coordinator: AtlasReaderCoordinator
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var operation = UUID()
    @ObservationIgnored private var pinOperation: UUID?
    @ObservationIgnored private var pendingPin: AtlasPagedReaderModel?
    private var forkedCapture = false
    private var retainedOnly = false
    private var previewSourceFollowing = false

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
            richSupported: document?.prepareDisplay != nil && document?.displayPage != nil
                && document?.displayFont != nil && document?.displayFromSource != nil,
            findSupported: document?.find != nil,
            execute: { try await coordinator.document($0) },
            cancel: { coordinator.cancel() }, revoke: { coordinator.revokeDocument() })
        documentPreview.didBecomeIdle = { [weak self] in self?.resumeDeferredReaderWork() }
    }
    deinit { task?.cancel(); textSelectionTask?.cancel() }

    var canGoBack: Bool { history.canBack && !busy }
    var canGoForward: Bool { history.canForward && !busy }
    var canGoNext: Bool { page?.nextOffset != nil && !busy }
    /// The visible split pane opts in. Binding an accepted source page only
    /// queues its latest anchor; automatic preview work never preempts a source
    /// navigation, pin, or a canceled selection call that is still draining.
    func setPreviewSourceFollowing(_ enabled: Bool) {
        previewSourceFollowing = enabled
        documentPreview.setSourceFollowing(enabled)
        resumeDeferredReaderWork()
    }
    private func resumeDeferredReaderWork() {
        startTextSelectionWork()
        guard !busy, !pinning, !resolvingTextSelection, textSelectionTask == nil,
              !documentPreview.busy else { return }
        documentPreview.followSourceIfNeeded()
    }
    var canRefresh: Bool { page != nil && refreshInput != nil && !busy && !documentPreview.busy }
    /// An explicitly admitted current-file capability, never the page's label.
    /// Forks carry this lease independently; retained-only imports have none.
    func currentFileForExternalEditor() -> (input: AtlasReaderInput, lease: AnyObject?, identity: AtlasReaderIdentity)? {
        guard canRefresh, let refreshInput, let page else { return nil }
        return (refreshInput, refreshAccessLease, page.identity)
    }
    var canPin: Bool { page != nil && !busy && !pinning && !documentPreview.busy && !resolvingTextSelection && coordinator.canFork }
    var canKeep: Bool {
        guard canPin, let page, !hasTextSelectionIntent || textSelection != nil else { return false }
        if page.identity.capturedBytes <= AtlasReaderLimits.transferBytes { return true }
        guard let selected = rangeForReadingDesk else { return false }
        return selected.start < selected.end && selected.end <= page.identity.capturedBytes
            && selected.end - selected.start <= AtlasReaderLimits.maximumPageBytes
    }
    /// First-time Keep and Desk location updates share one exact range. Pending
    /// native intent never falls back to a page or an older search highlight.
    var rangeForReadingDesk: (start: UInt64, end: UInt64)? {
        guard let page else { return nil }
        if hasTextSelectionIntent {
            guard !resolvingTextSelection, let selected = textSelection,
                  selected.identity.matches(page.identity),
                  selected.pageStart == page.start, selected.pageEnd == page.end else { return nil }
            return (selected.originalStart, selected.originalEnd)
        }
        if let selected = currentComparisonSelection {
            return (selected.range.start, selected.range.end)
        }
        if findIsCurrent, let hit = page.selection, matchHex != nil {
            return (hit.target.start, hit.target.end)
        }
        if outlineIsCurrent, let symbol = page.symbolSelection, symbolHex != nil {
            let selected = symbol.target.symbol.selectedRange
            return (selected.start, selected.end)
        }
        if referencesAreCurrent, let occurrence = page.referenceSelection, referenceHex != nil {
            let selected = occurrence.target.occurrence.range
            return (selected.start, selected.end)
        }
        return (page.start, page.end)
    }
    var canCompare: Bool { page != nil && !busy && !pinning && !documentPreview.busy && !resolvingTextSelection && coordinator.supportsComparison }
    var retainedRegistryDomain: UUID? { coordinator.retainedRegistryDomain }
    var retainedCaptureSHA256: String? { coordinator.retainedCaptureSHA256 }
    /// Only a ready, currently displayed capture can authorize an export.
    /// The returned lifetime keeps its immutable source alive off this view.
    func retainedExportSource(matching identity: AtlasReaderIdentity, registryDomain: UUID)
        -> AtlasReaderRetainedExportSource? {
        guard !busy, !pinning, !documentPreview.busy, !resolvingTextSelection,
              page?.identity.matches(identity) == true else { return nil }
        return coordinator.retainedExportSource(matching: identity, registryDomain: registryDomain)
    }
    var canFind: Bool {
        page != nil && coordinator.supportsFind && (findOptions.mode != .expression || coordinator.supportsFindExpression)
    }
    var canFindExpression: Bool { coordinator.supportsFindExpression }
    var canConfigureFind: Bool {
        findOptions.mode == .expression ? coordinator.supportsFindExpression : coordinator.supportsFindOptions
    }
    var canGoToPosition: Bool { page != nil && coordinator.supportsPosition }
    var findIsCurrent: Bool {
        findReport?.needle.utf8.elementsEqual(fileQuery.utf8) == true && findReport?.options == findOptions
    }
    var canMoveHit: Bool { findIsCurrent && findReport?.hits.isEmpty == false && !busy }
    var findPositionLabel: String? {
        guard findIsCurrent, let report = findReport, let index = selectedHitIndex,
              report.hits.indices.contains(index) else { return nil }
        let ordinal = report.hits[index].occurrence
        if let total = knownFindMatchCount { return "Match \(ordinal) of \(total)" }
        return "Match \(ordinal) of at least \(report.matchesSeen)"
    }
    var nativeSelectionRange: NSRange? {
        if let bracketSelection, let page, bracketSelection.matches(page) { return bracketSelection.nativeRange }
        if let selection = currentComparisonSelection { return selection.nativeRange }
        if let position = page?.position { return position.nativeCaretRange }
        if findIsCurrent, page?.selection?.target.generation == findReport?.generation,
           page?.selection != nil { return selectedRange }
        if outlineIsCurrent, let selection = page?.symbolSelection,
           selection.target.pageID == outlinePage?.id { return selectedRange }
        if referencesAreCurrent, let selection = page?.referenceSelection,
           selection.target.pageID == referencePage?.id { return selectedRange }
        return nil
    }
    var currentComparisonSelection: AtlasReaderComparisonSelection? {
        guard let selection = comparisonSelection, let page, selection.matches(page) else { return nil }
        return selection
    }

    /// Recheck a retained menu action against the currently displayed range.
    /// Copying a clipped range is explicitly limited to its shown intersection.
    func comparisonCopy(_ format: AtlasReaderComparisonSelection.CopyFormat, selectionID: UUID) -> String? {
        guard !busy, let selection = currentComparisonSelection, selection.id == selectionID else { return nil }
        switch format {
        case .shownText: return selection.shownText
        case .shownHex: return selection.shownHex
        case .location:
            let witness = retainedCaptureSHA256.map { "\nCapture SHA-256: \($0)" } ?? ""
            return "\(selection.identity.displayPath)\nOriginal bytes \(selection.range.start)..<\(selection.range.end)"
                + witness
        }
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
    var supportsReferenceWindows: Bool { coordinator.supportsReferenceWindows }
    var referencesAreCurrent: Bool {
        guard referenceRowsAuthorized, let referencePage else { return false }
        return referencePage.inventory.name.utf8.elementsEqual(referenceName.utf8)
            && page?.identity.matches(referencePage.inventory.identity) == true
    }
    var canReferenceBack: Bool { referencesAreCurrent && !referenceHistory.isEmpty && !busy }
    var canReferenceWindowBack: Bool {
        referencesAreCurrent && !busy && (referencePage?.inventory.window?.request.offset ?? 0) > 0
    }
    var canReferenceWindowNext: Bool {
        referencesAreCurrent && !busy && referencePage?.inventory.window?.nextOffset != nil
    }
    var referenceHex: String? {
        guard referencesAreCurrent, let selection = page?.referenceSelection,
              selection.target.pageID == referencePage?.id else { return nil }
        return selection.originalHex
    }
    var referenceSelectionLabel: String? {
        guard referenceHex != nil, let target = page?.referenceSelection?.target else { return nil }
        return "Identifier occurrence \(target.occurrence.ordinal) · LF line \(target.occurrence.line)"
    }

    /// A retained menu action must still name the displayed selection receipt.
    /// No selected string is reconstructed from native offsets or a live path.
    private var searchableSelection: AtlasReaderTextSelection? {
        guard !busy, !pinning, !documentPreview.busy, !resolvingTextSelection,
              let selection = textSelection, let intent = textSelectionIntent, let page,
              source === intent.source, navigation == intent.navigation,
              page.identity.matches(selection.identity), page.identity.matches(intent.page.identity),
              page.start == selection.pageStart, page.end == selection.pageEnd,
              page.start == intent.page.start, page.end == intent.page.end,
              selection.nativeRange == intent.range else { return nil }
        return selection
    }
    var canFindSelectedText: Bool {
        page != nil && coordinator.supportsFind
            && searchableSelection?.searchText(maximumUTF8Bytes: AtlasReaderFindReport.maximumNeedleBytes) != nil
    }
    var canGoToMatchingBracket: Bool {
        coordinator.supportsMatchingDelimiter && textSelectionTask == nil
            && searchableSelection?.matchingBracket != nil
    }

    /// Keep the existing page, exact text selection and history until both the
    /// lexical receipt and its independently read destination are verified.
    @discardableResult func goToMatchingBracket(selectionID: UUID) -> Bool {
        guard canGoToMatchingBracket, let selection = searchableSelection,
              selection.id == selectionID, let intent = textSelectionIntent else { return false }
        let ticket = begin(delimiter: true)
        notice = "Finding the matching bracket in retained source…"
        task = Task {
            do {
                let result = try await coordinator.matchingDelimiter(selection: selection)
                guard acceptsDelimiter(ticket: ticket, selection: selection, intent: intent),
                      result.selectionID == selection.id else { throw AtlasReaderError.canceled }
                guard let range = result.matchingRange else {
                    notice = result.notice
                    finish(ticket)
                    return
                }
                let request = AtlasReaderRequest.window(offset: range.start - min(range.start, 256),
                    bytes: AtlasReaderLimits.pageBytes)
                let next = try await coordinator.read(request)
                guard acceptsDelimiter(ticket: ticket, selection: selection, intent: intent),
                      next.identity.matches(selection.identity) else { throw AtlasReaderError.canceled }
                let mapping = Task.detached(priority: .userInitiated) {
                    try Task.checkCancellation()
                    return try result.selection(in: next)
                }
                let destination = try await withTaskCancellationHandler(operation: { try await mapping.value },
                    onCancel: { mapping.cancel() })
                guard acceptsDelimiter(ticket: ticket, selection: selection, intent: intent),
                      let nativeRange = destination.nativeRange,
                      install(next, request: request, moving: history.visit(request)) else {
                    throw AtlasReaderError.canceled
                }
                bracketSelection = destination
                delimiterWork = false
                notice = result.notice
                // Programmatic AppKit navigation does not emit a user selection
                // callback. Resolve the displayed mate through the same exact
                // selection path so repeated jumps and copying retain provenance.
                if let source { updateTextSelection(source: source, navigation: navigation, range: nativeRange) }
                finish(ticket)
            } catch { fail(error, ticket: ticket) }
        }
        return true
    }

    private func cancelDelimiterForSelectionChange() {
        guard delimiterWork else { return }
        operation = UUID()
        task?.cancel(); task = nil
        coordinator.cancel()
        busy = false; delimiterWork = false
        notice = "Bracket navigation canceled because the selection changed."
    }

    private func acceptsDelimiter(ticket: UUID, selection: AtlasReaderTextSelection,
                                  intent: TextSelectionIntent) -> Bool {
        !Task.isCancelled && busy && delimiterWork && operation == ticket
            && textSelection?.id == selection.id && textSelectionIntent?.id == intent.id
            && source === intent.source && navigation == intent.navigation
            && page?.identity.matches(selection.identity) == true
            && page?.start == selection.pageStart && page?.end == selection.pageEnd
    }
    var canFindSelectedDeclarations: Bool {
        canBuildOutline && searchableSelection?.searchText(maximumUTF8Bytes: 256) != nil
    }
    var canFindSelectedOccurrences: Bool {
        canFindReferences && searchableSelection?.searchText(maximumUTF8Bytes: AtlasReferencePage.nameBytes) != nil
    }
    func selectionQuery(_ mode: AtlasSearchMode, selectionID: UUID) -> AtlasReaderSelectionQuery? {
        guard let selection = searchableSelection, selection.id == selectionID else { return nil }
        return try? AtlasReaderSelectionQuery(selection: selection, mode: mode)
    }
    @discardableResult func findSelectedText(selectionID: UUID) -> Bool {
        guard canFindSelectedText, let selection = searchableSelection, selection.id == selectionID,
              let text = selection.searchText(maximumUTF8Bytes: AtlasReaderFindReport.maximumNeedleBytes) else { return false }
        fileQuery = text
        findOptions.mode = .literal
        findOptions.caseSensitive = true
        find()
        return true
    }
    @discardableResult func findSelectedDeclarations(selectionID: UUID) -> Bool {
        guard canBuildOutline, let selection = searchableSelection, selection.id == selectionID,
              let text = selection.searchText(maximumUTF8Bytes: 256) else { return false }
        outlineNeedle = text
        outlineMode = .exact
        if hasOutline { filterOutline() } else { buildOutline() }
        return true
    }
    @discardableResult func findSelectedOccurrences(selectionID: UUID) -> Bool {
        guard canFindReferences, let selection = searchableSelection, selection.id == selectionID,
              let text = selection.searchText(maximumUTF8Bytes: AtlasReferencePage.nameBytes) else { return false }
        // The shared engine validates the identifier grammar. Native code only
        // enforces transport bounds and never substitutes its own token rules.
        referenceName = text
        findReferences()
        return true
    }

    /// Only the displayed immutable source and navigation may report native
    /// coordinates. Selection notifications retain a small descriptor; one
    /// draining task and one newest descriptor bound work during a drag.
    func updateTextSelection(source: AtlasSource, navigation: UUID, range: NSRange?) {
        guard self.source === source, self.navigation == navigation, let page else { return }
        comparisonSelection = nil
        if range != bracketSelection?.nativeRange { bracketSelection = nil }
        guard let range, range.length != 0 else {
            cancelDelimiterForSelectionChange()
            clearTextSelection()
            resumeDeferredReaderWork()
            return
        }
        if let intent = textSelectionIntent, intent.source === source,
           intent.navigation == navigation, intent.range == range,
           textSelection != nil || resolvingTextSelection { return }
        cancelDelimiterForSelectionChange()
        let intent = TextSelectionIntent(id: UUID(), source: source, navigation: navigation,
            page: page, range: range)
        textSelectionIntent = intent
        hasTextSelectionIntent = true
        textSelection = nil
        coordinator.cancelTextSelection()
        guard coordinator.supportsTextSelection, !pinning, !documentPreview.busy else {
            pendingTextSelection = nil
            resolvingTextSelection = false
            textSelectionNotice = coordinator.supportsTextSelection
                ? "Select again after the reader finishes its current operation."
                : "Exact selected-source actions are unavailable for this reader."
            return
        }
        pendingTextSelection = intent
        resolvingTextSelection = true
        textSelectionNotice = "Resolving the selection against retained source bytes…"
        startTextSelectionWork()
    }

    /// Retry the still-displayed descriptor without depending on another
    /// AppKit notification or reopening a live path.
    func retryTextSelection() {
        guard let intent = textSelectionIntent, self.source === intent.source,
              navigation == intent.navigation, !busy, !pinning, !documentPreview.busy else { return }
        textSelectionIntent = nil
        updateTextSelection(source: intent.source, navigation: intent.navigation, range: intent.range)
    }

    /// A failed navigation or canceled query can leave the same native text
    /// selected. Preserve that intent while revoking its receipt, then resolve
    /// it again when the reader lane becomes available. Never turn it into an
    /// implicit page-wide Keep.
    private func suspendTextSelection() {
        guard let intent = textSelectionIntent else { return }
        textSelection = nil
        pendingTextSelection = intent
        resolvingTextSelection = true
        textSelectionNotice = "Resolving the displayed selection after the reader operation…"
        coordinator.cancelTextSelection()
        textSelectionTask?.cancel()
    }

    /// Revocation is synchronous even while the prior foreign call drains.
    /// Refused or pending text never becomes a page-wide Keep by implication.
    private func clearTextSelection() {
        textSelectionIntent = nil
        pendingTextSelection = nil
        textSelection = nil
        hasTextSelectionIntent = false
        resolvingTextSelection = false
        textSelectionNotice = ""
        coordinator.cancelTextSelection()
        textSelectionTask?.cancel()
    }

    private func startTextSelectionWork() {
        guard textSelectionTask == nil, pendingTextSelection != nil,
              !busy, !pinning, !documentPreview.busy else { return }
        let loop = UUID()
        textSelectionLoop = loop
        textSelectionTask = Task {
            defer {
                if textSelectionLoop == loop {
                    textSelectionLoop = nil
                    textSelectionTask = nil
                    resumeDeferredReaderWork()
                }
            }
            while !Task.isCancelled, let intent = pendingTextSelection {
                pendingTextSelection = nil
                guard self.source === intent.source, navigation == intent.navigation else { continue }
                guard !busy, !pinning, !documentPreview.busy else {
                    if textSelectionIntent?.id == intent.id {
                        resolvingTextSelection = false
                        textSelectionNotice = "Select again after the reader finishes its current operation."
                    }
                    continue
                }
                do {
                    let resolved = try await coordinator.resolveTextSelection(page: intent.page, range: intent.range)
                    guard !Task.isCancelled, textSelectionIntent?.id == intent.id,
                          self.source === intent.source, navigation == intent.navigation,
                          page?.identity.matches(intent.page.identity) == true,
                          page?.start == intent.page.start, page?.end == intent.page.end else { continue }
                    guard !busy, !pinning, !documentPreview.busy else {
                        resolvingTextSelection = false
                        textSelectionNotice = "The reader changed operations before selection delivery. Retry the displayed selection."
                        continue
                    }
                    textSelection = resolved
                    resolvingTextSelection = false
                    textSelectionNotice = resolved.summary
                } catch {
                    guard !Task.isCancelled, textSelectionIntent?.id == intent.id,
                          self.source === intent.source, navigation == intent.navigation else { continue }
                    textSelection = nil
                    resolvingTextSelection = false
                    textSelectionNotice = "The selected range could not be resolved exactly. Select a complete character range again. "
                        + ((error as? AtlasReaderError)?.message ?? AtlasReaderError.unavailable.message)
                }
            }
        }
    }

    var coverage: String {
        guard let page else { return "Retained capture limit: \(AtlasReaderLimits.captureBytes / 1024 / 1024) MiB. Oversized sources are refused, not truncated." }
        var messages = ["Logical page; layout is local to this window."]
        if page.identity.capturedBytes > AtlasReaderLimits.transferBytes {
            messages.append("Keep saves a selected passage or page up to 256 KiB with its original offsets. Comparison and complete-source desk transfer are limited to 4 MiB per source. Paging, Find and pinning use this retained capture.")
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
        let allowedImages = refreshImageSource
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
                resumeDeferredReaderWork()
            }
        }
        do {
            let copied = try await child.coordinator.fork(from: coordinator, page: original)
            try Task.checkCancellation()
            guard pinOperation == ticket, navigation == sourceNavigation,
                  page?.identity.matches(original.identity) == true else { throw AtlasReaderError.canceled }
            child.refreshInput = allowedRefresh
            child.refreshImageSource = allowedImages
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

    func open(root: String, path: String, accessLease: AnyObject?, initialLine: UInt64? = nil) {
        do {
            let request = try beginOpen(root: root, path: path, accessLease: accessLease, initialLine: initialLine)
            task = Task {
                do { try await completeOpen(request) }
                catch { /* completeOpen reports failure and retires an unpublished capture. */ }
            }
        } catch {
            notice = (error as? AtlasReaderError)?.message ?? AtlasReaderError.invalidInput.message
        }
    }

    /// Prepare a new, independent reader before a workspace history accepts it.
    /// The caller owns publication and closes this model if it is superseded.
    /// Cancellation waits for entered native work to drain before returning.
    func prepareOpen(root: String, path: String, accessLease: AnyObject?, initialLine: UInt64? = nil) async throws {
        try Task.checkCancellation()
        guard page == nil, !busy else { throw AtlasReaderError.unavailable }
        let request = try beginOpen(root: root, path: path, accessLease: accessLease, initialLine: initialLine)
        try await completeOpen(request)
    }

    private struct PreparedOpen {
        let input: AtlasReaderInput
        let accessLease: AnyObject?
        let imageSource: AtlasDocumentImageSource?
        let initialLine: UInt64?
        let ticket: UUID
    }

    /// Set up the operation before assigning the view-owned Task: begin()
    /// cancels earlier work and must never cancel the task being initialized.
    private func beginOpen(root: String, path: String, accessLease: AnyObject?,
                           initialLine: UInt64?) throws -> PreparedOpen {
        close()
        let input: AtlasReaderInput
        guard capturedTarget == nil || suppliedSource == nil else { throw AtlasReaderError.invalidInput }
        if let initialLine {
            guard initialLine > 0, capturedTarget == nil else { throw AtlasReaderError.invalidInput }
        }
        let liveInput = try AtlasReaderInput(root: root, path: path)
        if let target = capturedTarget {
            guard target.root.utf8.elementsEqual(root.utf8), target.path.utf8.elementsEqual(path.utf8),
                  !target.mode.isTextual || coordinator.supportsFind,
                  target.mode != .occurrences || coordinator.supportsReferences else { throw AtlasReaderError.invalidInput }
            input = try AtlasReaderInput(captured: target)
            findOptions = .init()
            fileQuery = target.mode.isTextual ? target.needle : ""
            referenceName = target.mode == .occurrences ? target.needle : ""
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
        refreshImageSource = try? AtlasDocumentImageSource(root: root, relativePath: path)
        refreshAccessLease = accessLease
        let imageSource = refreshImageSource
        if let initialLine { lineInput = String(initialLine) }
        let ticket = begin(query: capturedTarget?.mode.isTextual == true,
            references: capturedTarget?.mode == .occurrences, position: initialLine != nil)
        notice = capturedTarget != nil ? "Opening the exact retained search capture…"
            : suppliedSource != nil ? "Opening the complete source already displayed…"
            : "Capturing source for bounded page navigation…"
        return PreparedOpen(input: input, accessLease: accessLease, imageSource: imageSource,
            initialLine: initialLine, ticket: ticket)
    }

    private func completeOpen(_ request: PreparedOpen) async throws {
        let input = request.input, accessLease = request.accessLease, imageSource = request.imageSource
        let initialLine = request.initialLine, ticket = request.ticket
        do {
            let opened = try await coordinator.open(input: input, accessLease: accessLease, imageSource: imageSource)
            guard operation == ticket, !Task.isCancelled else { throw AtlasReaderError.canceled }
            if let target = capturedTarget {
                if target.mode.isTextual {
                    // Literal hits and expression anchor terms receive
                    // this reader's own exact-spelling query IDs.
                    let selected = try await AtlasCapturedReader.activate(target, identity: opened.identity,
                        find: { try await coordinator.find($0) },
                        nextPage: { needle, cursor in
                            try await coordinator.find(needle, startAfter: cursor)
                        }, read: { try await coordinator.read(.hit($0)) },
                        isCurrent: {
                            operation == ticket && fileQuery.utf8.elementsEqual(target.needle.utf8)
                                && findOptions == .init()
                        })
                    guard operation == ticket, !Task.isCancelled else { throw AtlasReaderError.canceled }
                    findReport = selected.report
                    knownFindMatchCount = selected.report.complete ? selected.report.matchesSeen : nil
                    selectedHitIndex = selected.index
                    install(selected.page, request: .hit(selected.target))
                } else if target.mode == .occurrences {
                    let selected = try await AtlasCapturedReader.activateOccurrence(target,
                        identity: opened.identity,
                        prepare: { try await coordinator.prepareReferences($0) },
                        nextPage: { try await coordinator.referencePage($0) },
                        prepareWindow: coordinator.supportsReferenceWindows ? { name, offset in
                            try await coordinator.prepareReferences(name,
                                window: AtlasReferenceWindowRequest(offset: offset, countAllMatches: true))
                        } : nil,
                        read: { try await coordinator.read(.reference($0)) },
                        isCurrent: {
                            operation == ticket && referenceName.utf8.elementsEqual(target.needle.utf8)
                        })
                    guard operation == ticket, !Task.isCancelled else { throw AtlasReaderError.canceled }
                    referencePage = selected.referencePage
                    referenceRowsAuthorized = true
                    referenceHistory = selected.history
                    install(selected.page, request: .reference(selected.target))
                } else {
                    let request = try AtlasCapturedReader.structuralRequest(target, identity: opened.identity)
                    let selected = try await coordinator.read(request)
                    guard operation == ticket, !Task.isCancelled else { throw AtlasReaderError.canceled }
                    install(selected, request: request)
                }
                finish(ticket)
                if target.mode == .occurrences {
                    notice = "Opened the exact whole-token occurrence in the retained search capture. Comments and literals are included; compiler resolution is not claimed. No live-file reread."
                } else {
                    notice = target.mode.isTextual ? "Opened the exact search capture without rereading the live file."
                        : "Opened retained \(target.mode == .headings ? "heading" : "declaration") evidence at original bytes \(target.start)–\(target.end). No live-file reread."
                }
            } else if let initialLine {
                let request = AtlasReaderRequest.position(line: initialLine, column: 1,
                    kind: .unicodeScalar, bytes: AtlasReaderLimits.pageBytes)
                do {
                    // Resolve against the just-opened immutable capture.
                    // No native offset guess, second open or live reread.
                    let located = try await coordinator.read(request)
                    guard operation == ticket, !Task.isCancelled else { throw AtlasReaderError.canceled }
                    install(located, request: request)
                    finish(ticket)
                    notice = "Opened requested line \(initialLine) in the retained source."
                } catch {
                    guard operation == ticket, !Task.isCancelled else { throw AtlasReaderError.canceled }
                    // Keep a useful readable capture when the requested
                    // line is absent or cannot be mapped, and report it.
                    install(opened, request: .firstPage)
                    finish(ticket)
                    notice = "Could not open requested line \(initialLine). Showing the first page. "
                        + ((error as? AtlasReaderError)?.message ?? AtlasReaderError.unavailable.message)
                }
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
                refreshInput = nil; refreshImageSource = nil; refreshAccessLease = nil
            }
            fail(error, ticket: ticket)
            if operation == ticket, Task.isCancelled { finish(ticket) }
            throw error
        }
    }

    /// Open one explicitly authorized file without creating parent-directory
    /// authority. This initializer is transactional for its host: only a new,
    /// empty model may use it and the host publishes it after successful return.
    /// Refresh and forks retain this exact path/lease, with no local image root.
    func openSelectedFile(input: AtlasReaderInput, accessLease: AnyObject?) async throws {
        try Task.checkCancellation()
        guard page == nil, !busy, capturedTarget == nil, suppliedSource == nil else {
            throw AtlasReaderError.unavailable
        }
        let ticket = begin()
        notice = "Capturing the selected file for bounded page navigation…"
        do {
            let replacement = try await coordinator.stageOpen(input: input, accessLease: accessLease)
            try Task.checkCancellation()
            guard operation == ticket else { throw AtlasReaderError.canceled }
            let opened = try coordinator.commitOpen(replacement)
            refreshInput = input
            refreshImageSource = nil
            refreshAccessLease = accessLease
            install(opened, request: .firstPage)
            finish(ticket)
            notice = "Opened the selected file. Reading nearby files requires a separate project-folder choice."
        } catch {
            if operation == ticket {
                coordinator.close()
                refreshInput = nil; refreshImageSource = nil; refreshAccessLease = nil
                fail(error, ticket: ticket)
            }
            throw error
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
    func openRetained(registryDomain: UUID, accessLease: AnyObject? = nil,
        initialize: @escaping @Sendable (UInt64) throws -> AtlasReaderRetainedInitialization) async throws {
        guard page == nil, !busy, capturedTarget == nil, suppliedSource == nil else {
            throw AtlasReaderError.unavailable
        }
        let ticket = begin()
        notice = "Opening the exact retained capture…"
        do {
            let opened = try await coordinator.openRetained(registryDomain: registryDomain,
                accessLease: accessLease, initialize: initialize)
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
        let imageSource = refreshImageSource
        let ticket = begin()
        isRefreshing = true
        notice = "Capturing current disk bytes… The retained source remains unchanged until refresh succeeds."
        task = Task {
            do {
                let replacement = try await coordinator.stageOpen(input: input,
                    accessLease: accessLease, imageSource: imageSource, using: liveTransport)
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

    /// Reserve an independently owned reader without publishing a half-opened
    /// pair. The complete byte target survives a clipped presentation window.
    func beginComparedRange(_ range: AtlasReaderComparisonReport.Range,
                           identity: AtlasReaderIdentity, atEnd: Bool = false) throws -> AtlasReaderComparisonNavigation {
        guard !busy, !pinning, !documentPreview.busy, !resolvingTextSelection,
              let page, page.identity.matches(identity), range.start <= range.end,
              range.end <= identity.capturedBytes else { throw AtlasReaderError.invalidInput }
        let context: UInt64 = 256
        let offset = atEnd
            ? range.end - min(range.end, AtlasReaderLimits.pageBytes - context)
            : range.start - min(range.start, context)
        let request = AtlasReaderRequest.window(offset: offset, bytes: AtlasReaderLimits.pageBytes)
        try request.validate(capturedBytes: identity.capturedBytes)
        let previousNavigation = navigation
        let ticket = begin()
        notice = "Opening the compared byte range…"
        return .init(operation: ticket, previousNavigation: previousNavigation,
            identity: identity, range: range, request: request)
    }

    private func acceptsComparedRange(_ target: AtlasReaderComparisonNavigation) -> Bool {
        busy && operation == target.operation && navigation == target.previousNavigation
            && page?.identity.matches(target.identity) == true
    }

    func prepareComparedRange(_ target: AtlasReaderComparisonNavigation) async throws -> AtlasReaderComparisonPage {
        try Task.checkCancellation()
        guard acceptsComparedRange(target) else { throw AtlasReaderError.canceled }
        let next = try await coordinator.read(target.request)
        try Task.checkCancellation()
        guard acceptsComparedRange(target), next.identity.matches(target.identity) else { throw AtlasReaderError.canceled }
        // Only this admitted window is mapped, off the main actor. Canceling
        // the navigation also revokes a worker that has not started mapping.
        let mapping = Task.detached(priority: .userInitiated) {
            try Task.checkCancellation()
            return try AtlasReaderComparisonSelection.make(range: target.range, identity: target.identity, page: next)
        }
        let selection = try await withTaskCancellationHandler(operation: { try await mapping.value },
            onCancel: { mapping.cancel() })
        try Task.checkCancellation()
        guard acceptsComparedRange(target) else { throw AtlasReaderError.canceled }
        return .init(navigation: target, page: next, selection: selection)
    }

    func canCommitComparedRange(_ candidate: AtlasReaderComparisonPage) -> Bool {
        acceptsComparedRange(candidate.navigation) && candidate.selection.matches(candidate.page)
            && candidate.selection.range == candidate.navigation.range
            && candidate.page.identity.matches(candidate.navigation.identity)
    }

    /// The comparison host checks BOTH candidates, then commits both in the
    /// same MainActor turn, with no suspension between these installations.
    @discardableResult func commitComparedRange(_ candidate: AtlasReaderComparisonPage) -> Bool {
        guard canCommitComparedRange(candidate) else { return false }
        let request = candidate.navigation.request
        guard install(candidate.page, request: request, moving: history.visit(request)) else { return false }
        comparisonSelection = candidate.selection
        finish(candidate.navigation.operation)
        return true
    }

    func cancelComparedRange(_ target: AtlasReaderComparisonNavigation) {
        guard operation == target.operation else { return }
        cancel()
    }

    @discardableResult func goToComparedRange(_ range: AtlasReaderComparisonReport.Range,
                                              identity: AtlasReaderIdentity, atEnd: Bool = false) -> Bool {
        guard let target = try? beginComparedRange(range, identity: identity, atEnd: atEnd) else { return false }
        task = Task {
            do {
                let candidate = try await prepareComparedRange(target)
                guard commitComparedRange(candidate) else { throw AtlasReaderError.canceled }
            } catch { fail(error, ticket: target.operation) }
        }
        return true
    }

    func find() {
        guard canFind else { return }
        let needle = fileQuery
        let options = findOptions
        do { try AtlasReaderFindReport.validateNeedle(needle, mode: options.mode) }
        catch {
            notice = options.mode == .expression
                ? "Enter an expression of up to 1024 UTF-8 bytes, without NUL characters."
                : AtlasReaderError.invalidInput.message
            return
        }
        clearFileSearch()
        let ticket = begin(query: true)
        notice = options.mode == .expression
            ? "Checking expression terms across the retained file…" : "Searching the retained file…"
        task = Task {
            do {
                let report = try await coordinator.find(needle, options: options)
                guard operation == ticket, !Task.isCancelled, fileQuery.utf8.elementsEqual(needle.utf8),
                      findOptions == options else { return }
                findReport = report
                knownFindMatchCount = report.complete ? report.matchesSeen : nil
                finish(ticket)
                notice = report.summary
                if !report.hits.isEmpty { moveHit(backwards: false) }
            } catch { fail(error, ticket: ticket) }
        }
    }
    func clearFileSearch() {
        if queryWork { cancel() }
        findReport = nil; selectedHitIndex = nil; knownFindMatchCount = nil
        if page?.selection != nil { selectedRange = nil }
        coordinator.clearFind()
    }
    func moveHit(backwards: Bool) {
        guard canMoveHit, let report = findReport else { return }
        let count = report.hits.count
        let index: Int
        if let selected = selectedHitIndex {
            if let start = report.pageStartAfter, coordinator.supportsFindPages(for: report.options.mode) {
                if backwards && selected == 0 {
                    if start > 0 {
                        loadFindPage(startAfter: start > report.retainedLimit ? start - report.retainedLimit : 0,
                                     backwards: true)
                        return
                    }
                    if let total = knownFindMatchCount, total > UInt64(count) {
                        let lastPage = ((total - 1) / report.retainedLimit) * report.retainedLimit
                        loadFindPage(startAfter: lastPage, backwards: true)
                        return
                    }
                    // Until EOF establishes the total, wrapping would silently
                    // treat this bounded page as the complete result set.
                    if report.hasMoreMatches || !report.complete {
                        notice = "At the first match. Continue forward to reach later matches."
                        return
                    }
                } else if !backwards && selected == count - 1 {
                    if report.hasMoreMatches, let last = report.hits.last {
                        loadFindPage(startAfter: last.occurrence, backwards: false)
                        return
                    }
                    guard report.complete else {
                        notice = report.summary
                        return
                    }
                    if start > 0 {
                        loadFindPage(startAfter: 0, backwards: false)
                        return
                    }
                }
            }
            index = backwards ? (selected == 0 ? count - 1 : selected - 1) : (selected == count - 1 ? 0 : selected + 1)
        } else { index = backwards ? count - 1 : 0 }
        guard let target = report.target(at: index) else { return }
        navigate(.hit(target), hitIndex: index)
    }

    /// One retained result page at a time. Replaying the prefix spends the
    /// normal scan budget and uses a fresh generation; source bytes stay fixed.
    private func loadFindPage(startAfter: UInt64, backwards: Bool) {
        guard canMoveHit, coordinator.supportsFindPages(for: findOptions.mode) else { return }
        let needle = fileQuery
        let options = findOptions
        // A host may accept a new generation before Swift rejects its receipt.
        // Revoke old rows now, including on cancellation or decode failure.
        findReport = nil; selectedHitIndex = nil; selectedRange = nil
        coordinator.clearFind()
        let ticket = begin(query: true)
        notice = "Loading more matches from the retained file…"
        task = Task {
            do {
                let report = try await coordinator.find(needle, options: options, startAfter: startAfter)
                guard operation == ticket, !Task.isCancelled, fileQuery.utf8.elementsEqual(needle.utf8),
                      findOptions == options else { return }
                findReport = report
                if report.complete { knownFindMatchCount = report.matchesSeen }
                finish(ticket)
                notice = report.summary
                if !report.hits.isEmpty { moveHit(backwards: backwards) }
            } catch { fail(error, ticket: ticket) }
        }
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
        let window = supportsReferenceWindows
            ? AtlasReferenceWindowRequest(offset: 0, countAllMatches: true) : nil
        loadReferenceWindow(name: name, request: window)
    }
    func nextReferenceWindow() {
        guard canReferenceWindowNext, let inventory = referencePage?.inventory,
              let window = inventory.window, let offset = window.nextOffset else { return }
        loadReferenceWindow(name: inventory.name,
            request: .init(offset: offset, countAllMatches: window.request.countAllMatches))
    }
    func previousReferenceWindow() {
        guard canReferenceWindowBack, let inventory = referencePage?.inventory,
              let window = inventory.window else { return }
        let offset = window.request.offset > inventory.maxItems ? window.request.offset - inventory.maxItems : 0
        loadReferenceWindow(name: inventory.name,
            request: .init(offset: offset, countAllMatches: window.request.countAllMatches))
    }
    private func loadReferenceWindow(name: String, request: AtlasReferenceWindowRequest?) {
        // Preserve the displayed source and prior rows while replacing the
        // inventory. A response lost after foreign acceptance cannot safely
        // restore the old generation's activation authority.
        referenceRowsAuthorized = false
        let ticket = begin(references: true)
        notice = "Finding whole-identifier text occurrences in this capture…"
        task = Task {
            do {
                let result = try await coordinator.prepareReferences(name, window: request)
                guard operation == ticket, !Task.isCancelled,
                      referenceName.utf8.elementsEqual(name.utf8) else { return }
                referencePage = result
                referenceHistory = []
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
        if !delimiterWork { suspendTextSelection() }
        cancelPin()
        let wasRefreshing = isRefreshing
        documentPreview.cancel(silent: true)
        operation = UUID()
        task?.cancel(); task = nil
        coordinator.cancel()
        if page == nil {
            coordinator.close()
            refreshInput = nil; refreshImageSource = nil; refreshAccessLease = nil
        }
        busy = false; isRefreshing = false
        queryWork = false
        outlineWork = false
        referenceWork = false
        positionWork = false
        delimiterWork = false
        notice = wasRefreshing ? "Refresh canceled. The previous retained source is unchanged."
            : (page == nil ? "Source opening canceled." : "Navigation canceled. The current retained page is unchanged.")
        resumeDeferredReaderWork()
    }
    func close() {
        previewSourceFollowing = false
        clearTextSelection()
        cancelPin()
        documentPreview.close()
        operation = UUID()
        task?.cancel(); task = nil
        coordinator.close()
        refreshInput = nil; refreshImageSource = nil; refreshAccessLease = nil
        isRefreshing = false; refreshedFromDisk = false; forkedCapture = false
        retainedOnly = false
        page = nil; source = nil; history.reset(); comparisonSelection = nil; bracketSelection = nil
        findReport = nil; selectedHitIndex = nil; selectedRange = nil; knownFindMatchCount = nil
        queryWork = false; outlineWork = false; referenceWork = false; positionWork = false; delimiterWork = false
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
    private func begin(query: Bool = false, outline: Bool = false, references: Bool = false,
                       position: Bool = false, delimiter: Bool = false) -> UUID {
        if !delimiter { suspendTextSelection() }
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
        delimiterWork = delimiter
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
        clearTextSelection()
        comparisonSelection = nil
        bracketSelection = nil
        self.page = page
        // Search/outline/occurrence windows include context before their exact
        // target. Follow that verified original anchor, not the context start
        // and never the native UTF-16 selection or a rendered-flow coordinate.
        let previewAnchor = page.position?.originalOffset ?? page.selection?.target.start
            ?? page.symbolSelection?.target.symbol.selectedRange.start
            ?? page.referenceSelection?.target.occurrence.range.start ?? page.start
        documentPreview.bind(identity: page.identity, sourceOffset: previewAnchor)
        documentPreview.setSourceFollowing(previewSourceFollowing)
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
        busy = false; isRefreshing = false; task = nil; queryWork = false; outlineWork = false; referenceWork = false; positionWork = false; delimiterWork = false
        resumeDeferredReaderWork()
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
