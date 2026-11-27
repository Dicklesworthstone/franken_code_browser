import Foundation
import Observation

/// One private captured-index owner with independent relationship inventories. Preparation
/// uses the production search builder; graph operations never read live files.
@Observable @MainActor final class AtlasProjectLinksModel {
    let root: String
    private(set) var page: AtlasProjectLinksPage?
    private(set) var captureProgress: AtlasSearchReport?
    /// Candidate receipts report progress without replacing accepted rows or
    /// lending their generation to the displayed relationship inventory.
    private(set) var relationshipProgress: AtlasProjectLinksPage?
    private(set) var selectedPath: [UInt8]?
    private(set) var direction: AtlasProjectLinkDirection
    private(set) var projection: AtlasProjectRelationshipProjection
    private(set) var busy = false
    private(set) var closed = false
    private(set) var notice = "Capture project sources and build relationships when ready."
    var availableProjections: [AtlasProjectRelationshipProjection] { transport.imports == nil ? [.documentation] : [.documentation, .imports] }
    var canOperate: Bool { !busy && !closed }
    var hasPreparedSources: Bool { prepared != nil }
    var hasAcceptedProjection: Bool {
        guard let prepared, let accepted = acceptedPages[projection] else { return false }
        return accepted.basis == prepared.basis
    }
    var selectedPathDisplay: String {
        selectedPath.flatMap { try? AtlasProjectLinkPath(bytes: $0).display } ?? "Whole project"
    }
    private struct Scope: Equatable {
        let basis: AtlasIndexedBasis
        let generation: UInt64
        let projection: AtlasProjectRelationshipProjection
        let direction: AtlasProjectLinkDirection
        let path: [UInt8]?
        let filterFile: UInt64?
        let total: UInt64

        init(_ page: AtlasProjectLinksPage) {
            basis = page.basis; generation = page.generation; projection = page.projection
            direction = page.direction; path = page.path?.bytes; filterFile = page.filterFile
            total = page.total
        }
    }

    private struct Location: Equatable {
        let scope: Scope
        let start: UInt64
        // Wire-byte limits can shorten a page. Only accepted offsets can
        // restore Previous on a later page; subtracting 64 loses evidence.
        let previousStarts: [UInt64]
    }

    private var locations = AtlasNavigationHistory<Location>(capacity: 32)
    var canGoBack: Bool { canOperate && locations.canBack }
    var canGoForward: Bool { canOperate && locations.canForward }
    var previousPageStart: UInt64? { locations.current?.previousStarts.last }

    @ObservationIgnored private let transport: AtlasProjectLinksTransport
    @ObservationIgnored private let access: AtlasSearchAccessLease
    @ObservationIgnored private let io = AtlasProjectIO()
    @ObservationIgnored private var indexWorker: AtlasIndexedSearch?
    @ObservationIgnored private var prepared: AtlasPreparedIndex?
    @ObservationIgnored private var lastGenerations: [AtlasProjectRelationshipProjection: UInt64] = [:]
    @ObservationIgnored private var acceptedPages: [AtlasProjectRelationshipProjection: AtlasProjectLinksPage] = [:]
    @ObservationIgnored private var cancellation: AtlasSearchCancellation?
    @ObservationIgnored private var task: Task<Void, Never>?

    init(root: String, initialPath: [UInt8]? = nil, direction: AtlasProjectLinkDirection = .all,
         projection: AtlasProjectRelationshipProjection = .documentation,
         access: AnyObject? = nil, transport: AtlasProjectLinksTransport) {
        self.root = root; self.transport = transport; self.access = AtlasSearchAccessLease(access)
        self.projection = projection
        indexWorker = AtlasIndexedSearch(transport: transport.index)
        let selectedPath = initialPath.flatMap { AtlasProjectLinkPath.valid($0) ? $0 : nil }
        self.selectedPath = selectedPath
        self.direction = selectedPath == nil ? .all : direction
    }

    func prepare(refresh: Bool = false) {
        prepare(projection: projection, refresh: refresh)
    }

    private func prepare(projection: AtlasProjectRelationshipProjection, refresh: Bool) {
        guard canOperate else { return }
        guard availableProjections.contains(projection) else { notice = AtlasProjectLinksError.unavailable.message; return }
        if !refresh, let retained = acceptedPages[projection], let prepared,
           retained.basis == prepared.basis {
            // Source bytes and extraction policy are immutable. Reload the
            // accepted inventory instead of destroying its backend generation
            // with an identical rebuild whose terminal receipt could fail.
            reload(projection: projection, retained: retained, index: prepared)
            return
        }
        let candidateWorker = refresh ? AtlasIndexedSearch(transport: transport.index) : indexWorker
        guard let worker = candidateWorker else {
            notice = AtlasProjectLinksError.unavailable.message; return
        }
        let (generation, exhausted) = (refresh ? 0 : (lastGenerations[projection] ?? 0)).addingReportingOverflow(1)
        guard !exhausted else { notice = "The relationship identity is exhausted. Close this browser and open it again."; return }
        // A fresh capture has a fresh owner. Failed attempts against the
        // current owner still consume their own generation before admission.
        if !refresh { lastGenerations[projection] = generation }
        let direction = direction, path = selectedPath
        captureProgress = nil; relationshipProgress = nil
        notice = refresh || prepared == nil ? "Capturing project sources within the existing file and byte limits…"
            : "Building \(projection.noun) from the retained source index without rereading files…"
        perform { model, flag in
            let root = model.root, access = model.access
            let mailbox = AtlasSearchProgressMailbox { [weak model] report in
                guard let model, !model.closed, model.cancellation === flag, !flag.isCanceled else { return }
                model.captureProgress = report
            }
            let index: AtlasPreparedIndex
            do {
                index = try await model.io.perform(cancellation: flag, keepingAlive: [worker, access]) {
                    defer { mailbox.close() }
                    return try worker.prepare(root: root, cancellation: flag, access: access, progress: { mailbox.offer($0) })
                }
            } catch { mailbox.close(); throw error }
            try model.accept(flag)
            // An initial completed capture remains useful after a canceled
            // graph build. A refresh candidate never replaces the old owner
            // until its terminal inventory and selected filter both validate.
            if !refresh && model.prepared == nil { model.prepared = index }
            model.captureProgress = nil
            let operations = try model.transport.operations(for: projection)
            var current = try await model.io.perform(cancellation: flag, keepingAlive: [index.owner]) {
                try AtlasProjectLinksPage.decode(index.owner.call {
                    try operations.begin($0, generation, index.basis.index, flag)
                }, index: index, generation: generation, command: projection.command("begin"), projection: projection)
            }
            guard current.coverage.steps == 0, current.coverage.lastStepFiles == 0,
                  current.coverage.examined == 0, current.coverage.parseBytes == 0,
                  current.coverage.retained == 0, current.coverage.linksSeen == 0,
                  current.coverage.diagnostics == 0, current.building else { throw AtlasProjectLinksError.invalidResponse }
            try model.accept(flag)
            model.relationshipProgress = current
            while current.building {
                let previous = current.coverage
                let next = try await model.io.perform(cancellation: flag, keepingAlive: [index.owner]) {
                    let next = try AtlasProjectLinksPage.decode(index.owner.call {
                        try operations.step($0, generation, flag)
                    }, index: index, generation: generation, command: projection.command("step"), projection: projection)
                    try previous.validateSuccessor(next.coverage)
                    return next
                }
                try model.accept(flag)
                current = next; model.relationshipProgress = next
            }
            // A missing selected path is a failed replacement, not permission
            // to widen the user's filter or replace it with an empty result.
            if path != nil || direction != .all {
                current = try await model.fetch(index, generation: generation, coverage: current.coverage,
                    direction: direction, path: path, start: 0, flag: flag)
            }
            try model.accept(flag)
            // One main-actor transaction publishes the exact owner, namespace,
            // projection and filter represented by the newly accepted page.
            if refresh {
                model.acceptedPages = [:]
                model.lastGenerations = [projection: generation]
            }
            model.indexWorker = worker; model.prepared = index
            model.record(current, refreshed: refresh)
            model.relationshipProgress = nil
            model.notice = current.summary
        }
    }

    /// Switching projection reuses the frozen source index and its other
    /// accepted inventory. Keep the displayed projection until a new page has
    /// validated: a failed switch must not strand the user's working browser.
    func selectProjection(_ projection: AtlasProjectRelationshipProjection) {
        guard canOperate, projection != self.projection, availableProjections.contains(projection) else { return }
        prepare(projection: projection, refresh: false)
    }

    private func reload(projection: AtlasProjectRelationshipProjection,
                        retained: AtlasProjectLinksPage, index: AtlasPreparedIndex) {
        let direction = direction, path = selectedPath
        let start = page?.projection == projection && page?.direction == direction
            && page?.path?.bytes == path ? (page?.start ?? 0) : 0
        notice = "Reloading the retained \(projection.noun) view without recapturing or reparsing sources…"
        perform { model, flag in
            let next = try await model.fetch(index, generation: retained.generation, coverage: retained.coverage,
                direction: direction, path: path, start: start, flag: flag)
            try model.accept(flag)
            model.record(next)
            model.notice = "Retained view reloaded. Use Refresh sources to include file edits. " + next.summary
        }
    }

    func selectPath(_ path: [UInt8]?, direction requestedDirection: AtlasProjectLinkDirection? = nil) {
        guard canOperate else { return }
        guard path == nil || AtlasProjectLinkPath.valid(path!) else { notice = AtlasProjectLinksError.invalidInput.message; return }
        let requested = path == nil ? .all : (requestedDirection ?? (direction == .all ? .outgoing : direction))
        changeFilter(direction: requested, path: path)
    }
    func filter(direction: AtlasProjectLinkDirection) {
        guard canOperate else { return }
        guard direction == .all || selectedPath != nil else { notice = "Choose an exact file path for outgoing or incoming relationships."; return }
        changeFilter(direction: direction, path: selectedPath)
    }
    private func changeFilter(direction: AtlasProjectLinkDirection, path: [UInt8]?) {
        guard let page, let prepared, !page.building else {
            self.direction = direction; selectedPath = path; return
        }
        perform { model, flag in
            let next = try await model.fetch(prepared, generation: page.generation, coverage: page.coverage,
                direction: direction, path: path, start: 0, flag: flag)
            try model.accept(flag)
            model.record(next)
            model.notice = "Showing \(next.total) retained \(next.projection.noun) in this exact file and direction filter. " + next.summary
        }
    }
    func loadPage(start: UInt64) {
        guard canOperate, let page, let prepared, !page.building, start <= page.total else { return }
        perform { model, flag in
            let next = try await model.fetch(prepared, generation: page.generation, coverage: page.coverage,
                direction: page.direction, path: page.path?.bytes, start: start, flag: flag)
            try model.accept(flag)
            model.record(next)
            model.notice = "Showing retained relationships \(start)..<\(start + UInt64(next.links.count)) of \(next.total). No source files were reread."
        }
    }

    /// Only the accepted filter, direction or projection adds a journey step.
    /// Paging within the same location updates its anchor and preserves Forward.
    private func record(_ next: AtlasProjectLinksPage, refreshed: Bool = false) {
        let scope = Scope(next)
        if !refreshed, let current = locations.current, current.scope == scope {
            var previous = current.previousStarts
            if next.start > current.start {
                previous.append(current.start)
            } else if next.start < current.start {
                previous = Array(previous.prefix { $0 < next.start })
            }
            locations.replaceCurrent(Location(scope: scope, start: next.start, previousStarts: previous))
        } else {
            let location = Location(scope: scope, start: next.start, previousStarts: [])
            if refreshed {
                locations.reset(to: location)
            } else {
                let move = locations.visit(location)
                locations.commit(move, resolved: location)
            }
        }
        display(next)
    }

    private func display(_ next: AtlasProjectLinksPage) {
        acceptedPages[next.projection] = next
        projection = next.projection; direction = next.direction; selectedPath = next.path?.bytes
        page = next
    }

    func goBack() { if canGoBack, let move = locations.back() { restore(move) } }
    func goForward() { if canGoForward, let move = locations.forward() { restore(move) } }

    private func restore(_ move: AtlasNavigationHistory<Location>.Move) {
        let target = move.target, scope = target.scope
        guard let index = prepared, index.basis == scope.basis,
              let retained = acceptedPages[scope.projection], retained.basis == scope.basis,
              retained.generation == scope.generation else { return }
        perform { model, flag in
            let next = try await model.fetch(index, generation: scope.generation, coverage: retained.coverage,
                direction: scope.direction, path: scope.path, start: target.start, flag: flag)
            try model.accept(flag)
            guard Scope(next) == scope, model.locations.commit(move, resolved: target) else {
                throw AtlasProjectLinksError.invalidResponse
            }
            model.display(next)
            model.notice = next.summary
        }
    }

    private func fetch(_ index: AtlasPreparedIndex, generation: UInt64, coverage: AtlasProjectLinksCoverage,
        direction: AtlasProjectLinkDirection, path: [UInt8]?, start: UInt64,
        flag: AtlasSearchCancellation) async throws -> AtlasProjectLinksPage {
        let projection = coverage.projection
        let operations = try transport.operations(for: projection)
        let hex = try path.map { try AtlasProjectLinkPath(bytes: $0).hex }
        return try await io.perform(cancellation: flag, keepingAlive: [index.owner]) {
            let next = try AtlasProjectLinksPage.decode(index.owner.call {
                try operations.page($0, generation, direction, hex, start, 64)
            }, index: index, generation: generation, command: projection.command("page"), direction: direction,
                path: path, start: start, limit: 64, projection: projection)
            guard next.coverage == coverage else { throw AtlasProjectLinksError.invalidResponse }
            return next
        }
    }

    func open(_ link: AtlasProjectLink, from page: AtlasProjectLinksPage, side: AtlasProjectLinkSide,
        readerTransport: AtlasReaderTransport, search: AtlasReaderSearchTransport? = nil,
        outline: AtlasReaderOutlineTransport? = nil,
        document: AtlasReaderDocumentTransport? = nil) async throws -> AtlasProjectLinkReaderCapture {
        guard canOperate, let accepted = self.page, accepted.id == page.id, !accepted.building,
              page.projection == projection, link.projection == projection,
              accepted.links.contains(link) else { throw AtlasProjectLinksError.invalidInput }
        return try await openRetainedLink(link, from: page, side: side, readerTransport: readerTransport,
            search: search, outline: outline, document: document)
    }

    /// Both the visible Inspector and a verified preview occurrence enter the
    /// same captured-reader transfer. A hidden outgoing page cannot replace the
    /// user's visible filter, projection, or pagination position.
    private func openRetainedLink(_ link: AtlasProjectLink, from page: AtlasProjectLinksPage,
        side: AtlasProjectLinkSide, readerTransport: AtlasReaderTransport,
        search: AtlasReaderSearchTransport?, outline: AtlasReaderOutlineTransport?,
        document: AtlasReaderDocumentTransport?) async throws -> AtlasProjectLinkReaderCapture {
        guard canOperate, let accepted = acceptedPages[page.projection], !accepted.building,
              accepted.basis == page.basis, accepted.generation == page.generation,
              accepted.coverage == page.coverage, page.links.contains(link),
              let prepared, prepared.basis == page.basis,
              let domain = transport.readerDomain, readerTransport.comparison?.domain == domain,
              side == .evidence || link.canOpenTarget else { throw AtlasProjectLinksError.invalidInput }
        let file = side == .evidence ? link.source : link.target!
        let flag = AtlasSearchCancellation(), operations = try transport.operations(for: page.projection)
        cancellation = flag; busy = true
        let reader = AtlasPagedReaderModel(transport: readerTransport, search: search, outline: outline, document: document)
        let receipt = AtlasProjectLinkReaderReceiptBox()
        defer { finish() }
        do {
            try await withTaskCancellationHandler(operation: {
                try await reader.openRetained(registryDomain: domain) { destination in
                    guard !flag.isCanceled else { throw AtlasProjectLinksError.canceled }
                    return try prepared.owner.call { handle in
                        let json = try operations.openReader(handle, destination, page.generation, link.id, side)
                        let admitted = try AtlasProjectLinkReaderReceipt.decode(json, index: prepared, generation: page.generation,
                            link: link, side: side, destination: destination)
                        try receipt.set(admitted)
                        return AtlasReaderRetainedInitialization(info: admitted.info,
                            initialOriginalOffset: admitted.initialOriginalOffset)
                    }
                }
                try accept(flag)
                guard self.prepared?.basis == prepared.basis,
                      self.acceptedPages[page.projection]?.generation == page.generation,
                      let initial = reader.page,
                      initial.identity.pathBytes == file.path.bytes,
                      initial.identity.capturedBytes == file.capturedBytes else { throw AtlasProjectLinksError.invalidResponse }
            }, onCancel: { flag.cancel() })
            let accepted = try receipt.accepted()
            notice = accepted.navigationDescription
            return AtlasProjectLinkReaderCapture(reader: reader, basis: prepared.basis, path: file.path,
                link: link, side: side, fragment: accepted.fragment, navigationDescription: accepted.navigationDescription)
        } catch {
            reader.close(); notice = Self.message(error); throw error
        }
    }

    func canFollowDocumentLinks(from source: AtlasProjectLinkReaderCapture) -> Bool {
        canOperate && ownsDocumentSource(source)
            && acceptedPages[.documentation].map({ !$0.building && $0.basis == source.basis }) == true
    }

    private func ownsDocumentSource(_ source: AtlasProjectLinkReaderCapture) -> Bool {
        guard !closed, prepared?.basis == source.basis, source.owner == source.basis.owner,
              let domain = transport.readerDomain, source.reader.retainedRegistryDomain == domain,
              source.reader.retainedCaptureSHA256 == source.file.digest,
              let identity = source.reader.page?.identity,
              identity.pathBytes == source.file.path.bytes,
              identity.capturedBytes == source.file.capturedBytes else { return false }
        return true
    }

    private func ownsDocumentLink(_ target: AtlasReaderDocumentLinkTarget,
        from source: AtlasProjectLinkReaderCapture) -> Bool {
        ownsDocumentSource(source) && target.link.kind == .local
            && source.reader.page?.identity.matches(target.inventory.identity) == true
            && source.reader.documentPreview.allows(target)
            && target.link.evidence.start == 0 && target.link.evidence.end == source.file.capturedBytes
    }

    /// Resolve the exact FMD occurrence in retained metadata, then open the
    /// captured target with its qualified heading/line anchor. Path spelling
    /// alone never authorizes navigation or a working-tree reopen.
    func openDocumentLink(_ target: AtlasReaderDocumentLinkTarget, from source: AtlasProjectLinkReaderCapture,
        readerTransport: AtlasReaderTransport, search: AtlasReaderSearchTransport? = nil,
        outline: AtlasReaderOutlineTransport? = nil,
        document: AtlasReaderDocumentTransport? = nil) async throws -> AtlasProjectLinkReaderCapture {
        guard canFollowDocumentLinks(from: source), ownsDocumentLink(target, from: source),
              readerTransport.comparison?.domain == transport.readerDomain else {
            throw AtlasProjectLinksError.staleDocumentLink
        }
        do {
            let (link, page) = try await resolveDocumentLink(target, from: source)
            try Task.checkCancellation()
            guard canFollowDocumentLinks(from: source), ownsDocumentLink(target, from: source) else {
                throw AtlasProjectLinksError.staleDocumentLink
            }
            let opened = try await openRetainedLink(link, from: page, side: .target,
                readerTransport: readerTransport, search: search, outline: outline, document: document)
            guard !Task.isCancelled, ownsDocumentLink(target, from: source),
                  acceptedPages[.documentation]?.generation == page.generation else {
                opened.reader.close()
                throw AtlasProjectLinksError.staleDocumentLink
            }
            notice = "Opened the linked capture. Back restores the previous retained reader."
            return opened
        } catch {
            notice = Self.message(error)
            throw error
        }
    }

    private func resolveDocumentLink(_ target: AtlasReaderDocumentLinkTarget,
        from source: AtlasProjectLinkReaderCapture) async throws -> (AtlasProjectLink, AtlasProjectLinksPage) {
        guard canOperate, ownsDocumentLink(target, from: source), let index = prepared,
              let inventory = acceptedPages[.documentation], !inventory.building,
              inventory.basis == source.basis else { throw AtlasProjectLinksError.staleDocumentLink }
        let flag = AtlasSearchCancellation()
        cancellation = flag; busy = true
        defer { finish() }
        return try await withTaskCancellationHandler(operation: {
            var first: UInt64 = 0, lastOccurrence: UInt64 = 0, lastLink: UInt64 = 0
            var total: UInt64?
            while true {
                try accept(flag)
                guard ownsDocumentLink(target, from: source),
                      acceptedPages[.documentation]?.generation == inventory.generation else {
                    throw AtlasProjectLinksError.staleDocumentLink
                }
                let next = try await fetch(index, generation: inventory.generation,
                    coverage: inventory.coverage, direction: .outgoing, path: source.file.path.bytes,
                    start: first, flag: flag)
                try accept(flag)
                guard ownsDocumentLink(target, from: source), next.filterFile == source.file.id,
                      total == nil || total == next.total else { throw AtlasProjectLinksError.invalidResponse }
                total = next.total
                for link in next.links {
                    guard link.source == source.file, link.sourceLink > lastOccurrence, link.id > lastLink else {
                        throw AtlasProjectLinksError.invalidResponse
                    }
                    lastOccurrence = link.sourceLink; lastLink = link.id
                    if link.sourceLink == target.link.id {
                        guard link.block == target.link.blockIndex,
                              link.evidenceRange.start == target.link.evidence.start,
                              link.evidenceRange.end == target.link.evidence.end,
                              link.destination.utf8.elementsEqual(target.link.destination.utf8) else {
                            throw AtlasProjectLinksError.invalidResponse
                        }
                        guard link.canOpenTarget else { throw AtlasProjectLinksError.unavailableDocumentTarget }
                        return (link, next)
                    }
                }
                guard let offset = next.next else { throw AtlasProjectLinksError.unindexedDocumentLink }
                // The wire proves strict progress and at most 4096 entries.
                first = offset
            }
        }, onCancel: { flag.cancel() })
    }

    func cancel() {
        guard busy else { return }
        cancellation?.cancel(); task?.cancel(); io.cancel()
        notice = "Cancel requested. Waiting for entered capture or relationship work to drain."
    }
    func close() {
        guard !closed else { return }
        closed = true; cancel(); page = nil; captureProgress = nil; relationshipProgress = nil; acceptedPages = [:]
        locations.reset()
        if !busy { prepared = nil; indexWorker = nil }
    }
    private func accept(_ flag: AtlasSearchCancellation) throws {
        guard !closed, !flag.isCanceled, !Task.isCancelled, cancellation === flag else { throw AtlasProjectLinksError.canceled }
    }
    private func perform(_ operation: @escaping @MainActor (AtlasProjectLinksModel, AtlasSearchCancellation) async throws -> Void) {
        guard canOperate else { return }
        let flag = AtlasSearchCancellation()
        cancellation = flag; busy = true
        task = Task {
            do { try await operation(self, flag) }
            catch {
                notice = Self.message(error)
                if page != nil { notice += " Your previous relationship view is unchanged." }
            }
            finish()
        }
    }
    private func finish() {
        busy = false; cancellation = nil; task = nil; captureProgress = nil; relationshipProgress = nil
        if closed { prepared = nil; indexWorker = nil }
    }
    private static func message(_ error: Error) -> String {
        if let error = error as? AtlasProjectLinksError { return error.message }
        if let error = error as? AtlasSearchError { return error.message }
        if let error = error as? AtlasReaderError { return error.message }
        if error is CancellationError { return AtlasProjectLinksError.canceled.message }
        if let error = error as? AtlasProjectIOError, case .canceled = error { return AtlasProjectLinksError.canceled.message }
        return AtlasProjectLinksError.invalidResponse.message
    }
}
