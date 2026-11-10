import Foundation
import Observation

/// One private captured-index owner per documentation surface. Preparation
/// uses the production search builder; graph operations never read live files.
@Observable @MainActor final class AtlasProjectLinksModel {
    let root: String
    private(set) var page: AtlasProjectLinksPage?
    private(set) var captureProgress: AtlasSearchReport?
    private(set) var selectedPath: [UInt8]?
    private(set) var direction: AtlasProjectLinkDirection
    private(set) var busy = false
    private(set) var closed = false
    private(set) var notice = "Capture project sources and build documentation links when ready."
    var canOperate: Bool { !busy && !closed }
    var hasPreparedSources: Bool { prepared != nil }
    var selectedPathDisplay: String {
        selectedPath.flatMap { try? AtlasProjectLinkPath(bytes: $0).display } ?? "Whole project"
    }
    @ObservationIgnored private let transport: AtlasProjectLinksTransport
    @ObservationIgnored private let access: AtlasSearchAccessLease
    @ObservationIgnored private let io = AtlasProjectIO()
    @ObservationIgnored private var indexWorker: AtlasIndexedSearch?
    @ObservationIgnored private var prepared: AtlasPreparedIndex?
    @ObservationIgnored private var lastGeneration: UInt64 = 0
    @ObservationIgnored private var cancellation: AtlasSearchCancellation?
    @ObservationIgnored private var task: Task<Void, Never>?

    init(root: String, initialPath: [UInt8]? = nil, direction: AtlasProjectLinkDirection = .all,
         access: AnyObject? = nil, transport: AtlasProjectLinksTransport) {
        self.root = root; self.transport = transport; self.access = AtlasSearchAccessLease(access)
        indexWorker = AtlasIndexedSearch(transport: transport.index)
        selectedPath = initialPath.flatMap { AtlasProjectLinkPath.valid($0) ? $0 : nil }
        self.direction = selectedPath == nil ? .all : direction
    }

    func prepare(refresh: Bool = false) {
        guard canOperate else { return }
        if refresh {
            prepared = nil; lastGeneration = 0
            indexWorker = AtlasIndexedSearch(transport: transport.index)
        }
        let (generation, exhausted) = lastGeneration.addingReportingOverflow(1)
        guard !exhausted else { notice = "The documentation-link identity is exhausted. Close this browser and open it again."; return }
        lastGeneration = generation
        // A replacement graph never lends an earlier row its new generation.
        page = nil; captureProgress = nil
        notice = prepared == nil ? "Capturing project sources within the existing file and byte limits…"
            : "Rebuilding links from the retained source index without rereading files…"
        perform { model, flag in
            guard let worker = model.indexWorker else { throw AtlasProjectLinksError.unavailable }
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
            model.prepared = index; model.captureProgress = nil
            let transport = model.transport
            var current = try await model.io.perform(cancellation: flag, keepingAlive: [index.owner]) {
                try AtlasProjectLinksPage.decode(index.owner.call {
                    try transport.begin($0, generation, index.basis.index, flag)
                }, index: index, generation: generation, command: "links-begin")
            }
            guard current.coverage.steps == 0, current.coverage.lastStepFiles == 0,
                  current.coverage.examined == 0, current.coverage.parseBytes == 0,
                  current.coverage.retained == 0, current.coverage.linksSeen == 0,
                  current.coverage.diagnostics == 0, current.building else { throw AtlasProjectLinksError.invalidResponse }
            try model.accept(flag)
            model.page = current
            while current.building {
                let previous = current.coverage
                let next = try await model.io.perform(cancellation: flag, keepingAlive: [index.owner]) {
                    let next = try AtlasProjectLinksPage.decode(index.owner.call {
                        try transport.step($0, generation, flag)
                    }, index: index, generation: generation, command: "links-step")
                    try previous.validateSuccessor(next.coverage)
                    return next
                }
                try model.accept(flag)
                current = next; model.page = next
            }
            // Build receipts use All; apply the user's exact path only after a
            // terminal graph has been accepted. Missing paths are explicit errors.
            if model.selectedPath != nil || model.direction != .all {
                let direction = model.direction, path = model.selectedPath
                do {
                    let filtered = try await model.fetch(index, generation: generation, coverage: current.coverage,
                        direction: direction, path: path, start: 0, flag: flag)
                    try model.accept(flag)
                    model.page = filtered
                } catch {
                    // The accepted graph remains inspectable even when a path
                    // belongs outside this bounded catalog. Do not claim zero.
                    model.selectedPath = nil; model.direction = .all
                    throw error
                }
            }
            model.notice = model.page?.summary ?? "Documentation links are ready."
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
        guard direction == .all || selectedPath != nil else { notice = "Choose an exact file path for outgoing links or backlinks."; return }
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
            model.page = next; model.direction = direction; model.selectedPath = path
            model.notice = "Showing \(next.total) retained links in this exact file and direction filter. " + next.summary
        }
    }
    func loadPage(start: UInt64) {
        guard canOperate, let page, let prepared, !page.building, start <= page.total else { return }
        perform { model, flag in
            let next = try await model.fetch(prepared, generation: page.generation, coverage: page.coverage,
                direction: page.direction, path: page.path?.bytes, start: start, flag: flag)
            try model.accept(flag)
            model.page = next
            model.notice = "Showing retained links \(start)..<\(start + UInt64(next.links.count)) of \(next.total). No source files were reread."
        }
    }
    private func fetch(_ index: AtlasPreparedIndex, generation: UInt64, coverage: AtlasProjectLinksCoverage,
        direction: AtlasProjectLinkDirection, path: [UInt8]?, start: UInt64,
        flag: AtlasSearchCancellation) async throws -> AtlasProjectLinksPage {
        let transport = transport
        let hex = try path.map { try AtlasProjectLinkPath(bytes: $0).hex }
        return try await io.perform(cancellation: flag, keepingAlive: [index.owner]) {
            let next = try AtlasProjectLinksPage.decode(index.owner.call {
                try transport.page($0, generation, direction, hex, start, 64)
            }, index: index, generation: generation, command: "links-page", direction: direction,
                path: path, start: start, limit: 64)
            guard next.coverage == coverage else { throw AtlasProjectLinksError.invalidResponse }
            return next
        }
    }

    func open(_ link: AtlasProjectLink, from page: AtlasProjectLinksPage, side: AtlasProjectLinkSide,
        readerTransport: AtlasReaderTransport, search: AtlasReaderSearchTransport? = nil,
        outline: AtlasReaderOutlineTransport? = nil,
        document: AtlasReaderDocumentTransport? = nil) async throws -> AtlasProjectLinkReaderCapture {
        guard canOperate, let accepted = self.page, accepted.id == page.id, !accepted.building,
              accepted.links.contains(link), let prepared, prepared.basis == page.basis,
              let domain = transport.readerDomain, readerTransport.comparison?.domain == domain,
              side == .evidence || link.canOpenTarget else { throw AtlasProjectLinksError.invalidInput }
        let file = side == .evidence ? link.source : link.target!
        let flag = AtlasSearchCancellation(), transport = transport
        cancellation = flag; busy = true
        let reader = AtlasPagedReaderModel(transport: readerTransport, search: search, outline: outline, document: document)
        defer { finish() }
        do {
            try await withTaskCancellationHandler(operation: {
                try await reader.openRetained(offset: 0, registryDomain: domain) { destination in
                    guard !flag.isCanceled else { throw AtlasProjectLinksError.canceled }
                    return try prepared.owner.call { handle in
                        let json = try transport.openReader(handle, destination, page.generation, link.id, side)
                        return try AtlasProjectLinkReaderReceipt.decode(json, index: prepared, generation: page.generation,
                            link: link, side: side, destination: destination)
                    }
                }
                try accept(flag)
                guard self.page?.id == page.id, let initial = reader.page,
                      initial.identity.pathBytes == file.path.bytes,
                      initial.identity.capturedBytes == file.capturedBytes else { throw AtlasProjectLinksError.invalidResponse }
            }, onCancel: { flag.cancel() })
            notice = "Opened the exact retained \(side == .evidence ? "Markdown source" : "target") document. No live file was reopened."
            return AtlasProjectLinkReaderCapture(reader: reader, owner: prepared.basis.owner, path: file.path, link: link, side: side)
        } catch {
            reader.close(); notice = Self.message(error); throw error
        }
    }

    func cancel() {
        guard busy else { return }
        cancellation?.cancel(); task?.cancel(); io.cancel()
        notice = "Cancel requested. Waiting for entered capture or documentation work to drain."
    }
    func close() {
        guard !closed else { return }
        closed = true; cancel(); page = nil; captureProgress = nil
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
                // An unfinished graph cannot authorize later row activation.
                if page?.building == true { page = nil }
            }
            finish()
        }
    }
    private func finish() {
        busy = false; cancellation = nil; task = nil; captureProgress = nil
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
