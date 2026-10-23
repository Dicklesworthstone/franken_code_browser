import Foundation
import Observation
import Dispatch

enum AtlasProjectBrowserError: Error, Sendable {
    case unavailable, invalidResponse, stale, canceled
    case engine(String)
    var message: String {
        switch self {
        case .unavailable: return "Project browsing is unavailable. Refresh the project to try again."
        case .invalidResponse: return "The project page could not be verified. Reload the latest page before opening files."
        case .stale: return "This file page is no longer retained. Choose a file from a current page."
        case .canceled: return "Project work stopped. An entered discovery batch may still be finishing."
        case .engine(let code): return "Project operation unavailable (\(code))."
        }
    }
}

private struct AtlasCatalogNumber: Decodable {
    let value: UInt64
    init(from decoder: Decoder) throws {
        let text = try decoder.singleValueContainer().decode(String.self)
        guard let value = AtlasReaderWire.integer(text) else { throw AtlasProjectBrowserError.invalidResponse }
        self.value = value
    }
}

struct AtlasProjectBrowserFile: Decodable, Sendable, Identifiable {
    let id: UInt64
    let rawPath: [UInt8]
    let display: String
    let observedBytes: UInt64
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(AtlasCatalogNumber.self, forKey: .file_id).value
        observedBytes = try c.decode(AtlasCatalogNumber.self, forKey: .observed_bytes).value
        let path = try c.decode(AtlasCatalogPath.self, forKey: .path)
        rawPath = path.raw; display = path.display
        guard id > 0 else { throw AtlasProjectBrowserError.invalidResponse }
    }
    func matches(_ other: Self) -> Bool {
        id == other.id && rawPath == other.rawPath && observedBytes == other.observedBytes
    }
    private enum CodingKeys: CodingKey { case file_id, path, observed_bytes }
}

private struct AtlasCatalogPath: Decodable {
    let raw: [UInt8]
    let display: String
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let hex = try c.decode(String.self, forKey: .hex)
        guard try c.decode(String.self, forKey: .encoding) == "unix-bytes", hex.utf8.count <= 4096 else {
            throw AtlasProjectBrowserError.invalidResponse
        }
        raw = try AtlasReaderWire.pathBytes(hex)
        display = try c.decode(String.self, forKey: .display)
        guard !raw.isEmpty, raw.count <= 2048, raw.first != 47, !raw.contains(0),
              !raw.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                  $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
              }), !display.isEmpty, display.utf8.count <= 16_384 else {
            throw AtlasProjectBrowserError.invalidResponse
        }
    }
    private enum CodingKeys: CodingKey { case encoding, hex, display }
}

struct AtlasProjectBrowserInfo: Sendable {
    let owner, latest, firstRetained, retainedPages, retainedFiles, retainedPathBytes, discovered: UInt64
    let state: String
    let more, paused, complete: Bool
    let directories, excluded, symlinks, special, unavailable, cycles, pathLimited, depthLimited, queueRefused: UInt64
    func retains(_ page: UInt64) -> Bool { page >= firstRetained && page <= latest }
    var omissionCount: UInt64 {
        // Presentation only: never wrap counters from a very long discovery.
        [unavailable, cycles, pathLimited, depthLimited, queueRefused].reduce(0) {
            let (sum, overflow) = $0.addingReportingOverflow($1); return overflow ? UInt64.max : sum
        }
    }
    static func decode(_ json: String?, owner: UInt64, command: String = "info") throws -> Self {
        try decode(AtlasProjectBrowserWire.admit(json, owner: owner, command: command), owner: owner)
    }
    fileprivate static func decode(_ data: Data, owner: UInt64) throws -> Self {
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            let latest = w.latest_page.value, first = w.first_retained_page.value
            guard owner > 0, w.owner.value == owner, w.identity_scope == "retained-session",
                  w.ordering == "provisional-discovery-order", w.policy == "product-defaults/no-rule-files-v1",
                  !w.native_presented, w.discovery_source_bytes_read.value == 0, w.discovery_source_read_calls.value == 0,
                  ["discovering", "paused", "complete", "partial"].contains(w.state),
                  w.paused == (w.state == "paused"), w.more == (w.state == "discovering" || w.state == "paused"),
                  w.discovery_complete == (w.state == "complete"),
                  !w.discovery_complete || [w.discovery.unavailable.value, w.discovery.cycles.value,
                    w.discovery.path_limited.value, w.discovery.depth_limited.value, w.discovery.queue_refused.value].allSatisfy({ $0 == 0 }),
                  w.catalogued_files.value <= w.discovery.files.value,
                  latest > 0, first > 0, first <= latest, latest - first < 4,
                  w.retained_pages.value == latest - first + 1,
                  w.retained_files.value <= 1024, w.retained_files.value <= w.catalogued_files.value,
                  w.retained_path_bytes.value <= 270_336,
                  w.limits.page_entries.value == 256, w.limits.page_path_bytes.value == 67_584,
                  w.limits.max_path_bytes.value == 2048, w.limits.retained_pages.value == 4,
                  w.limits.retained_files.value == 1024, w.limits.retained_path_bytes.value == 270_336 else {
                throw AtlasProjectBrowserError.invalidResponse
            }
            return Self(owner: owner, latest: latest, firstRetained: first, retainedPages: w.retained_pages.value,
                retainedFiles: w.retained_files.value, retainedPathBytes: w.retained_path_bytes.value,
                discovered: w.catalogued_files.value, state: w.state, more: w.more, paused: w.paused,
                complete: w.discovery_complete, directories: w.discovery.directories.value,
                excluded: w.discovery.excluded_entries.value, symlinks: w.discovery.symlinks_not_followed.value,
                special: w.discovery.special_objects.value, unavailable: w.discovery.unavailable.value,
                cycles: w.discovery.cycles.value, pathLimited: w.discovery.path_limited.value,
                depthLimited: w.discovery.depth_limited.value, queueRefused: w.discovery.queue_refused.value)
        } catch let error as AtlasProjectBrowserError { throw error }
        catch { throw AtlasProjectBrowserError.invalidResponse }
    }
    private struct Limits: Decodable {
        let page_entries, page_path_bytes, max_path_bytes, retained_pages, retained_files, retained_path_bytes: AtlasCatalogNumber
    }
    private struct Discovery: Decodable {
        let files, directories, excluded_entries, symlinks_not_followed, special_objects: AtlasCatalogNumber
        let unavailable, cycles, path_limited, depth_limited, queue_refused: AtlasCatalogNumber
    }
    private struct Wire: Decodable {
        let owner, latest_page, first_retained_page, retained_pages, retained_files, retained_path_bytes, catalogued_files: AtlasCatalogNumber
        let identity_scope, ordering, policy, state: String
        let native_presented, more, paused, discovery_complete: Bool
        let discovery_source_bytes_read, discovery_source_read_calls: AtlasCatalogNumber
        let limits: Limits
        let discovery: Discovery
    }
}

struct AtlasProjectBrowserPage: Sendable {
    let info: AtlasProjectBrowserInfo
    let id: UInt64
    let files: [AtlasProjectBrowserFile]
    static func decode(_ json: String?, owner: UInt64, page expected: UInt64? = nil) throws -> Self {
        let data = try AtlasProjectBrowserWire.admit(json, owner: owner, command: "page")
        do {
            let info = try AtlasProjectBrowserInfo.decode(data, owner: owner)
            let w = try JSONDecoder().decode(Wire.self, from: data), id = w.page_id.value
            guard info.retains(id), expected == nil || expected == id, w.after_page.value == id - 1,
                  UInt64(w.files.count) <= info.retainedFiles else { throw AtlasProjectBrowserError.invalidResponse }
            var ids = Set<UInt64>(), paths = Set<[UInt8]>(), bytes = 0, charged = 0, previousID: UInt64 = 0
            for file in w.files {
                guard file.id > previousID, file.id <= info.discovered,
                      ids.insert(file.id).inserted, paths.insert(file.rawPath).inserted else {
                    throw AtlasProjectBrowserError.invalidResponse
                }
                previousID = file.id
                bytes += file.rawPath.count
                charged += file.rawPath.count * 3 + file.display.utf8.count + 256
                guard bytes <= 67_584, charged <= AtlasProjectBrowserWire.maximumResponseBytes else {
                    throw AtlasProjectBrowserError.invalidResponse
                }
            }
            guard UInt64(bytes) <= info.retainedPathBytes else { throw AtlasProjectBrowserError.invalidResponse }
            return Self(info: info, id: id, files: w.files)
        } catch let error as AtlasProjectBrowserError { throw error }
        catch { throw AtlasProjectBrowserError.invalidResponse }
    }
    private struct Wire: Decodable {
        let page_id, after_page: AtlasCatalogNumber
        let files: [AtlasProjectBrowserFile]
        enum CodingKeys: CodingKey { case page_id, after_page, files }
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            page_id = try c.decode(AtlasCatalogNumber.self, forKey: .page_id)
            after_page = try c.decode(AtlasCatalogNumber.self, forKey: .after_page)
            var rows = try c.nestedUnkeyedContainer(forKey: .files)
            guard rows.count.map({ $0 <= 256 }) ?? true else { throw AtlasProjectBrowserError.invalidResponse }
            var files: [AtlasProjectBrowserFile] = []; files.reserveCapacity(rows.count ?? 0)
            while !rows.isAtEnd {
                guard files.count < 256 else { throw AtlasProjectBrowserError.invalidResponse }
                files.append(try rows.decode(AtlasProjectBrowserFile.self))
            }
            self.files = files
        }
    }
}

enum AtlasProjectBrowserWire {
    static let maximumResponseBytes = 1024 * 1024
    static func admit(_ json: String?, owner: UInt64, command: String) throws -> Data {
        guard let json, !json.isEmpty, json.utf8.count <= maximumResponseBytes else { throw AtlasProjectBrowserError.unavailable }
        let data = Data(json.utf8)
        struct Header: Decodable {
            let schema, status: String
            let command: String?
            let owner: AtlasCatalogNumber
            let error: Failure?
        }
        struct Failure: Decodable { let code: String }
        do {
            let header = try JSONDecoder().decode(Header.self, from: data)
            guard owner > 0, header.owner.value == owner, header.schema == "fcb.project-catalog-session/1" else {
                throw AtlasProjectBrowserError.invalidResponse
            }
            if header.status == "error" {
                guard let code = header.error?.code, !code.isEmpty, code.utf8.count <= 512,
                      code.unicodeScalars.allSatisfy({ $0.value >= 32 && $0.value != 127 }) else {
                    throw AtlasProjectBrowserError.invalidResponse
                }
                throw AtlasProjectBrowserError.engine(code)
            }
            guard header.status == "ok", header.error == nil, header.command == command else {
                throw AtlasProjectBrowserError.invalidResponse
            }
            return data
        } catch let error as AtlasProjectBrowserError { throw error }
        catch { throw AtlasProjectBrowserError.invalidResponse }
    }
}

enum AtlasProjectBrowserRequest: Sendable {
    case open(String), info, page(UInt64), advance(after: UInt64), stop
    case openReader(reader: UInt64, page: UInt64, file: UInt64, bytes: UInt64)
}
struct AtlasProjectBrowserTransport: Sendable {
    let create: @Sendable () -> UInt64
    let run: @Sendable (UInt64, AtlasProjectBrowserRequest) -> String?
    let cancel: @Sendable (UInt64) -> Bool
    let close: @Sendable (UInt64) -> Bool
    let retirementFailed: @Sendable () -> Void
    let readerDomain: UUID?
}

private struct AtlasProjectBrowserReceipt: Sendable {
    let readerJSON: String
    let window: AtlasReaderPage
    static func decode(_ json: String?, owner: UInt64, page: UInt64, file: AtlasProjectBrowserFile,
        destination: UInt64) throws -> Self {
        let data = try AtlasProjectBrowserWire.admit(json, owner: owner, command: "open-reader")
        do {
            let current = try AtlasProjectBrowserInfo.decode(data, owner: owner)
            struct Selection: Decodable {
                let page_id, file_id, reader_owner, source_bytes_read, source_read_calls: AtlasCatalogNumber
                let path: AtlasCatalogPath
                let source_observation: String
            }
            let selected = try JSONDecoder().decode(Selection.self, from: data)
            guard current.retains(page), selected.page_id.value == page, selected.file_id.value == file.id,
                  selected.path.raw == file.rawPath, destination > 0, destination != owner,
                  selected.reader_owner.value == destination,
                  selected.source_observation == "new-capture-after-metadata-selection",
                  let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let nested = object["reader"] as? [String: Any], let window = object["window"] as? [String: Any] else {
                throw AtlasProjectBrowserError.invalidResponse
            }
            func encoded(_ value: [String: Any]) throws -> String {
                String(decoding: try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self)
            }
            let readerJSON = try encoded(nested)
            let reader = try AtlasReaderInfo.decodeRetainedInitialization(readerJSON, handle: destination, pathBytes: file.rawPath)
            let bytes = selected.source_bytes_read.value, calls = selected.source_read_calls.value
            guard bytes == reader.identity.capturedBytes, bytes <= AtlasReaderLimits.captureBytes,
                  (bytes == 0 && calls == 0) || (bytes > 0 && calls > 0 && calls <= 1_048_576) else {
                throw AtlasProjectBrowserError.invalidResponse
            }
            // Directory metadata predates this explicitly requested capture;
            // observedBytes is not a source snapshot or digest promise.
            let first = try AtlasReaderPage.decode(encoded(window), info: reader, request: .window(offset: 0, bytes: 16_384))
            return Self(readerJSON: readerJSON, window: first)
        } catch let error as AtlasProjectBrowserError { throw error }
        catch { throw AtlasProjectBrowserError.invalidResponse }
    }
    func validate(_ page: AtlasReaderPage?) throws {
        guard let page, page.identity.matches(window.identity), page.start == window.start else {
            throw AtlasProjectBrowserError.invalidResponse
        }
        let common = Int(min(page.end, window.end) - page.start) * 2
        guard page.originalHex.utf8.prefix(common).elementsEqual(window.originalHex.utf8.prefix(common)) else {
            throw AtlasProjectBrowserError.invalidResponse
        }
    }
}
private final class AtlasProjectBrowserReceiptBox: @unchecked Sendable {
    private let lock = NSLock()
    private var receipt: AtlasProjectBrowserReceipt?
    func set(_ value: AtlasProjectBrowserReceipt) throws {
        lock.lock(); defer { lock.unlock() }
        guard receipt == nil else { throw AtlasProjectBrowserError.invalidResponse }
        receipt = value
    }
    func accepted() throws -> AtlasProjectBrowserReceipt {
        lock.lock(); defer { lock.unlock() }
        guard let receipt else { throw AtlasProjectBrowserError.invalidResponse }
        return receipt
    }
}
@MainActor final class AtlasProjectBrowserCapture: Identifiable {
    let id = UUID()
    let file: AtlasProjectBrowserFile
    let reader: AtlasPagedReaderModel
    init(file: AtlasProjectBrowserFile, reader: AtlasPagedReaderModel) { self.file = file; self.reader = reader }
}

private final class AtlasProjectBrowserAccess: @unchecked Sendable {
    let reference: AnyObject?
    init(_ reference: AnyObject?) { self.reference = reference }
}
private final class AtlasProjectBrowserSession: @unchecked Sendable {
    let owner: UInt64
    let transport: AtlasProjectBrowserTransport
    private let access: AtlasProjectBrowserAccess
    private static let retirement = DispatchQueue(label: "dev.frankencode.browser.discovery-retirement", qos: .utility)
    init(transport: AtlasProjectBrowserTransport, access: AnyObject?) throws {
        let owner = transport.create()
        guard owner != 0 else { throw AtlasProjectBrowserError.unavailable }
        self.owner = owner; self.transport = transport; self.access = AtlasProjectBrowserAccess(access)
    }
    func cancel() { _ = transport.cancel(owner) }
    deinit {
        let owner = owner, transport = transport, access = access
        Self.retirement.async {
            withExtendedLifetime(access) {
                for attempt in 0..<8 {
                    if transport.close(owner) { return }
                    if attempt < 7 { Thread.sleep(forTimeInterval: 0.001) }
                }
                transport.retirementFailed()
            }
        }
    }
}

/// One visible metadata page and one entered operation. The Rust session owns
/// at most four pages; Swift never accumulates the traversed project tree.
@Observable @MainActor final class AtlasProjectBrowserModel {
    private(set) var info: AtlasProjectBrowserInfo?
    private(set) var page: AtlasProjectBrowserPage?
    private(set) var root = ""
    private(set) var busy = false
    private(set) var closed = false
    private(set) var notice = "Opening the first project page…"
    @ObservationIgnored private let transport: AtlasProjectBrowserTransport
    @ObservationIgnored private let io = AtlasProjectIO()
    @ObservationIgnored private var session: AtlasProjectBrowserSession?
    @ObservationIgnored private var flag: AtlasSearchCancellation?
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var stopRequested = false

    init(transport: AtlasProjectBrowserTransport) { self.transport = transport }
    var canOperate: Bool { !busy && !closed && info != nil }
    var canContinue: Bool { canOperate && info?.more == true }
    var currentPage: Bool { guard let info, let page else { return false }; return info.owner == page.info.owner && info.retains(page.id) }

    func open(root: String, accessLease: AnyObject?) async throws {
        guard !closed, !busy, session == nil, root.hasPrefix("/"), root.utf8.count <= 16_384,
              !root.utf8.contains(0) else { throw AtlasProjectBrowserError.unavailable }
        let lease = try AtlasProjectBrowserSession(transport: transport, access: accessLease)
        self.root = root; session = lease; busy = true
        let flag = AtlasSearchCancellation(); self.flag = flag
        defer { finish() }
        do {
            let first = try AtlasProjectBrowserPage.decode(await run(lease, .open(root), flag: flag), owner: lease.owner, page: 1)
            guard !closed, session === lease else { throw AtlasProjectBrowserError.canceled }
            accept(first)
        } catch {
            // Opening/advancing can consume a batch before cancellation or a
            // lost reply. Recover accepted metadata; never repeat the walk.
            guard await recover(lease) else { notice = Self.message(error); throw error }
        }
    }
    func advance() {
        guard canContinue, let info, info.latest < UInt64.max else { return }
        let after = info.latest
        perform { model, lease, flag in
            do {
                let next = try AtlasProjectBrowserPage.decode(await model.run(lease, .advance(after: after), flag: flag),
                    owner: lease.owner, page: after + 1)
                guard !model.closed, model.session === lease else { throw AtlasProjectBrowserError.canceled }
                model.accept(next)
            } catch {
                let recovered = await model.recover(lease)
                model.notice = recovered ? "Reloaded the latest discovered page after the interrupted batch. Continue when ready."
                    : Self.message(error)
            }
        }
    }
    func load(page id: UInt64) {
        guard canOperate, info?.retains(id) == true else { return }
        perform { model, lease, flag in
            let page = try AtlasProjectBrowserPage.decode(await model.run(lease, .page(id), flag: flag), owner: lease.owner, page: id)
            guard !model.closed, model.session === lease else { throw AtlasProjectBrowserError.canceled }
            model.accept(page)
        }
    }
    func reload() {
        perform { model, lease, _ in
            model.notice = await model.recover(lease) ? "Reloaded the latest retained page without advancing discovery."
                : "The latest project page is unavailable. Refresh the project to start a new discovery."
        }
    }
    func stop() {
        guard !closed else { return }
        if busy {
            stopRequested = true; flag?.cancel(); session?.cancel()
            notice = "Stopping after entered project work drains…"
            return
        }
        guard info?.more == true else { return }
        perform { model, lease, flag in
            do {
                let info = try AtlasProjectBrowserInfo.decode(await model.run(lease, .stop, flag: flag), owner: lease.owner, command: "stop")
                guard !model.closed, model.session === lease else { throw AtlasProjectBrowserError.canceled }
                model.info = info
                model.notice = "Discovery paused. Retained pages remain readable; Continue resumes from this cursor."
            } catch {
                _ = await model.recover(lease); model.notice = Self.message(error)
            }
        }
    }
    func openFile(_ file: AtlasProjectBrowserFile, from page: AtlasProjectBrowserPage,
        readerTransport: AtlasReaderTransport, search: AtlasReaderSearchTransport? = nil,
        outline: AtlasReaderOutlineTransport? = nil, document: AtlasReaderDocumentTransport? = nil) async throws -> AtlasProjectBrowserCapture {
        guard canOperate, currentPage, let accepted = self.page, let lease = session, let domain = transport.readerDomain,
              readerTransport.comparison?.domain == domain, page.info.owner == lease.owner,
              accepted.id == page.id, accepted.info.owner == page.info.owner,
              accepted.files.contains(where: { $0.matches(file) }), page.files.contains(where: { $0.matches(file) }) else {
            throw AtlasProjectBrowserError.stale
        }
        try Task.checkCancellation()
        let flag = AtlasSearchCancellation(); self.flag = flag; busy = true
        let reader = AtlasPagedReaderModel(transport: readerTransport, search: search, outline: outline, document: document)
        defer { finish() }
        do {
            return try await withTaskCancellationHandler(operation: {
                let receipt = AtlasProjectBrowserReceiptBox()
                try await reader.openRetained(pathBytes: file.rawPath, offset: 0, registryDomain: domain) { destination in
                    guard !flag.isCanceled else { throw AtlasProjectBrowserError.canceled }
                    return try withExtendedLifetime(lease) {
                        let admitted = try AtlasProjectBrowserReceipt.decode(lease.transport.run(lease.owner,
                            .openReader(reader: destination, page: page.id, file: file.id, bytes: AtlasReaderLimits.captureBytes)),
                            owner: lease.owner, page: page.id, file: file, destination: destination)
                        try receipt.set(admitted)
                        return admitted.readerJSON
                    }
                }
                try Task.checkCancellation()
                guard !closed, !flag.isCanceled, session === lease else { throw AtlasProjectBrowserError.canceled }
                try receipt.accepted().validate(reader.page)
                notice = "Opened a new source capture from the selected file. Its reader survives page eviction and project closure."
                return AtlasProjectBrowserCapture(file: file, reader: reader)
            }, onCancel: { flag.cancel(); lease.cancel() })
        } catch {
            reader.close(); notice = Self.message(error); throw error
        }
    }
    func close() {
        guard !closed else { return }
        closed = true; stopRequested = false; flag?.cancel(); session?.cancel(); task?.cancel()
        info = nil; page = nil
        if !busy { session = nil }
    }
    private func accept(_ page: AtlasProjectBrowserPage) {
        self.page = page; info = page.info
        notice = page.info.more ? "Project page ready. Continue discovers the next bounded batch."
            : (page.info.complete ? "Discovery finished for this project policy." : "Discovery finished with omissions; inspect the coverage counts.")
    }
    private func recover(_ lease: AtlasProjectBrowserSession) async -> Bool {
        guard !closed, session === lease else { return false }
        let recovery = AtlasSearchCancellation()
        do {
            let info = try AtlasProjectBrowserInfo.decode(await run(lease, .info, flag: recovery), owner: lease.owner)
            let page = try AtlasProjectBrowserPage.decode(await run(lease, .page(info.latest), flag: recovery), owner: lease.owner, page: info.latest)
            guard !closed, session === lease else { return false }
            accept(page); return true
        } catch {
            if !closed, session === lease { info = nil; page = nil }
            return false
        }
    }
    private func run(_ lease: AtlasProjectBrowserSession, _ request: AtlasProjectBrowserRequest,
        flag: AtlasSearchCancellation) async throws -> String? {
        try await io.perform(cancellation: flag, keepingAlive: [lease]) { lease.transport.run(lease.owner, request) }
    }
    private func perform(_ work: @escaping @MainActor (AtlasProjectBrowserModel, AtlasProjectBrowserSession, AtlasSearchCancellation) async throws -> Void) {
        guard !busy, !closed, let lease = session else { return }
        let flag = AtlasSearchCancellation(); self.flag = flag; busy = true
        task = Task {
            do { try await work(self, lease, flag) }
            catch { notice = Self.message(error) }
            finish()
        }
    }
    private func finish() {
        busy = false; flag = nil; task = nil
        if closed { session = nil }
        else if stopRequested { stopRequested = false; stop() }
    }
    private static func message(_ error: Error) -> String {
        if let error = error as? AtlasProjectBrowserError { return error.message }
        if let error = error as? AtlasReaderError { return error.message }
        if error is CancellationError { return AtlasProjectBrowserError.canceled.message }
        if let error = error as? AtlasProjectIOError, case .canceled = error { return AtlasProjectBrowserError.canceled.message }
        return AtlasProjectBrowserError.invalidResponse.message
    }
}
