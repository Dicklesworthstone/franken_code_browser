import Foundation
import Observation

/// One main-reader visit. The path is metadata for explicit current-file
/// actions; Back/Forward only expose this already accepted reader owner.
@MainActor final class AtlasWorkspaceReaderCapture: Identifiable {
    let id = UUID()
    let root: String
    let relativePath: [UInt8]
    let label: String
    let reader: AtlasPagedReaderModel
    var path: String? { String(bytes: relativePath, encoding: .utf8) }

    init(root: String, relativePath: [UInt8], label: String, reader: AtlasPagedReaderModel) throws {
        guard root.hasPrefix("/"), root.utf8.count <= 16_384, !root.utf8.contains(0),
              !relativePath.isEmpty, relativePath.count <= 16_384, relativePath.first != 47,
              !relativePath.contains(0), label.utf8.count <= 65_536,
              !relativePath.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                  $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
              }), let page = reader.page, !reader.busy else { throw AtlasReaderError.invalidInput }
        let absolute = Array(root.utf8) + (root.hasSuffix("/") ? [] : [47]) + relativePath
        guard page.identity.pathBytes == relativePath || page.identity.pathBytes == absolute else {
            throw AtlasReaderError.invalidInput
        }
        self.root = root; self.relativePath = relativePath; self.label = label; self.reader = reader
    }
}

/// Main-window file navigation owns three accepted captures and at most one
/// opening candidate. One newest request can wait behind cancellation without
/// allocating a reader. The native eight-reader registry still admits every
/// owner, including two optional pins and readers in other product surfaces.
@Observable @MainActor final class AtlasWorkspaceReaderHistory {
    private let history = AtlasDocumentLinkHistory<AtlasWorkspaceReaderCapture>(
        retire: { $0.reader.close() },
        failureMessage: {
            "Source could not be opened. Your retained reader and Forward history are unchanged. "
                + (($0 as? AtlasReaderError)?.message ?? AtlasReaderError.unavailable.message)
        })
    private var pending: Request?
    private(set) var requestedLabel: String?
    @ObservationIgnored private var requestID = UUID()

    @MainActor private final class Request {
        let id = UUID()
        let label: String
        let open: @MainActor () async throws -> AtlasWorkspaceReaderCapture
        /// An ordinary request owns only input metadata. An already prepared
        /// Quick Open reader must be closed if its waiting request is replaced.
        let discard: @MainActor () -> Void
        private(set) var entered = false
        init(label: String, open: @escaping @MainActor () async throws -> AtlasWorkspaceReaderCapture,
             discard: @escaping @MainActor () -> Void) {
            self.label = label; self.open = open; self.discard = discard
        }
        func execute() async throws -> AtlasWorkspaceReaderCapture {
            entered = true
            return try await open()
        }
    }

    var current: AtlasWorkspaceReaderCapture? { history.current }
    var captures: [AtlasWorkspaceReaderCapture] { history.captures }
    var count: Int { history.captures.count }
    var position: Int { history.current == nil ? 0 : history.position + 1 }
    var busy: Bool { history.busy }
    var canceling: Bool { history.canceling }
    var canBack: Bool { history.canBack }
    var canForward: Bool { history.canForward }
    var notice: String {
        if let pending { return "Waiting to open \(pending.label) after the previous source operation drains. The retained reader stays available." }
        return history.notice
    }

    /// Same root bytes and native grant retain history across catalog refresh.
    /// A new path or replacement grant immediately withdraws every old reader.
    @discardableResult
    func bind(root: String, accessIdentity: ObjectIdentifier?) -> Bool {
        let retained = history.bind(root: root, accessIdentity: accessIdentity)
        if !retained {
            discardPending()
            requestID = UUID()
            requestedLabel = nil
        }
        return retained
    }

    func open(root: String, path: String, accessLease: AnyObject?, initialLine: UInt64? = nil,
              captured: AtlasSearchCapturedHit? = nil,
              makeReader: @escaping @MainActor () -> AtlasPagedReaderModel) {
        bind(root: root, accessIdentity: accessLease.map(ObjectIdentifier.init))
        guard history.bound else { return }
        // Bound waiting metadata before retaining the closure and its source.
        let relativePath: [UInt8]
        if let captured {
            guard captured.root.utf8.elementsEqual(root.utf8), captured.path.utf8.elementsEqual(path.utf8),
                  (try? AtlasReaderInput(captured: captured)) != nil else { return }
            relativePath = captured.pathBytes
        } else {
            guard (try? AtlasReaderInput(root: root, path: path)) != nil else { return }
            relativePath = Array(path.utf8)
        }
        submit(Request(label: path, open: {
            let reader = makeReader()
            do {
                try await reader.prepareOpen(root: root, path: path, accessLease: accessLease, initialLine: initialLine)
                try Task.checkCancellation()
                return try AtlasWorkspaceReaderCapture(root: root, relativePath: relativePath,
                    label: path, reader: reader)
            } catch {
                reader.close()
                throw error
            }
        }, discard: {}))
    }

    /// Quick Open has already verified and opened the raw filename. Adopt that
    /// exact owner, including non-UTF-8 names, without a pathname round-trip.
    func adopt(root: String, relativePath: [UInt8], label: String,
               accessLease: AnyObject?, reader: AtlasPagedReaderModel) {
        bind(root: root, accessIdentity: accessLease.map(ObjectIdentifier.init))
        guard history.bound else { reader.close(); return }
        guard !history.captures.contains(where: { $0.reader === reader }) else { return }
        do {
            let capture = try AtlasWorkspaceReaderCapture(root: root, relativePath: relativePath,
                label: label, reader: reader)
            submit(Request(label: label, open: { capture }, discard: { reader.close() }))
        } catch { reader.close() }
    }

    func back() { history.back() }
    func forward() { history.forward() }
    func cancel() {
        discardPending()
        requestID = UUID()
        requestedLabel = nil
        history.cancel()
    }
    func catalogRefreshed() {
        discardPending()
        requestID = UUID()
        requestedLabel = nil
        history.catalogRefreshed()
    }
    func close() {
        discardPending()
        requestID = UUID()
        requestedLabel = nil
        history.close()
    }

    private func submit(_ request: Request) {
        requestedLabel = request.label
        requestID = request.id
        if history.busy {
            discardPending()
            pending = request
            history.cancel()
        } else { start(request) }
    }

    private func start(_ request: Request) {
        guard history.canFollow else { request.discard(); return }
        history.follow(didFinish: { [weak self] in
            if !request.entered { request.discard() }
            guard let self else { return }
            if self.requestID == request.id { self.requestedLabel = nil }
            if let next = self.pending {
                self.pending = nil
                self.start(next)
            }
        }) {
            // follow() can be canceled before entering this closure. A waiting
            // prepared reader is retained by the request until the task drains.
            try await request.execute()
        }
    }

    private func discardPending() {
        let previous = pending
        pending = nil
        previous?.discard()
    }
}
