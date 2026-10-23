import Foundation
import Observation
import Dispatch

enum AtlasProgressiveFileError: Error, Sendable {
    case unavailable, invalidResponse, stale, canceled
    case engine(String)
    var message: String {
        switch self {
        case .unavailable: return "File finding is unavailable. Reload accepted results or start this search again."
        case .invalidResponse: return "The file results could not be verified. Reload before opening a file."
        case .stale: return "This file result is no longer current. Select a current result before opening it."
        case .canceled: return "File finding stopped. Entered work may still be finishing."
        case .engine(let code): return "File finding unavailable (\(code))."
        }
    }
}

private struct AtlasFinderNumber: Decodable {
    let value: UInt64
    init(from decoder: Decoder) throws {
        let text = try decoder.singleValueContainer().decode(String.self)
        guard let value = AtlasReaderWire.integer(text) else { throw AtlasProgressiveFileError.invalidResponse }
        self.value = value
    }
}

private struct AtlasFinderPath: Decodable {
    let raw: [UInt8]
    let display: String
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let hex = try c.decode(String.self, forKey: .hex)
        guard try c.decode(String.self, forKey: .encoding) == "unix-bytes",
              hex.utf8.count <= 4096 else { throw AtlasProgressiveFileError.invalidResponse }
        raw = try AtlasReaderWire.pathBytes(hex)
        display = try c.decode(String.self, forKey: .display)
        guard !raw.isEmpty, raw.count <= 2048, raw.first != 47, !raw.contains(0),
              !raw.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                  $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
              }), !display.isEmpty, display.utf8.count <= 16_384 else {
            throw AtlasProgressiveFileError.invalidResponse
        }
    }
    private enum CodingKeys: CodingKey { case encoding, hex, display }
}

struct AtlasProgressiveFileRow: Decodable, Sendable, Identifiable {
    let id: UInt64
    let rawPath: [UInt8]
    let display, kind: String
    var path: String? { String(bytes: rawPath, encoding: .utf8) }
    var explanation: String { kind.replacingOccurrences(of: "-", with: " ") }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(AtlasFinderNumber.self, forKey: .file_id).value
        let path = try c.decode(AtlasFinderPath.self, forKey: .path)
        rawPath = path.raw; display = path.display
        kind = try c.decode(String.self, forKey: .match_kind)
        guard id > 0, ["exact-filename", "exact-path", "exact-component", "filename-prefix",
            "component-prefix", "path-prefix", "filename-subsequence", "path-subsequence"].contains(kind) else {
            throw AtlasProgressiveFileError.invalidResponse
        }
    }
    func matches(_ other: Self) -> Bool { id == other.id && rawPath == other.rawPath && kind == other.kind }
    private enum CodingKeys: CodingKey { case file_id, path, match_kind }
}

struct AtlasProgressiveFileSelection: Hashable, Sendable {
    let owner, generation, file: UInt64
}

struct AtlasProgressiveFileReport: Sendable {
    let owner, generation, revision, catalogued, searched, matches, examined, candidates, work: UInt64
    let retainedPathBytes, processedBatches: UInt64
    let replayAfter: UInt64?
    let query: AtlasFileQuery
    let state: String
    let more, paused, discoveryComplete, complete, pending, truncated: Bool
    let unavailable, cycles, pathLimited, depthLimited, queueRefused: UInt64
    let excluded, symlinks, special: UInt64
    let rows: [AtlasProgressiveFileRow]
    var omissionCount: UInt64 {
        [unavailable, cycles, pathLimited, depthLimited, queueRefused].reduce(0) {
            let sum = $0.addingReportingOverflow($1); return sum.overflow ? UInt64.max : sum.partialValue
        }
    }
    func selection(_ file: UInt64) -> AtlasProgressiveFileSelection {
        .init(owner: owner, generation: generation, file: file)
    }
    func row(_ selection: AtlasProgressiveFileSelection) -> AtlasProgressiveFileRow? {
        guard selection.owner == owner, selection.generation == generation else { return nil }
        return rows.first { $0.id == selection.file }
    }
    var summary: String {
        var text = "\(rows.count) of \(matches) matching files shown · \(searched) filenames searched."
        if more { text += paused ? " Paused; Continue searches more files." : " More files remain to be searched." }
        else if complete { text += " Search finished for this project policy." }
        else { text += " Search finished with incomplete coverage." }
        if truncated { text += " Best \(rows.count) results retained; refine the query for other matches." }
        if omissionCount > 0 { text += " \(omissionCount) discovery omissions." }
        return text
    }
    static func decode(_ json: String?, owner: UInt64, generation: UInt64, query: AtlasFileQuery,
        command: String, revision expected: UInt64? = nil) throws -> Self {
        let data = try AtlasProgressiveFileWire.admit(json, owner: owner, command: command)
        do {
            let w = try JSONDecoder().decode(Wire.self, from: data)
            let revision = w.revision.value, searched = w.searched_files.value, retained = w.retained_hits.value
            let complete = w.search_complete
            guard w.query_generation.value == generation, generation > 0, revision > 0,
                  expected == nil || expected == revision, w.processed_batches.value == revision - 1,
                  w.query_hex == query.text.utf8.map({ String(format: "%02x", $0) }).joined(),
                  w.mode == query.mode.wire, w.caseMode == (query.matchCase ? "sensitive" : "unicode-lowercase"),
                  w.key_version.value == 1, w.identity_scope == "retained-query-session",
                  w.ordering == "ranked-observed-paths", w.policy == "product-defaults/no-rule-files-v1",
                  !w.source_payload_read, w.discovery_source_bytes_read.value == 0,
                  w.discovery_source_read_calls.value == 0,
                  ["searching", "paused", "complete", "partial"].contains(w.state),
                  w.paused == (w.state == "paused"),
                  w.more == (w.state == "searching" || w.state == "paused"),
                  complete == (w.state == "complete"), !w.pending_batch || w.more,
                  !complete || (w.discovery_complete && !w.pending_batch && !w.more
                    && w.discovery.unavailable.value == 0 && w.discovery.cycles.value == 0
                    && w.discovery.path_limited.value == 0 && w.discovery.depth_limited.value == 0
                    && w.discovery.queue_refused.value == 0),
                  searched <= w.catalogued_files.value, w.catalogued_files.value <= w.discovery.files.value,
                  w.files_examined.value <= searched, w.candidates_examined.value >= w.files_examined.value,
                  w.matches_seen.value <= searched, retained == UInt64(w.hits.count),
                  retained <= 256, retained <= w.matches_seen.value,
                  w.truncated == (w.matches_seen.value > retained),
                  w.retained_path_bytes.value <= 524_288,
                  w.limits.max_results.value == 256, w.limits.max_path_bytes.value == 2048,
                  w.limits.max_retained_path_bytes.value == 524_288,
                  w.limits.max_batches_per_advance.value == 16,
                  w.limits.response_bytes.value == UInt64(AtlasProgressiveFileWire.maximumResponseBytes),
                  w.replay_after_revision == nil || (w.replay_after_revision!.value > 0
                    && w.replay_after_revision!.value < revision) else {
                throw AtlasProgressiveFileError.invalidResponse
            }
            var ids = Set<UInt64>(), paths = Set<[UInt8]>(), bytes = 0
            for row in w.hits {
                guard row.id <= w.catalogued_files.value, ids.insert(row.id).inserted,
                      paths.insert(row.rawPath).inserted else { throw AtlasProgressiveFileError.invalidResponse }
                bytes += row.rawPath.count
            }
            guard UInt64(bytes) == w.retained_path_bytes.value else { throw AtlasProgressiveFileError.invalidResponse }
            return Self(owner: owner, generation: generation, revision: revision,
                catalogued: w.catalogued_files.value, searched: searched, matches: w.matches_seen.value,
                examined: w.files_examined.value, candidates: w.candidates_examined.value, work: w.work_units.value,
                retainedPathBytes: w.retained_path_bytes.value, processedBatches: w.processed_batches.value,
                replayAfter: w.replay_after_revision?.value, query: query, state: w.state, more: w.more,
                paused: w.paused, discoveryComplete: w.discovery_complete, complete: complete,
                pending: w.pending_batch, truncated: w.truncated,
                unavailable: w.discovery.unavailable.value, cycles: w.discovery.cycles.value,
                pathLimited: w.discovery.path_limited.value, depthLimited: w.discovery.depth_limited.value,
                queueRefused: w.discovery.queue_refused.value, excluded: w.discovery.excluded_entries.value,
                symlinks: w.discovery.symlinks_not_followed.value, special: w.discovery.special_objects.value,
                rows: w.hits)
        } catch let error as AtlasProgressiveFileError { throw error }
        catch { throw AtlasProgressiveFileError.invalidResponse }
    }
    private struct Limits: Decodable {
        let max_results, max_path_bytes, max_retained_path_bytes, max_batches_per_advance, response_bytes: AtlasFinderNumber
    }
    private struct Discovery: Decodable {
        let files, directories, excluded_entries, symlinks_not_followed, special_objects: AtlasFinderNumber
        let unavailable, cycles, path_limited, depth_limited, queue_refused: AtlasFinderNumber
    }
    private struct Hits: Decodable {
        let values: [AtlasProgressiveFileRow]
        init(from decoder: Decoder) throws {
            var c = try decoder.unkeyedContainer()
            guard c.count.map({ $0 <= 256 }) ?? true else { throw AtlasProgressiveFileError.invalidResponse }
            var values: [AtlasProgressiveFileRow] = []; values.reserveCapacity(c.count ?? 0)
            while !c.isAtEnd {
                guard values.count < 256 else { throw AtlasProgressiveFileError.invalidResponse }
                values.append(try c.decode(AtlasProgressiveFileRow.self))
            }
            self.values = values
        }
    }
    private struct Wire: Decodable {
        let query_generation, revision, key_version: AtlasFinderNumber
        let query_hex, mode, caseMode, identity_scope, ordering, policy, state: String
        let source_payload_read, more, paused, discovery_complete, search_complete, pending_batch, truncated: Bool
        let discovery_source_bytes_read, discovery_source_read_calls: AtlasFinderNumber
        let catalogued_files, searched_files, matches_seen, files_examined, candidates_examined, work_units: AtlasFinderNumber
        let retained_hits, retained_path_bytes, processed_batches: AtlasFinderNumber
        let replay_after_revision: AtlasFinderNumber?
        let limits: Limits
        let discovery: Discovery
        private let boundedHits: Hits
        var hits: [AtlasProgressiveFileRow] { boundedHits.values }
        enum CodingKeys: String, CodingKey {
            case query_generation, revision, key_version, query_hex, mode, identity_scope, ordering, policy, state
            case source_payload_read, more, paused, discovery_complete, search_complete, pending_batch, truncated
            case discovery_source_bytes_read, discovery_source_read_calls, catalogued_files, searched_files
            case matches_seen, files_examined, candidates_examined, work_units, retained_hits, retained_path_bytes
            case processed_batches, replay_after_revision, limits, discovery
            case boundedHits = "hits"
            case caseMode = "case"
        }
    }
}

enum AtlasProgressiveFileWire {
    static let maximumResponseBytes = 4 * 1024 * 1024
    static func admit(_ json: String?, owner: UInt64, command: String) throws -> Data {
        guard let json, !json.isEmpty, json.utf8.count <= maximumResponseBytes else { throw AtlasProgressiveFileError.unavailable }
        let data = Data(json.utf8)
        struct Failure: Decodable { let code: String }
        struct Header: Decodable {
            let schema, status: String
            let command: String?
            let owner: AtlasFinderNumber
            let error: Failure?
        }
        do {
            let w = try JSONDecoder().decode(Header.self, from: data)
            guard w.schema == "fcb.file-finder-session/1", w.owner.value == owner else {
                throw AtlasProgressiveFileError.invalidResponse
            }
            if w.status == "error", let error = w.error, error.code.utf8.count <= 512 {
                throw AtlasProgressiveFileError.engine(error.code)
            }
            guard w.status == "ok", w.command == command else { throw AtlasProgressiveFileError.invalidResponse }
            return data
        } catch let error as AtlasProgressiveFileError { throw error }
        catch { throw AtlasProgressiveFileError.invalidResponse }
    }
}

enum AtlasProgressiveFileRequest: Sendable {
    case open(root: String, generation: UInt64, query: AtlasFileQuery)
    case info
    case page(generation: UInt64, revision: UInt64)
    case advance(generation: UInt64, after: UInt64, batches: UInt64)
    case stop(generation: UInt64)
    case openReader(reader: UInt64, generation: UInt64, revision: UInt64, file: UInt64, bytes: UInt64)
}
struct AtlasProgressiveFileTransport: Sendable {
    let create: @Sendable () -> UInt64
    let run: @Sendable (UInt64, AtlasProgressiveFileRequest) -> String?
    let cancel: @Sendable (UInt64) -> Bool
    let close: @Sendable (UInt64) -> Bool
    let retirementFailed: @Sendable () -> Void
    let readerDomain: UUID?
}

private struct AtlasProgressiveFileReceipt: Sendable {
    let readerJSON: String
    let window: AtlasReaderPage
    static func decode(_ json: String?, report: AtlasProgressiveFileReport,
        row: AtlasProgressiveFileRow, destination: UInt64) throws -> Self {
        let data = try AtlasProgressiveFileWire.admit(json, owner: report.owner, command: "open-reader")
        do {
            // Every receipt repeats immutable query identity and current result
            // revision. A row from another ranking snapshot cannot authorize I/O.
            struct Selection: Decodable {
                let selection: AtlasProgressiveFileRow
                let query_generation, revision, key_version: AtlasFinderNumber
                let query_hex, mode, caseMode, identity_scope, ordering, policy: String
                let source_payload_read: Bool
                let discovery_source_bytes_read, discovery_source_read_calls: AtlasFinderNumber
                let catalogued_files, searched_files, matches_seen, retained_hits, retained_path_bytes: AtlasFinderNumber
                let reader_owner, source_bytes_read, source_read_calls: AtlasFinderNumber
                let source_observation: String
                enum CodingKeys: String, CodingKey {
                    case selection, query_generation, revision, key_version, query_hex, mode
                    case identity_scope, ordering, policy, source_payload_read
                    case discovery_source_bytes_read, discovery_source_read_calls
                    case catalogued_files, searched_files, matches_seen, retained_hits, retained_path_bytes
                    case reader_owner, source_bytes_read, source_read_calls, source_observation
                    case caseMode = "case"
                }
            }
            let selected = try JSONDecoder().decode(Selection.self, from: data)
            guard selected.query_generation.value == report.generation, selected.revision.value == report.revision,
                  selected.key_version.value == 1,
                  selected.query_hex == report.query.text.utf8.map({ String(format: "%02x", $0) }).joined(),
                  selected.mode == report.query.mode.wire,
                  selected.caseMode == (report.query.matchCase ? "sensitive" : "unicode-lowercase"),
                  selected.identity_scope == "retained-query-session", selected.ordering == "ranked-observed-paths",
                  selected.policy == "product-defaults/no-rule-files-v1", selected.source_payload_read,
                  selected.discovery_source_bytes_read.value == 0, selected.discovery_source_read_calls.value == 0,
                  selected.catalogued_files.value == report.catalogued, selected.searched_files.value == report.searched,
                  selected.matches_seen.value == report.matches, selected.retained_hits.value == UInt64(report.rows.count),
                  selected.retained_path_bytes.value == report.retainedPathBytes,
                  selected.selection.matches(row), destination > 0, destination != report.owner,
                  selected.reader_owner.value == destination,
                  selected.source_observation == "new-capture-after-metadata-selection",
                  let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let nested = object["reader"] as? [String: Any], let window = object["window"] as? [String: Any] else {
                throw AtlasProgressiveFileError.invalidResponse
            }
            func encoded(_ value: [String: Any]) throws -> String {
                String(decoding: try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]), as: UTF8.self)
            }
            let readerJSON = try encoded(nested)
            let reader = try AtlasReaderInfo.decodeRetainedInitialization(readerJSON, handle: destination, pathBytes: row.rawPath)
            let bytes = selected.source_bytes_read.value, calls = selected.source_read_calls.value
            guard bytes == reader.identity.capturedBytes, bytes <= AtlasReaderLimits.captureBytes,
                  (bytes == 0 && calls == 0) || (bytes > 0 && calls > 0 && calls <= 1_048_576) else {
                throw AtlasProgressiveFileError.invalidResponse
            }
            let first = try AtlasReaderPage.decode(encoded(window), info: reader, request: .window(offset: 0, bytes: 16_384))
            return Self(readerJSON: readerJSON, window: first)
        } catch let error as AtlasProgressiveFileError { throw error }
        catch { throw AtlasProgressiveFileError.invalidResponse }
    }
    func validate(_ page: AtlasReaderPage?) throws {
        guard let page, page.identity.matches(window.identity), page.start == window.start else {
            throw AtlasProgressiveFileError.invalidResponse
        }
        let common = Int(min(page.end, window.end) - page.start) * 2
        guard page.originalHex.utf8.prefix(common).elementsEqual(window.originalHex.utf8.prefix(common)) else {
            throw AtlasProgressiveFileError.invalidResponse
        }
    }
}
private final class AtlasProgressiveFileReceiptBox: @unchecked Sendable {
    private let lock = NSLock()
    private var receipt: AtlasProgressiveFileReceipt?
    func set(_ value: AtlasProgressiveFileReceipt) throws {
        lock.lock(); defer { lock.unlock() }
        guard receipt == nil else { throw AtlasProgressiveFileError.invalidResponse }
        receipt = value
    }
    func accepted() throws -> AtlasProgressiveFileReceipt {
        lock.lock(); defer { lock.unlock() }
        guard let receipt else { throw AtlasProgressiveFileError.invalidResponse }
        return receipt
    }
}
@MainActor final class AtlasProgressiveFileCapture: Identifiable {
    let id = UUID()
    let root: String
    let owner, generation, revision: UInt64
    let file: AtlasProgressiveFileRow
    let reader: AtlasPagedReaderModel
    init(root: String, report: AtlasProgressiveFileReport, file: AtlasProgressiveFileRow, reader: AtlasPagedReaderModel) {
        self.root = root; owner = report.owner; generation = report.generation; revision = report.revision
        self.file = file; self.reader = reader
    }
}
private final class AtlasProgressiveFileAccess: @unchecked Sendable {
    let reference: AnyObject?
    init(_ reference: AnyObject?) { self.reference = reference }
}
private final class AtlasProgressiveFileSession: @unchecked Sendable {
    let owner: UInt64
    let generation: UInt64 = 1
    let query: AtlasFileQuery
    let transport: AtlasProgressiveFileTransport
    private let access: AtlasProgressiveFileAccess
    private static let retirement = DispatchQueue(label: "dev.frankencode.browser.progressive-finder-retirement", qos: .utility)
    init(query: AtlasFileQuery, transport: AtlasProgressiveFileTransport, access: AnyObject?) throws {
        let owner = transport.create()
        guard owner != 0 else { throw AtlasProgressiveFileError.unavailable }
        self.owner = owner; self.query = query; self.transport = transport; self.access = AtlasProgressiveFileAccess(access)
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

/// One entered worker operation and one newest pending input. Each query owns
/// a fresh bounded Rust traversal; edits never accumulate full path catalogs.
/// Empty progress snapshots advance automatically until useful hits or Stop.
@Observable @MainActor final class AtlasProgressiveQuickOpenModel {
    var text = "" { didSet { if !oldValue.utf8.elementsEqual(text.utf8) { submit(debounce: true) } } }
    var mode: AtlasFileFindMode = .fuzzy { didSet { if oldValue != mode { submit(debounce: true) } } }
    var matchCase = false { didSet { if oldValue != matchCase { submit(debounce: true) } } }
    var selected: AtlasProgressiveFileSelection? {
        didSet { if opening && oldValue != selected { flag?.cancel(); session?.cancel() } }
    }
    private(set) var report: AtlasProgressiveFileReport?
    private(set) var root = ""
    private(set) var busy = false
    private(set) var opening = false
    private(set) var closed = false
    private(set) var notice = "Find a filename or path without reading source contents."
    @ObservationIgnored private let transport: AtlasProgressiveFileTransport
    @ObservationIgnored private let io = AtlasProjectIO()
    @ObservationIgnored private var access: AnyObject?
    @ObservationIgnored private var session: AtlasProgressiveFileSession?
    @ObservationIgnored private var flag: AtlasSearchCancellation?
    @ObservationIgnored private var task: Task<Void, Never>?
    @ObservationIgnored private var version = UUID()
    @ObservationIgnored private var debouncing = false
    @ObservationIgnored private var pending: (version: UUID, query: AtlasFileQuery, debounce: Bool)?
    @ObservationIgnored private var stopRequested = false
    @ObservationIgnored private var hasPresentedHits = false

    init(transport: AtlasProgressiveFileTransport) { self.transport = transport }
    var isCurrent: Bool {
        guard !closed, let report, let session, let query = try? input() else { return false }
        return report.owner == session.owner && report.generation == session.generation
            && report.query == query && session.query == query
    }
    var canOpen: Bool { !busy && isCurrent && selected.flatMap { report?.row($0) } != nil }
    var canContinue: Bool { !busy && isCurrent && report?.more == true }
    var canReload: Bool { !busy && !closed && session != nil && session?.query == (try? input()) }
    func bind(root: String, accessLease: AnyObject?) {
        guard self.root.isEmpty, !closed, root.hasPrefix("/"), root.utf8.count <= 16_384,
              !root.utf8.contains(0) else { notice = AtlasProgressiveFileError.unavailable.message; return }
        self.root = root; access = accessLease
        if !text.isEmpty { submit() }
    }
    func submit(debounce: Bool = false) {
        guard !closed else { return }
        version = UUID(); pending = nil; stopRequested = false
        report = nil; selected = nil; hasPresentedHits = false
        flag?.cancel(); session?.cancel()
        if debouncing { task?.cancel() }
        guard !root.isEmpty, !text.isEmpty else {
            notice = "Find a filename or path without reading source contents."
            if !busy { session = nil }
            return
        }
        do { pending = (version, try input(), debounce) }
        catch { notice = Self.message(error); if !busy { session = nil }; return }
        notice = busy ? "Replacing the previous search after entered work drains…" : "Finding project files…"
        if !busy { session = nil; startPending() }
    }
    func continueSearch() {
        guard canContinue, let report, let lease = session else { return }
        let ticket = version
        perform { model, flag in
            do {
                let next = try await model.advance(lease, after: report.revision, flag: flag)
                guard model.accepts(lease, ticket: ticket) else { return }
                model.accept(next)
                while let current = model.report, current.rows.isEmpty, current.more, !flag.isCanceled,
                      model.accepts(lease, ticket: ticket) {
                    await Task.yield()
                    let next = try await model.advance(lease, after: current.revision, flag: flag)
                    guard model.accepts(lease, ticket: ticket) else { return }
                    model.accept(next)
                }
            } catch {
                guard model.accepts(lease, ticket: ticket) else { return }
                model.notice = await model.recover(lease, ticket: ticket)
                    ? "Reloaded accepted progress after the interrupted step. Continue when ready." : Self.message(error)
            }
        }
    }
    func reload() {
        guard canReload, let lease = session else { return }
        let ticket = version
        perform { model, _ in
            model.notice = await model.recover(lease, ticket: ticket)
                ? "Reloaded accepted results without searching more files."
                : "Accepted results are unavailable. Start this search again."
        }
    }
    func stop() {
        guard !closed else { return }
        pending = nil
        if busy {
            stopRequested = true; flag?.cancel(); session?.cancel()
            if debouncing { task?.cancel() }
            notice = "Stopping after entered work drains…"
            return
        }
        pause()
    }
    func move(backwards: Bool) {
        guard !busy, isCurrent, let report, !report.rows.isEmpty else { return }
        let index = selected.flatMap { id in report.rows.firstIndex { $0.id == id.file && report.row(id) != nil } }
        let next = index.map { backwards ? max(0, $0 - 1) : min(report.rows.count - 1, $0 + 1) }
            ?? (backwards ? report.rows.count - 1 : 0)
        selected = report.selection(report.rows[next].id)
    }
    func openSelected(readerTransport: AtlasReaderTransport, search: AtlasReaderSearchTransport? = nil,
        outline: AtlasReaderOutlineTransport? = nil, document: AtlasReaderDocumentTransport? = nil,
        deliver: @escaping @MainActor (AtlasProgressiveFileCapture) -> Void) {
        guard canOpen, let report, let id = selected, let row = report.row(id), let lease = session,
              let domain = transport.readerDomain, readerTransport.comparison?.domain == domain else { return }
        let ticket = version, root = root
        opening = true
        perform { model, flag in
            let reader = AtlasPagedReaderModel(transport: readerTransport, search: search, outline: outline, document: document)
            do {
                let receipt = AtlasProgressiveFileReceiptBox()
                try await reader.openRetained(pathBytes: row.rawPath, offset: 0, registryDomain: domain) { destination in
                    guard !flag.isCanceled else { throw AtlasProgressiveFileError.canceled }
                    return try withExtendedLifetime(lease) {
                        let admitted = try AtlasProgressiveFileReceipt.decode(lease.transport.run(lease.owner,
                            .openReader(reader: destination, generation: report.generation, revision: report.revision,
                                file: row.id, bytes: AtlasReaderLimits.captureBytes)),
                            report: report, row: row, destination: destination)
                        try receipt.set(admitted)
                        return admitted.readerJSON
                    }
                }
                guard model.accepts(lease, ticket: ticket), !flag.isCanceled, model.selected == id,
                      model.report?.revision == report.revision else { throw AtlasProgressiveFileError.canceled }
                try receipt.accepted().validate(reader.page)
                deliver(AtlasProgressiveFileCapture(root: root, report: report, file: row, reader: reader))
            } catch {
                reader.close()
                if model.accepts(lease, ticket: ticket) { model.notice = Self.message(error) }
            }
        }
    }
    func close() {
        guard !closed else { return }
        closed = true; version = UUID(); pending = nil; stopRequested = false
        flag?.cancel(); session?.cancel()
        if debouncing { task?.cancel() }
        report = nil; selected = nil; access = nil
        if !busy { session = nil }
    }
    private func input() throws -> AtlasFileQuery { try .init(text: text, mode: mode, matchCase: matchCase) }
    private func accepts(_ lease: AtlasProgressiveFileSession, ticket: UUID) -> Bool {
        !closed && version == ticket && session === lease && (try? input()) == lease.query
    }
    private func startPending() {
        guard !busy, !closed, let request = pending else { return }
        pending = nil; busy = true; debouncing = request.debounce
        let flag = AtlasSearchCancellation(); self.flag = flag
        task = Task {
            defer { finish() }
            do {
                if request.debounce { try await Task.sleep(nanoseconds: 150_000_000) }
                guard !closed, version == request.version, !flag.isCanceled else { return }
                debouncing = false
                let lease = try AtlasProgressiveFileSession(query: request.query, transport: transport, access: access)
                session = lease
                do {
                    let initial = try AtlasProgressiveFileReport.decode(await run(lease,
                        .open(root: root, generation: lease.generation, query: lease.query), flag: flag),
                        owner: lease.owner, generation: lease.generation, query: lease.query, command: "page", revision: 1)
                    guard accepts(lease, ticket: request.version) else { return }
                    accept(initial)
                    while let current = report, current.rows.isEmpty, current.more, !flag.isCanceled,
                          accepts(lease, ticket: request.version) {
                        await Task.yield()
                        let next = try await advance(lease, after: current.revision, flag: flag)
                        guard accepts(lease, ticket: request.version) else { return }
                        accept(next)
                    }
                } catch {
                    guard accepts(lease, ticket: request.version) else { return }
                    notice = await recover(lease, ticket: request.version)
                        ? "Reloaded accepted progress after the interrupted search. Continue when ready." : Self.message(error)
                }
            } catch {
                if !closed, version == request.version { notice = Self.message(error) }
            }
        }
    }
    private func advance(_ lease: AtlasProgressiveFileSession, after: UInt64,
        flag: AtlasSearchCancellation) async throws -> AtlasProgressiveFileReport {
        guard after < UInt64.max else { throw AtlasProgressiveFileError.stale }
        let next = try AtlasProgressiveFileReport.decode(await run(lease,
            .advance(generation: lease.generation, after: after, batches: 16), flag: flag),
            owner: lease.owner, generation: lease.generation, query: lease.query, command: "page")
        guard next.revision > after, next.revision - after <= 16, next.replayAfter == after else {
            throw AtlasProgressiveFileError.invalidResponse
        }
        return next
    }
    private func accept(_ report: AtlasProgressiveFileReport) {
        let previous = selected
        self.report = report
        notice = report.summary
        if let previous, report.row(previous) == nil {
            selected = nil
            notice += " The selected result left the retained best matches. Choose a result to open."
        } else if !hasPresentedHits, let first = report.rows.first {
            selected = report.selection(first.id)
        }
        if !report.rows.isEmpty { hasPresentedHits = true }
    }
    private func recover(_ lease: AtlasProgressiveFileSession, ticket: UUID) async -> Bool {
        guard accepts(lease, ticket: ticket) else { return false }
        let recovery = AtlasSearchCancellation()
        do {
            let info = try AtlasProgressiveFileReport.decode(await run(lease, .info, flag: recovery),
                owner: lease.owner, generation: lease.generation, query: lease.query, command: "info")
            if let current = report, current.owner == lease.owner, info.revision < current.revision {
                throw AtlasProgressiveFileError.invalidResponse
            }
            let page = try AtlasProgressiveFileReport.decode(await run(lease,
                .page(generation: lease.generation, revision: info.revision), flag: recovery),
                owner: lease.owner, generation: lease.generation, query: lease.query, command: "page", revision: info.revision)
            guard accepts(lease, ticket: ticket) else { return false }
            accept(page); return true
        } catch {
            if accepts(lease, ticket: ticket) { report = nil; selected = nil }
            return false
        }
    }
    private func pause() {
        guard !busy, !closed, let lease = session, let current = report, current.more else { return }
        let ticket = version
        perform { model, flag in
            do {
                let report = try AtlasProgressiveFileReport.decode(await model.run(lease,
                    .stop(generation: lease.generation), flag: flag),
                    owner: lease.owner, generation: lease.generation, query: lease.query, command: "stop", revision: current.revision)
                guard model.accepts(lease, ticket: ticket) else { return }
                model.accept(report)
            } catch {
                guard model.accepts(lease, ticket: ticket) else { return }
                _ = await model.recover(lease, ticket: ticket); model.notice = Self.message(error)
            }
        }
    }
    private func run(_ lease: AtlasProgressiveFileSession, _ request: AtlasProgressiveFileRequest,
        flag: AtlasSearchCancellation) async throws -> String? {
        try await io.perform(cancellation: flag, keepingAlive: [lease]) { lease.transport.run(lease.owner, request) }
    }
    private func perform(_ work: @escaping @MainActor (AtlasProgressiveQuickOpenModel, AtlasSearchCancellation) async throws -> Void) {
        guard !busy, !closed else { return }
        busy = true; debouncing = false
        let flag = AtlasSearchCancellation(); self.flag = flag
        task = Task {
            do { try await work(self, flag) }
            catch { if !closed { notice = Self.message(error) } }
            finish()
        }
    }
    private func finish() {
        busy = false; opening = false; debouncing = false; flag = nil; task = nil
        if closed { session = nil; access = nil }
        else if pending != nil { session = nil; startPending() }
        else if session?.query != (try? input()) { session = nil; stopRequested = false }
        else if stopRequested { stopRequested = false; pause() }
    }
    private static func message(_ error: Error) -> String {
        if let error = error as? AtlasProgressiveFileError { return error.message }
        if let error = error as? AtlasFileFinderError { return error.message }
        if let error = error as? AtlasReaderError { return error.message }
        if error is CancellationError { return AtlasProgressiveFileError.canceled.message }
        if let error = error as? AtlasProjectIOError, case .canceled = error { return AtlasProgressiveFileError.canceled.message }
        return AtlasProgressiveFileError.invalidResponse.message
    }
}
