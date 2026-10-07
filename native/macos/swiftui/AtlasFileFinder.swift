import Foundation
import Dispatch

/// Native marshaling/scheduling of the shared path index, never a second ranker.
enum AtlasFileFinderError: Error, Sendable {
    case invalidInput, unavailable, invalidResponse, canceled, exhausted
    case engine(String)
    var message: String {
        switch self {
        case .invalidInput: return "Enter 1–256 UTF-8 bytes of filename or path text without NUL."
        case .unavailable: return "File finding is unavailable. Retry or refresh the file catalog."
        case .invalidResponse: return "The file results could not be verified. No file was opened."
        case .canceled: return "File finding canceled."
        case .exhausted: return "This file catalog exhausted its request identities. Refresh it before searching again."
        case .engine(let code): return "File finding unavailable (\(code))."
        }
    }
}
enum AtlasFileFindMode: UInt8, CaseIterable, Sendable {
    case fuzzy = 0, exact = 1, prefix = 2
    var label: String { switch self { case .fuzzy: "Fuzzy"; case .exact: "Exact"; case .prefix: "Prefix" } }
    var wire: String { label.lowercased() }
}
struct AtlasFileQuery: Sendable, Equatable {
    let text: String
    let mode: AtlasFileFindMode
    let matchCase: Bool
    init(text: String, mode: AtlasFileFindMode = .fuzzy, matchCase: Bool = false) throws {
        guard !text.isEmpty, text.utf8.count <= 256, !text.utf8.contains(0) else { throw AtlasFileFinderError.invalidInput }
        self.text = text; self.mode = mode; self.matchCase = matchCase
    }
    static func == (a: Self, b: Self) -> Bool {
        a.mode == b.mode && a.matchCase == b.matchCase && a.text.utf8.elementsEqual(b.text.utf8)
    }
}
struct AtlasFileFinderRow: Sendable, Identifiable, Equatable {
    let id: UInt64
    let node: UInt64
    let rawPath: [UInt8]
    let display: String
    let kind: String
    var path: String? { String(bytes: rawPath, encoding: .utf8) }
    var explanation: String { kind.replacingOccurrences(of: "-", with: " ") }
}
struct AtlasFileRowID: Hashable, Sendable { let report: UUID; let file: UInt64 }
struct AtlasFileFinderReport: Sendable {
    let id: UUID
    let catalog: UUID
    let root: String
    let query: AtlasFileQuery
    let generation, owner, manifest, layout, cataloguedFiles, matchesSeen: UInt64
    let discoveryComplete, complete, truncated: Bool
    let rows: [AtlasFileFinderRow]
    var summary: String {
        var text = "\(rows.count) of \(matchesSeen) matching files shown · \(cataloguedFiles) catalogued."
        if !discoveryComplete { text += " Discovery is partial; additional files may exist." }
        else if !complete { text += " Search coverage is partial." }
        if truncated { text += " Result limit reached; refine the filename or path." }
        return text + " Frozen catalog; Refresh discovers changes. No source contents searched."
    }
    func row(_ id: AtlasFileRowID) -> AtlasFileFinderRow? {
        guard id.report == self.id else { return nil }
        return rows.first { $0.id == id.file }
    }
    func rowID(_ file: UInt64) -> AtlasFileRowID { .init(report: id, file: file) }
}
struct AtlasFileOpenChoice: Sendable {
    let catalog: UUID
    let report: UUID
    let root, path: String
}
struct AtlasFileFinderTransport: Sendable {
    let create: @Sendable () -> UInt64
    let open: @Sendable (UInt64, String, UInt64, AtlasSearchCancellation) -> String?
    let find: @Sendable (UInt64, UInt64, AtlasFileQuery, UInt64, AtlasSearchCancellation) -> String?
    let page: @Sendable (UInt64, UInt64, UInt64, UInt64) -> String?
    let select: @Sendable (UInt64, UInt64, UInt64) -> String?
    let close: @Sendable (UInt64) -> Bool
    let retirementFailed: @Sendable () -> Void
}

private final class AtlasFileAccess: @unchecked Sendable {
    let reference: AnyObject?
    init(_ reference: AnyObject?) { self.reference = reference }
}
private final class AtlasFileCatalog: @unchecked Sendable {
    private static let retirement = DispatchQueue(label: "dev.frankencode.browser.file-catalog-retirement", qos: .utility)
    let handle: UInt64
    let transport: AtlasFileFinderTransport
    let access: AtlasFileAccess
    init(handle: UInt64, transport: AtlasFileFinderTransport, access: AtlasFileAccess) {
        self.handle = handle; self.transport = transport; self.access = access
    }
    deinit {
        let handle = handle, transport = transport, access = access
        Self.retirement.async {
            withExtendedLifetime(access) {
                // The existing Rust registry caps live atlas owners at four.
                // Never spin indefinitely or destroy catalog geometry on input.
                for attempt in 0..<8 {
                    if transport.close(handle) { return }
                    if attempt < 7 { Thread.sleep(forTimeInterval: 0.001) }
                }
                transport.retirementFailed()
            }
        }
    }
}

/// Inert until the first explicit request. Only a host worker calls find/select;
/// NSLock serializes those operations, never an AppKit input callback. One
/// frozen catalog and the Rust path keys are reused for every query refinement.
/// A fresh instance is required for an explicit refresh or a different root.
final class AtlasFileFinder: @unchecked Sendable {
    static let maximumFiles: UInt64 = 20_000
    static let maximumResults: UInt64 = 256
    let catalogID = UUID()
    private let root: String
    private let access: AtlasFileAccess
    private let transport: AtlasFileFinderTransport
    private let lock = NSLock()
    private var session: AtlasFileCatalog?
    private var metadata: FileCatalogInfo?
    private var manifest: UInt64?
    private var generation: UInt64 = 0
    private var accepted: AtlasFileFinderReport?

    init(root: String, accessLease: AnyObject? = nil, transport: AtlasFileFinderTransport) throws {
        guard !root.isEmpty, root.utf8.count <= 16_384, !root.utf8.contains(0) else { throw AtlasFileFinderError.invalidInput }
        self.root = root; self.access = AtlasFileAccess(accessLease); self.transport = transport
    }

    func find(_ query: AtlasFileQuery, cancellation: AtlasSearchCancellation) throws -> AtlasFileFinderReport {
        lock.lock(); defer { lock.unlock() }
        try check(cancellation)
        let (next, overflow) = generation.addingReportingOverflow(1)
        guard !overflow else { throw AtlasFileFinderError.exhausted }
        generation = next
        accepted = nil
        if session == nil {
            let handle = transport.create()
            guard handle != 0 else { throw AtlasFileFinderError.unavailable }
            let candidate = AtlasFileCatalog(handle: handle, transport: transport, access: access)
            let opened = transport.open(handle, root, Self.maximumFiles, cancellation)
            try check(cancellation)
            let info = try FileCatalogInfo.decode(opened, handle: handle)
            try check(cancellation)
            metadata = info; session = candidate
        }
        guard let session, let metadata else { throw AtlasFileFinderError.unavailable }
        try check(cancellation)
        let found = transport.find(session.handle, next, query, Self.maximumResults, cancellation)
        try check(cancellation)
        let first = try FileResultPage.decode(found,
            handle: session.handle, query: query, generation: next, info: metadata, command: "find")
        try check(cancellation)
        if let manifest, manifest != first.manifest { throw AtlasFileFinderError.invalidResponse }
        var rows: [AtlasFileFinderRow] = []
        var ids = Set<UInt64>(), paths = Set<Data>()
        var textBytes = 0
        func append(_ page: FileResultPage, start: UInt64, limit: UInt64) throws {
            guard page.sameResult(first), start == UInt64(rows.count), start <= first.retained else { throw AtlasFileFinderError.invalidResponse }
            let end = min(start + limit, first.retained)
            guard UInt64(page.rows.count) == end - start,
                  page.next == (end < first.retained ? end : nil) else { throw AtlasFileFinderError.invalidResponse }
            for row in page.rows {
                try check(cancellation)
                guard ids.insert(row.id).inserted, paths.insert(Data(row.rawPath)).inserted else { throw AtlasFileFinderError.invalidResponse }
                textBytes += row.rawPath.count + row.display.utf8.count + 256
                guard textBytes <= 8 * 1024 * 1024 else { throw AtlasFileFinderError.invalidResponse }
                rows.append(row)
            }
        }
        try append(first, start: 0, limit: 64)
        while UInt64(rows.count) < first.retained {
            try check(cancellation)
            let offset = UInt64(rows.count)
            let page = try FileResultPage.decode(transport.page(session.handle, next, offset, 128),
                handle: session.handle, query: query, generation: next, info: metadata, command: "page")
            try append(page, start: offset, limit: 128)
        }
        try check(cancellation)
        let report = AtlasFileFinderReport(id: UUID(), catalog: catalogID, root: root, query: query,
            generation: next, owner: session.handle, manifest: first.manifest, layout: metadata.layout,
            cataloguedFiles: metadata.files, matchesSeen: first.matches, discoveryComplete: metadata.complete,
            complete: first.complete, truncated: first.truncated, rows: rows)
        manifest = first.manifest; accepted = report
        return report
    }

    /// Resolve one CURRENT file ID through Rust before handing the raw path to
    /// the app's ordinary file opener. This selects metadata only; opening is a
    /// separate new source observation, not a retained-content search jump.
    func select(_ id: AtlasFileRowID, cancellation: AtlasSearchCancellation) throws -> AtlasFileOpenChoice {
        lock.lock(); defer { lock.unlock() }
        try check(cancellation)
        guard let session, let report = accepted, let row = report.row(id), let path = row.path else {
            throw AtlasFileFinderError.invalidInput
        }
        let data = try FileFinderWire.admit(transport.select(session.handle, report.generation, row.id), handle: session.handle)
        struct Wire: Decodable {
            let schema, status, command, owner, source_manifest, layout_revision, query_generation: String
            let source_payload_read: Bool
            let selection: FileFinderWire.Row
        }
        do {
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            guard wire.schema == "fcb.atlas-paths/1", wire.status == "ok", wire.command == "select",
                  try FileFinderWire.number(wire.owner) == report.owner,
                  try FileFinderWire.number(wire.source_manifest) == report.manifest,
                  try FileFinderWire.number(wire.layout_revision) == report.layout,
                  try FileFinderWire.number(wire.query_generation) == report.generation,
                  !wire.source_payload_read, try wire.selection.value() == row else { throw AtlasFileFinderError.invalidResponse }
        } catch { throw AtlasFileFinderError.invalidResponse }
        try check(cancellation)
        return .init(catalog: catalogID, report: report.id, root: root, path: path)
    }
    private func check(_ flag: AtlasSearchCancellation) throws {
        if flag.isCanceled { throw AtlasFileFinderError.canceled }
    }
}

private struct FileCatalogInfo {
    let layout, files: UInt64
    let complete: Bool
    static func decode(_ json: String?, handle: UInt64) throws -> Self {
        let data = try FileFinderWire.admit(json, handle: handle)
        struct Wire: Decodable {
            let schema, status, command, owner, layout_revision, catalogued_files, scope, retention: String
            let discovery_complete: Bool
        }
        do {
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            let layout = try FileFinderWire.number(wire.layout_revision), files = try FileFinderWire.number(wire.catalogued_files)
            guard wire.schema == "fcb.atlas-session/1", wire.status == "ok", wire.command == "info",
                  try FileFinderWire.number(wire.owner) == handle, layout > 0, files <= AtlasFileFinder.maximumFiles,
                  wire.scope == "all", wire.retention == "frozen-catalog-and-spatial-index" else { throw AtlasFileFinderError.invalidResponse }
            return Self(layout: layout, files: files, complete: wire.discovery_complete)
        } catch { throw AtlasFileFinderError.invalidResponse }
    }
}
private struct FileResultPage {
    let manifest, retained, matches, examined, candidates, work: UInt64
    let complete, truncated: Bool
    let next: UInt64?
    let rows: [AtlasFileFinderRow]
    func sameResult(_ other: Self) -> Bool {
        manifest == other.manifest && retained == other.retained && matches == other.matches
            && examined == other.examined && candidates == other.candidates && work == other.work
            && complete == other.complete && truncated == other.truncated
    }
    static func decode(_ json: String?, handle: UInt64, query: AtlasFileQuery, generation: UInt64,
                       info: FileCatalogInfo, command: String) throws -> Self {
        let data = try FileFinderWire.admit(json, handle: handle)
        do {
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            let manifest = try FileFinderWire.number(wire.source_manifest)
            let retained = try FileFinderWire.number(wire.retained_hits), matches = try FileFinderWire.number(wire.matches_seen)
            let examined = try FileFinderWire.number(wire.files_examined), candidates = try FileFinderWire.number(wire.candidates_examined)
            guard wire.schema == "fcb.atlas-paths/1", wire.status == "ok", wire.command == command,
                  try FileFinderWire.number(wire.owner) == handle, manifest > 0,
                  try FileFinderWire.number(wire.layout_revision) == info.layout,
                  try FileFinderWire.number(wire.query_generation) == generation,
                  !wire.source_payload_read, wire.query_hex == FileFinderWire.hex(query.text.utf8),
                  wire.mode == query.mode.wire, wire.case == (query.matchCase ? "sensitive" : "unicode-lowercase"),
                  retained <= AtlasFileFinder.maximumResults, retained <= matches, matches <= info.files,
                  examined <= info.files, candidates >= examined,
                  wire.truncated == (matches > retained),
                  !wire.search_complete || info.complete else { throw AtlasFileFinderError.invalidResponse }
            return Self(manifest: manifest, retained: retained, matches: matches, examined: examined, candidates: candidates,
                work: try FileFinderWire.number(wire.work_units), complete: wire.search_complete, truncated: wire.truncated,
                next: try wire.next_offset.map(FileFinderWire.number), rows: try wire.hits.map { try $0.value() })
        } catch { throw AtlasFileFinderError.invalidResponse }
    }
    private struct Wire: Decodable {
        let schema, status, command, owner, source_manifest, layout_revision, query_generation, query_hex, mode, `case`: String
        let source_payload_read, search_complete, truncated: Bool
        let retained_hits, matches_seen, files_examined, candidates_examined, work_units: String
        let next_offset: String?
        let hits: [FileFinderWire.Row]
        enum CodingKeys: String, CodingKey {
            case schema, status, command, owner, source_manifest, layout_revision, query_generation, query_hex, mode, `case`,
                 source_payload_read, search_complete, truncated, retained_hits, matches_seen, files_examined,
                 candidates_examined, work_units, next_offset, hits
        }
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            schema = try c.decode(String.self, forKey: .schema); status = try c.decode(String.self, forKey: .status)
            command = try c.decode(String.self, forKey: .command); owner = try c.decode(String.self, forKey: .owner)
            source_manifest = try c.decode(String.self, forKey: .source_manifest); layout_revision = try c.decode(String.self, forKey: .layout_revision)
            query_generation = try c.decode(String.self, forKey: .query_generation); query_hex = try c.decode(String.self, forKey: .query_hex)
            mode = try c.decode(String.self, forKey: .mode); self.case = try c.decode(String.self, forKey: .case)
            source_payload_read = try c.decode(Bool.self, forKey: .source_payload_read)
            search_complete = try c.decode(Bool.self, forKey: .search_complete); truncated = try c.decode(Bool.self, forKey: .truncated)
            retained_hits = try c.decode(String.self, forKey: .retained_hits); matches_seen = try c.decode(String.self, forKey: .matches_seen)
            files_examined = try c.decode(String.self, forKey: .files_examined); candidates_examined = try c.decode(String.self, forKey: .candidates_examined)
            work_units = try c.decode(String.self, forKey: .work_units); next_offset = try c.decodeIfPresent(String.self, forKey: .next_offset)
            var list = try c.nestedUnkeyedContainer(forKey: .hits)
            if let count = list.count, count > 128 { throw AtlasFileFinderError.invalidResponse }
            var rows: [FileFinderWire.Row] = []
            while !list.isAtEnd {
                guard rows.count < 128 else { throw AtlasFileFinderError.invalidResponse }
                rows.append(try list.decode(FileFinderWire.Row.self))
            }
            hits = rows
        }
    }
}
private enum FileFinderWire {
    static func admit(_ json: String?, handle: UInt64) throws -> Data {
        guard let json else { throw AtlasFileFinderError.unavailable }
        guard json.utf8.count <= 4 * 1024 * 1024, let data = json.data(using: .utf8) else { throw AtlasFileFinderError.invalidResponse }
        struct Status: Decodable { let status, owner: String; let error: Diagnostic? }
        struct Diagnostic: Decodable { let code: String }
        guard let value = try? JSONDecoder().decode(Status.self, from: data),
              (try? number(value.owner)) == handle, handle > 0 else { throw AtlasFileFinderError.invalidResponse }
        if value.status == "error" {
            guard let code = value.error?.code, !code.isEmpty, code.utf8.count <= 512,
                  code.unicodeScalars.allSatisfy({ $0.value >= 32 && $0.value != 127 }) else { throw AtlasFileFinderError.invalidResponse }
            throw AtlasFileFinderError.engine(code)
        }
        guard value.status == "ok", value.error == nil else { throw AtlasFileFinderError.invalidResponse }
        return data
    }
    static func number(_ text: String) throws -> UInt64 {
        guard let value = UInt64(text), String(value) == text else { throw AtlasFileFinderError.invalidResponse }
        return value
    }
    static func hex(_ bytes: some Collection<UInt8>) -> String {
        let digits = Array("0123456789abcdef".utf8)
        return String(decoding: bytes.flatMap { [digits[Int($0 >> 4)], digits[Int($0 & 15)]] }, as: UTF8.self)
    }
    struct Row: Decodable {
        let file_id, node, match_kind: String
        let path: Path
        struct Path: Decodable { let encoding, hex, display: String }
        func value() throws -> AtlasFileFinderRow {
            let file = try number(file_id), node = try number(node)
            let input = Array(path.hex.utf8)
            guard file > 0, node <= UInt32.max, path.encoding == "unix-bytes", !input.isEmpty,
                  input.count <= 32_768, input.count.isMultiple(of: 2), path.display.utf8.count <= 65_536,
                  ["exact-filename", "exact-path", "exact-component", "filename-prefix", "component-prefix",
                   "path-prefix", "filename-subsequence", "path-subsequence"].contains(match_kind) else { throw AtlasFileFinderError.invalidResponse }
            func nibble(_ byte: UInt8) throws -> UInt8 {
                switch byte { case 48...57: byte - 48; case 97...102: byte - 87; default: throw AtlasFileFinderError.invalidResponse }
            }
            var raw: [UInt8] = []; raw.reserveCapacity(input.count / 2)
            for offset in stride(from: 0, to: input.count, by: 2) {
                raw.append(try nibble(input[offset]) * 16 + nibble(input[offset + 1]))
            }
            guard raw.first != 47, !raw.contains(0), !raw.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
            }) else { throw AtlasFileFinderError.invalidResponse }
            return .init(id: file, node: node, rawPath: raw, display: path.display, kind: match_kind)
        }
    }
}
