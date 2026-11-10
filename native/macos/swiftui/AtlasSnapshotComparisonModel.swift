import Foundation
import Observation
import Dispatch

/// A selected archive grant is retained, never operated on, by worker code.
/// Both descriptors and both grants retire together after the final call.
private final class AtlasSnapshotComparisonAccess: @unchecked Sendable {
    let before, after: AnyObject?
    init(before: AnyObject?, after: AnyObject?) { self.before = before; self.after = after }
}
private final class AtlasSnapshotComparisonSession: @unchecked Sendable {
    let handle: UInt64
    let transport: AtlasSnapshotComparisonTransport
    private let access: AtlasSnapshotComparisonAccess
    private static let retirement = DispatchQueue(label: "dev.frankencode.browser.snapshot-comparison-retirement", qos: .utility)
    init(transport: AtlasSnapshotComparisonTransport, before: AnyObject?, after: AnyObject?) throws {
        let handle = transport.create()
        guard handle > 0 else { throw AtlasSnapshotComparisonError.unavailable }
        self.handle = handle; self.transport = transport
        access = AtlasSnapshotComparisonAccess(before: before, after: after)
    }
    func cancel() { _ = transport.cancel(handle) }
    deinit {
        let handle = handle, transport = transport, access = access
        Self.retirement.async {
            withExtendedLifetime(access) {
                for attempt in 0..<8 {
                    if transport.close(handle) { return }
                    if attempt < 7 { Thread.sleep(forTimeInterval: 0.001) }
                }
                transport.retirementFailed()
            }
        }
    }
}

/// A pending pair never changes the provenance of an accepted comparison.
/// Metadata merge, filtering and member transfer share one bounded worker lane.
@Observable @MainActor final class AtlasSnapshotComparisonModel {
    private(set) var info: AtlasSnapshotComparisonInfo?
    private(set) var page: AtlasSnapshotComparisonPage?
    private(set) var progress: AtlasSnapshotComparisonProgress?
    private(set) var filter: AtlasSnapshotComparisonFilter = .differences
    private(set) var busy = false
    private(set) var closed = false
    private(set) var notice = "Choose before and after FCBS captures to compare their saved paths and sources."
    var canOperate: Bool { !busy && !closed }
    var coverage: String { page?.summary ?? "The comparison has not accepted a pair of saved archives." }
    @ObservationIgnored private let transport: AtlasSnapshotComparisonTransport
    @ObservationIgnored private let io = AtlasProjectIO()
    @ObservationIgnored private var session: AtlasSnapshotComparisonSession?
    @ObservationIgnored private var pending: AtlasSnapshotComparisonSession?
    @ObservationIgnored private var cancellation: AtlasSearchCancellation?
    @ObservationIgnored private var task: Task<Void, Never>?

    init(transport: AtlasSnapshotComparisonTransport) { self.transport = transport }

    func open(before: String, after: String, beforeAccess: AnyObject?, afterAccess: AnyObject?) {
        guard canOperate else { return }
        guard AtlasSnapshotComparisonPath.validArchive(before), AtlasSnapshotComparisonPath.validArchive(after) else {
            notice = AtlasSnapshotComparisonError.invalidInput.message; return
        }
        let requestedFilter = filter
        progress = nil
        notice = "Validating both selected saved captures…"
        perform { model, flag in
            let candidate = try AtlasSnapshotComparisonSession(transport: model.transport, before: beforeAccess, after: afterAccess)
            model.pending = candidate
            do {
                let opened: AtlasSnapshotComparisonInfo
                do {
                    let result = try await model.io.perform(cancellation: flag, keepingAlive: [candidate]) {
                        try candidate.transport.open(candidate.handle, before, after, flag)
                    }
                    opened = try AtlasSnapshotComparisonInfo.decode(result, owner: candidate.handle,
                        before: Array(before.utf8), after: Array(after.utf8), fresh: true)
                } catch {
                    // A lost initial reply cannot justify reopening a pathname.
                    // Read accepted metadata once on the same private owner.
                    try model.accept(flag)
                    let result = try await model.io.perform(cancellation: flag, keepingAlive: [candidate]) {
                        try candidate.transport.info(candidate.handle)
                    }
                    opened = try AtlasSnapshotComparisonInfo.decode(result, owner: candidate.handle,
                        before: Array(before.utf8), after: Array(after.utf8), fresh: true)
                }
                try model.accept(flag)
                let identity = opened.identity, generation: UInt64 = 1
                model.notice = "Comparing saved manifests without reading member payloads…"
                var current = try await model.io.perform(cancellation: flag, keepingAlive: [candidate]) {
                    try AtlasSnapshotComparisonPage.decode(candidate.transport.begin(candidate.handle, generation, flag),
                        identity: identity, generation: generation, command: "begin")
                }
                guard current.building, current.progress.steps == 0, current.progress.lastStepEntries == 0,
                      current.progress.compared == 0, current.info.io == opened.io,
                      current.info.lastAttempt == generation else { throw AtlasSnapshotComparisonError.invalidResponse }
                try model.accept(flag)
                model.progress = current.progress
                while current.building {
                    let previous = current.progress
                    let previousIO = current.info.io
                    let next = try await model.io.perform(cancellation: flag, keepingAlive: [candidate]) {
                        let result = try AtlasSnapshotComparisonPage.decode(
                            candidate.transport.step(candidate.handle, generation, 256, flag),
                            identity: identity, generation: generation, command: "step")
                        try previous.validateSuccessor(result.progress, maximumEntries: 256)
                        guard result.info.io == previousIO else { throw AtlasSnapshotComparisonError.invalidResponse }
                        return result
                    }
                    try model.accept(flag)
                    current = next; model.progress = next.progress
                }
                if requestedFilter != .differences {
                    current = try await model.fetch(candidate, page: current, filter: requestedFilter,
                        start: 0, previousIO: current.info.io, flag: flag)
                }
                try model.accept(flag)
                guard model.pending === candidate else { throw AtlasSnapshotComparisonError.canceled }
                model.session = candidate; model.pending = nil
                model.info = current.info; model.page = current; model.filter = requestedFilter
                model.progress = nil; model.notice = current.summary
            } catch {
                if model.pending === candidate { model.pending = nil; candidate.cancel() }
                throw error
            }
        }
    }

    func setFilter(_ requested: AtlasSnapshotComparisonFilter) {
        guard canOperate else { return }
        guard let page, let session, let info else { filter = requested; return }
        perform { model, flag in
            let next = try await model.fetch(session, page: page, filter: requested, start: 0,
                previousIO: info.io, flag: flag)
            try model.accept(flag)
            guard model.session === session, model.page?.id == page.id else { throw AtlasSnapshotComparisonError.canceled }
            model.page = next; model.info = next.info; model.filter = requested
            model.notice = "\(next.total) paths in \(requested.label.lowercased()). " + next.summary
        }
    }

    func loadPage(start: UInt64) {
        guard canOperate, let page, let session, let info, start <= page.total else { return }
        perform { model, flag in
            let next = try await model.fetch(session, page: page, filter: page.filter, start: start,
                previousIO: info.io, flag: flag)
            try model.accept(flag)
            guard model.session === session, model.page?.id == page.id else { throw AtlasSnapshotComparisonError.canceled }
            model.page = next; model.info = next.info
            model.notice = "Showing \(next.rows.count) saved paths from offset \(start), of \(next.total) matching paths. No member payloads were read."
        }
    }
    private func fetch(_ session: AtlasSnapshotComparisonSession, page: AtlasSnapshotComparisonPage,
        filter: AtlasSnapshotComparisonFilter, start: UInt64, previousIO: AtlasSnapshotComparisonIOCounts,
        flag: AtlasSearchCancellation) async throws -> AtlasSnapshotComparisonPage {
        try await io.perform(cancellation: flag, keepingAlive: [session]) {
            let next = try AtlasSnapshotComparisonPage.decode(
                session.transport.page(session.handle, page.generation, filter, start, 64),
                identity: page.identity, generation: page.generation, command: "page", filter: filter, start: start)
            guard next.progress == page.progress, next.info.io.follows(previousIO) else {
                throw AtlasSnapshotComparisonError.invalidResponse
            }
            return next
        }
    }

    func open(_ row: AtlasSnapshotComparisonRow, from page: AtlasSnapshotComparisonPage,
        side: AtlasSnapshotComparisonSide, readerTransport: AtlasReaderTransport,
        search: AtlasReaderSearchTransport? = nil, outline: AtlasReaderOutlineTransport? = nil,
        document: AtlasReaderDocumentTransport? = nil) async throws -> AtlasSnapshotComparisonReaderCapture {
        guard canOperate, let accepted = self.page, accepted.id == page.id, accepted.rows.contains(row),
              let session, let info, info.identity == page.identity, row.canOpen(side),
              let domain = transport.readerDomain, readerTransport.comparison?.domain == domain else {
            throw AtlasSnapshotComparisonError.invalidInput
        }
        let flag = AtlasSearchCancellation(), delivery = AtlasSnapshotComparisonReaderDelivery()
        cancellation = flag; busy = true
        let reader = AtlasPagedReaderModel(transport: readerTransport, search: search, outline: outline, document: document)
        defer { finish() }
        do {
            try await withTaskCancellationHandler(operation: {
                try await reader.openRetained(offset: 0, registryDomain: domain) { destination in
                    guard !flag.isCanceled else { throw AtlasSnapshotComparisonError.canceled }
                    // A previously canceled or lost activation may have advanced
                    // archive I/O. Observe this owner, never reopen either path.
                    let latest = try AtlasSnapshotComparisonInfo.decode(session.transport.info(session.handle),
                        owner: session.handle, before: page.identity.before.path.bytes, after: page.identity.after.path.bytes)
                    guard latest.identity == page.identity, latest.acceptedGeneration == page.generation,
                          latest.pendingGeneration == nil, latest.io.follows(info.io),
                          !flag.isCanceled else { throw AtlasSnapshotComparisonError.invalidResponse }
                    let receipt = try AtlasSnapshotComparisonReaderReceipt.decode(
                        session.transport.openReader(session.handle, destination, page.generation, row.id, side),
                        identity: page.identity, generation: page.generation, row: row, side: side,
                        destination: destination, previousIO: latest.io)
                    try delivery.publish(receipt)
                    return receipt.readerInfo
                }
                try accept(flag)
                guard self.session === session, self.page?.id == page.id, let initial = reader.page,
                      let member = row.member(side), initial.identity.pathBytes == row.path.bytes,
                      initial.identity.capturedBytes == member.bytes else { throw AtlasSnapshotComparisonError.invalidResponse }
            }, onCancel: { flag.cancel(); session.cancel() })
            let receipt = try delivery.take()
            self.info = receipt.comparisonInfo
            notice = "Opened the exact \(side.label.lowercased()) saved member. Its reader owns independent source bytes."
            return AtlasSnapshotComparisonReaderCapture(reader: reader, identity: page.identity,
                generation: page.generation, row: row, side: side)
        } catch {
            reader.close(); notice = Self.message(error); throw error
        }
    }

    func cancel() {
        guard busy else { return }
        cancellation?.cancel(); task?.cancel(); pending?.cancel(); session?.cancel(); io.cancel()
        notice = "Cancellation requested. Waiting for entered archive work to drain."
    }
    func close() {
        guard !closed else { return }
        closed = true; cancel(); page = nil; info = nil; progress = nil
        if !busy { session = nil; pending = nil }
    }
    private func accept(_ flag: AtlasSearchCancellation) throws {
        guard !closed, cancellation === flag, !flag.isCanceled, !Task.isCancelled else {
            throw AtlasSnapshotComparisonError.canceled
        }
    }
    private func perform(_ operation: @escaping @MainActor (AtlasSnapshotComparisonModel, AtlasSearchCancellation) async throws -> Void) {
        guard canOperate else { return }
        let flag = AtlasSearchCancellation()
        cancellation = flag; busy = true
        task = Task {
            do { try await operation(self, flag) }
            catch { notice = Self.message(error) }
            progress = nil
            finish()
        }
    }
    private func finish() {
        busy = false; cancellation = nil; task = nil
        if closed { session = nil; pending = nil }
    }
    private static func message(_ error: Error) -> String {
        if error is CancellationError { return AtlasSnapshotComparisonError.canceled.message }
        if let error = error as? AtlasProjectIOError, case .canceled = error {
            return AtlasSnapshotComparisonError.canceled.message
        }
        return (error as? AtlasSnapshotComparisonError)?.message
            ?? (error as? AtlasReaderError)?.message ?? AtlasSnapshotComparisonError.invalidResponse.message
    }
}
