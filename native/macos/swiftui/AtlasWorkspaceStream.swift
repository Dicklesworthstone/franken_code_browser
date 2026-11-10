import Foundation

enum AtlasWorkspaceStreamLimits {
    static let files: UInt64 = 20_000
    static let needleBytes = 1024
    static let hits: UInt64 = 256
    static let page: UInt64 = 32
    static let scanBytes: UInt64 = 256 * 1024 * 1024
    static let maximumScanBytes: UInt64 = 4 * 1024 * 1024 * 1024
    static let readCalls: UInt64 = 1_048_576
    static let responseBytes = 8 * 1024 * 1024
}

enum AtlasWorkspaceStreamError: Error, Sendable {
    case invalidInput, unavailable, invalidResponse, canceled
    case engine(String)
    var message: String {
        switch self {
        case .invalidInput: return "Enter exact text of 1–1024 UTF-8 bytes without NUL and choose a supported scan limit."
        case .unavailable: return "Large-file project search is unavailable. Close another search or refresh this file catalog."
        case .invalidResponse: return "The search response could not be verified. Previously accepted evidence is unchanged."
        case .canceled: return "Search canceled. Entered file work may still be draining."
        case .engine(let code): return "The requested search operation is unavailable (\(code))."
        }
    }
}

struct AtlasWorkspaceStreamRequest: Equatable, Sendable {
    let needle: String
    let maximumMatches, maximumScanBytes, maximumReadCalls: UInt64
    init(needle: String, maximumScanBytes: UInt64 = AtlasWorkspaceStreamLimits.scanBytes,
         maximumMatches: UInt64 = AtlasWorkspaceStreamLimits.hits,
         maximumReadCalls: UInt64 = AtlasWorkspaceStreamLimits.readCalls) throws {
        guard !needle.isEmpty, needle.utf8.count <= AtlasWorkspaceStreamLimits.needleBytes,
              !needle.utf8.contains(0), (1...AtlasWorkspaceStreamLimits.hits).contains(maximumMatches),
              (1...AtlasWorkspaceStreamLimits.maximumScanBytes).contains(maximumScanBytes),
              (1...AtlasWorkspaceStreamLimits.readCalls).contains(maximumReadCalls) else {
            throw AtlasWorkspaceStreamError.invalidInput
        }
        self.needle = needle; self.maximumScanBytes = maximumScanBytes
        self.maximumMatches = maximumMatches; self.maximumReadCalls = maximumReadCalls
    }
    static func == (a: Self, b: Self) -> Bool {
        a.needle.utf8.elementsEqual(b.needle.utf8) && a.maximumMatches == b.maximumMatches
            && a.maximumScanBytes == b.maximumScanBytes && a.maximumReadCalls == b.maximumReadCalls
    }
}

/// A cooperative stop keeps this search's finalized literal evidence. It is
/// separate from cancellation, which revokes delivery and discards the attempt.
final class AtlasWorkspaceStreamStop: @unchecked Sendable {
    private let lock = NSLock()
    private var requested = false
    var isRequested: Bool { lock.lock(); defer { lock.unlock() }; return requested }
    func requestStop() { lock.lock(); requested = true; lock.unlock() }
}

struct AtlasWorkspaceStreamTransport: Sendable {
    typealias ProgressiveSearch = @Sendable (UInt64, AtlasWorkspaceStreamRequest,
        AtlasSearchCancellation, AtlasWorkspaceStreamStop, @Sendable (String) -> Void) throws -> String?
    let create: @Sendable () -> UInt64
    let open: @Sendable (UInt64, String, AtlasSearchCancellation) throws -> String?
    let search: @Sendable (UInt64, AtlasWorkspaceStreamRequest, AtlasSearchCancellation) throws -> String?
    let page: @Sendable (UInt64, UInt64, UInt64, UInt64) throws -> String?
    let hit: @Sendable (UInt64, UInt64, UInt64) throws -> String?
    let clear: @Sendable (UInt64) throws -> String?
    let cancel: @Sendable (UInt64) -> Bool
    let close: @Sendable (UInt64) -> Bool
    let retirementFailed: @Sendable () -> Void
    let progressiveSearch: ProgressiveSearch?

    init(create: @escaping @Sendable () -> UInt64,
         open: @escaping @Sendable (UInt64, String, AtlasSearchCancellation) throws -> String?,
         search: @escaping @Sendable (UInt64, AtlasWorkspaceStreamRequest, AtlasSearchCancellation) throws -> String?,
         page: @escaping @Sendable (UInt64, UInt64, UInt64, UInt64) throws -> String?,
         hit: @escaping @Sendable (UInt64, UInt64, UInt64) throws -> String?,
         clear: @escaping @Sendable (UInt64) throws -> String?,
         cancel: @escaping @Sendable (UInt64) -> Bool,
         close: @escaping @Sendable (UInt64) -> Bool,
         retirementFailed: @escaping @Sendable () -> Void,
         progressiveSearch: ProgressiveSearch? = nil) {
        self.create = create; self.open = open; self.search = search; self.page = page
        self.hit = hit; self.clear = clear; self.cancel = cancel; self.close = close
        self.retirementFailed = retirementFailed; self.progressiveSearch = progressiveSearch
    }
}

struct AtlasWorkspaceStreamCatalog: Equatable, Sendable {
    let owner, manifest, layout, files: UInt64
    let root: String
    let discoveryComplete: Bool
    static func == (a: Self, b: Self) -> Bool {
        a.owner == b.owner && a.manifest == b.manifest && a.layout == b.layout && a.files == b.files
            && a.discoveryComplete == b.discoveryComplete && a.root.utf8.elementsEqual(b.root.utf8)
    }
    static func decode(_ json: String?, owner: UInt64) throws -> Self {
        let w = try AtlasWorkspaceStreamWire.read(json, owner: owner)
        do {
            try w.header(schema: "fcb.atlas-session/1", command: "info", owner: owner)
            let layout = try w.number("layout_revision"), files = try w.number("catalogued_files")
            let manifest = try w.number("source_manifest"), rawRoot = try w.object("root_path").pathBytes()
            guard let root = String(bytes: rawRoot, encoding: .utf8), manifest > 0,
                  layout > 0, files <= AtlasWorkspaceStreamLimits.files,
                  try w.number("workspace_files") == files, try w.text("scope") == "all",
                  try w.text("retention") == "frozen-catalog-and-spatial-index" else {
                throw AtlasWorkspaceStreamError.invalidResponse
            }
            _ = try AtlasObservedReaderInput(root: root, relativePath: [120])
            return Self(owner: owner, manifest: manifest, layout: layout, files: files, root: root,
                discoveryComplete: try w.flag("discovery_complete"))
        } catch { throw AtlasWorkspaceStreamError.invalidResponse }
    }
}

struct AtlasWorkspaceStreamIdentity: Equatable, Sendable {
    let owner, manifest, layout, generation: UInt64
    let root: [UInt8]
}

struct AtlasWorkspaceStreamSummary: Equatable, Sendable {
    let identity: AtlasWorkspaceStreamIdentity
    let request: AtlasWorkspaceStreamRequest
    let discoveryComplete, complete, truncated: Bool
    let stop: String?
    let discoveryPolicy: String
    let excludedEntries: UInt64
    let cataloguedFiles, examinedFiles, unexaminedFiles, incompleteFiles, unavailableFiles, unsupportedFiles, failedFiles, matchingFiles: UInt64
    let retainedHits, matchesSeen, payloadBytes, scannedBytes, readCalls, interruptedCalls, peakBufferBytes, witnessBytes: UInt64

    var countLabel: String {
        if complete { return "\(retainedHits) matches in \(matchingFiles) observed files" }
        if truncated { return "At least \(matchesSeen) matches; first \(retainedHits) retained" }
        return "\(retainedHits) retained matches · partial search"
    }
    var coverage: String {
        var result = "Examined \(examinedFiles) of \(cataloguedFiles) catalogued files; \(unexaminedFiles) unexamined, \(incompleteFiles) incomplete (\(unavailableFiles) unavailable, \(unsupportedFiles) unsupported text, \(failedFiles) failed reads). "
        result += "Scanned \(scannedBytes) bytes; read \(payloadBytes) bytes in \(readCalls) calls. "
        if !discoveryComplete { result += "Discovery is incomplete; additional project files may be absent. " }
        switch stop {
        case "match-limit": result += "The result limit stopped the scan. "
        case "byte-limit": result += "The total byte limit stopped the scan. "
        case "read-call-limit": result += "The read-call limit stopped the scan. "
        case "user-stopped": result += "Stopped at your request; matches from finalized files were kept. Any unfinished file remains unexamined. "
        default: break
        }
        return result + "Discovery policy: \(discoveryPolicy); \(excludedEntries) excluded entries (files or directories). Files were observed separately, without an atomic workspace snapshot. Only matching literal bytes are retained."
    }

    fileprivate static func decode(_ w: AtlasWorkspaceStreamWire, catalog: AtlasWorkspaceStreamCatalog,
                                   root: String, request: AtlasWorkspaceStreamRequest, terminal: Bool = true) throws -> Self {
        let identity = try w.streamIdentity(owner: catalog.owner, root: root)
        let limit = try w.object("limits")
        let files = try w.number("catalogued_files"), examined = try w.number("files_examined")
        let unexamined = try w.number("files_unexamined"), incomplete = try w.number("incomplete_files")
        let unavailable = try w.number("unavailable_files"), matching = try w.number("matching_files")
        let unsupported = try w.number("unsupported_files"), failed = try w.number("failed_files")
        let policy = try w.text("discovery_policy"), excluded = try w.number("excluded_entries")
        let retained = try w.number("retained_hits"), seen = try w.number("matches_seen")
        let payload = try w.number("payload_bytes_read"), scanned = try w.number("scanned_input_bytes")
        let calls = try w.number("read_calls"), interrupted = try w.number("interrupted_read_calls")
        let peak = try w.number("peak_input_buffer_bytes"), witnesses = try w.number("retained_witness_bytes")
        let discovery = try w.flag("discovery_complete"), complete = try w.flag("search_complete")
        let truncated = try w.flag("truncated"), stop = try w.optionalText("stop_reason")
        guard identity.layout == catalog.layout, identity.manifest == catalog.manifest,
              files == catalog.files, discovery == catalog.discoveryComplete,
              !policy.isEmpty, policy.utf8.count <= 256,
              policy.unicodeScalars.allSatisfy({ $0.value >= 32 && $0.value != 127 }),
              try w.text("needle").utf8.elementsEqual(request.needle.utf8),
              try limit.number("max_matches") == request.maximumMatches,
              try limit.number("max_scan_bytes") == request.maximumScanBytes,
              try limit.number("max_read_calls") == request.maximumReadCalls,
              examined <= files, unexamined == files - examined, incomplete <= examined,
              unavailable <= incomplete, unsupported <= incomplete - unavailable,
              failed <= incomplete - unavailable - unsupported, matching <= examined, matching <= retained,
              (retained == 0) == (matching == 0), retained <= request.maximumMatches,
              stop == nil || ["match-limit", "byte-limit", "read-call-limit", "user-stopped"].contains(stop!),
              terminal || (stop == nil && !truncated && !complete),
              truncated == (stop == "match-limit"), seen == retained + (truncated ? 1 : 0),
              !truncated || retained == request.maximumMatches,
              payload <= request.maximumScanBytes, scanned <= payload,
              calls <= request.maximumReadCalls, interrupted <= calls,
              payload == 0 || calls > 0, peak <= min(payload, 16 * 1024),
              witnesses >= matching, witnesses <= matching * UInt64(request.needle.utf8.count * 2),
              stop != "byte-limit" || payload == request.maximumScanBytes,
              stop != "read-call-limit" || calls == request.maximumReadCalls,
              complete == (terminal && discovery && unexamined == 0 && incomplete == 0 && stop == nil) else {
            throw AtlasWorkspaceStreamError.invalidResponse
        }
        return Self(identity: identity, request: request, discoveryComplete: discovery, complete: complete,
            truncated: truncated, stop: stop, discoveryPolicy: policy, excludedEntries: excluded,
            cataloguedFiles: files, examinedFiles: examined,
            unexaminedFiles: unexamined, incompleteFiles: incomplete, unavailableFiles: unavailable,
            unsupportedFiles: unsupported, failedFiles: failed,
            matchingFiles: matching, retainedHits: retained, matchesSeen: seen, payloadBytes: payload,
            scannedBytes: scanned, readCalls: calls, interruptedCalls: interrupted, peakBufferBytes: peak, witnessBytes: witnesses)
    }
}

struct AtlasWorkspaceStreamHit: Equatable, Identifiable, Sendable {
    let index, occurrence, file, node, revision: UInt64
    let path: [UInt8]
    let displayPath: String
    let range: AtlasObservedByteRange
    let encoding: AtlasObservedReaderEncoding
    let observedLength: UInt64
    let finalLength: UInt64?
    let consistency, state, witnessSHA256: String
    let complete: Bool
    var id: UInt64 { index }
    static func == (a: Self, b: Self) -> Bool {
        a.index == b.index && a.occurrence == b.occurrence && a.file == b.file && a.node == b.node
            && a.revision == b.revision && a.path == b.path && a.displayPath.utf8.elementsEqual(b.displayPath.utf8)
            && a.range == b.range && a.encoding == b.encoding && a.observedLength == b.observedLength
            && a.finalLength == b.finalLength && a.consistency == b.consistency && a.state == b.state
            && a.witnessSHA256 == b.witnessSHA256 && a.complete == b.complete
    }
    fileprivate func sameFile(as other: Self) -> Bool {
        file == other.file && node == other.node && revision == other.revision && path == other.path
            && encoding == other.encoding && observedLength == other.observedLength && finalLength == other.finalLength
            && consistency == other.consistency && state == other.state && complete == other.complete
            && witnessSHA256 == other.witnessSHA256
    }
    fileprivate static func decode(_ w: AtlasWorkspaceStreamWire, root: String,
                                   summary: AtlasWorkspaceStreamSummary) throws -> Self {
        let index = try w.number("index"), occurrence = try w.number("occurrence_id")
        let file = try w.number("file_id"), node = try w.number("node_ordinal"), revision = try w.number("source_revision")
        let path = try w.object("path"), raw = try path.pathBytes()
        _ = try AtlasObservedReaderInput(root: root, relativePath: raw)
        let original = try w.object("original_range")
        let range = try AtlasObservedByteRange(start: original.number("start"), end: original.number("end"))
        guard let encoding = AtlasObservedReaderEncoding(rawValue: try w.text("encoding")) else {
            throw AtlasWorkspaceStreamError.invalidResponse
        }
        let length = try w.number("observed_length"), final = try w.optionalNumber("final_observed_length")
        let consistency = try w.text("consistency"), state = try w.text("file_state")
        let complete = try w.flag("file_complete"), digest = try w.text("witness_sha256")
        let literalBytes = UInt64(AtlasObservedReaderSearchRequest.originalBytes(summary.request.needle, encoding: encoding).count)
        guard index < summary.retainedHits, occurrence > 0, occurrence <= summary.request.maximumMatches,
              file > 0, revision > 0, range.count == literalBytes,
              range.end <= length, range.end <= summary.scannedBytes,
              encoding == .utf8 || range.start % 2 == 0,
              ["complete-observed-input", "match-limit", "byte-limit", "read-call-limit", "short-read", "failed"].contains(state),
              ["unchanged-metadata-not-atomic", "changed-during-read", "short-read", "metadata-unavailable"].contains(consistency),
              consistency != "unchanged-metadata-not-atomic" || final == length,
              complete == (state == "complete-observed-input" && consistency == "unchanged-metadata-not-atomic"),
              AtlasObservedReaderWire.validHex(digest, bytes: 32) else { throw AtlasWorkspaceStreamError.invalidResponse }
        return Self(index: index, occurrence: occurrence, file: file, node: node, revision: revision,
            path: raw, displayPath: try path.text("display"), range: range, encoding: encoding,
            observedLength: length, finalLength: final, consistency: consistency, state: state,
            witnessSHA256: digest, complete: complete)
    }
}

struct AtlasWorkspaceStreamTarget: Equatable, Sendable {
    let identity: AtlasWorkspaceStreamIdentity
    let hit: AtlasWorkspaceStreamHit
}

struct AtlasWorkspaceStreamPage: Sendable {
    let summary: AtlasWorkspaceStreamSummary
    let offset: UInt64
    let next: UInt64?
    let hits: [AtlasWorkspaceStreamHit]
    static func decode(_ json: String?, catalog: AtlasWorkspaceStreamCatalog, root: String,
                       request: AtlasWorkspaceStreamRequest, previous: AtlasWorkspaceStreamSummary? = nil,
                       offset: UInt64 = 0, requireTerminal: Bool = false) throws -> Self {
        let w = try AtlasWorkspaceStreamWire.read(json, owner: catalog.owner)
        do {
            try w.terminalState(expected: true, required: requireTerminal)
            return try decode(w, catalog: catalog, root: root, request: request,
                command: previous == nil ? "search" : "page", terminal: true, previous: previous, offset: offset)
        } catch { throw AtlasWorkspaceStreamError.invalidResponse }
    }

    fileprivate static func decode(_ w: AtlasWorkspaceStreamWire, catalog: AtlasWorkspaceStreamCatalog,
        root: String, request: AtlasWorkspaceStreamRequest, command: String, terminal: Bool,
        previous: AtlasWorkspaceStreamSummary? = nil, offset: UInt64 = 0) throws -> Self {
        try w.header(schema: "fcb.atlas-stream-search/1", command: command, owner: catalog.owner)
        let summary = try AtlasWorkspaceStreamSummary.decode(w, catalog: catalog, root: root,
            request: request, terminal: terminal)
        let rows = try w.rows("hits", limit: Int(AtlasWorkspaceStreamLimits.page))
        let count = try w.number("page_count"), next = try w.optionalNumber("next_offset")
        guard previous == nil || previous == summary, offset <= summary.retainedHits,
              try w.number("page_offset") == offset, count == UInt64(rows.count),
              count == min(AtlasWorkspaceStreamLimits.page, summary.retainedHits - offset),
              next == (offset + count < summary.retainedHits ? offset + count : nil) else {
            throw AtlasWorkspaceStreamError.invalidResponse
        }
        let hits = try rows.enumerated().map { position, row in
            let hit = try AtlasWorkspaceStreamHit.decode(row, root: root, summary: summary)
            guard hit.index == offset + UInt64(position) else { throw AtlasWorkspaceStreamError.invalidResponse }
            return hit
        }
        return Self(summary: summary, offset: offset, next: next, hits: hits)
    }
}

/// A provisional first page is display evidence only. It cannot authorize
/// retained-hit selection, copy or a current-source read before terminal paging.
struct AtlasWorkspaceStreamProgress: Sendable {
    let sequence: UInt64
    let page: AtlasWorkspaceStreamPage
    var summary: AtlasWorkspaceStreamSummary { page.summary }
    var countLabel: String {
        "\(summary.retainedHits) matches so far in \(summary.matchingFiles) finalized files"
    }
    var coverage: String {
        "Search in progress. Matches shown come from finalized file observations. Byte and read counts also include the unfinished file; its matches are not yet retained. " + summary.coverage
    }

    static func decode(_ json: String?, catalog: AtlasWorkspaceStreamCatalog,
        request: AtlasWorkspaceStreamRequest, previous: Self? = nil) throws -> Self {
        let w = try AtlasWorkspaceStreamWire.read(json, owner: catalog.owner)
        do {
            try w.terminalState(expected: false, required: true)
            let sequence = try w.number("progress_sequence")
            guard sequence > 0 else { throw AtlasWorkspaceStreamError.invalidResponse }
            let page = try AtlasWorkspaceStreamPage.decode(w, catalog: catalog, root: catalog.root,
                request: request, command: "progress", terminal: false)
            _ = try AtlasWorkspaceStreamReport.validatePrefix(page.hits, summary: page.summary)
            let result = Self(sequence: sequence, page: page)
            if let previous { try result.validate(after: previous) }
            return result
        } catch { throw AtlasWorkspaceStreamError.invalidResponse }
    }

    func validate(after previous: Self) throws {
        guard sequence > previous.sequence else { throw AtlasWorkspaceStreamError.invalidResponse }
        try Self.validateAdvance(from: previous.page, to: page)
    }
    func validateTerminal(_ terminal: AtlasWorkspaceStreamPage) throws {
        try Self.validateAdvance(from: page, to: terminal)
    }
    private static func validateAdvance(from previous: AtlasWorkspaceStreamPage,
        to next: AtlasWorkspaceStreamPage) throws {
        let old = previous.summary, new = next.summary
        guard new.identity == old.identity, new.request == old.request,
              new.cataloguedFiles == old.cataloguedFiles, new.discoveryComplete == old.discoveryComplete,
              new.discoveryPolicy.utf8.elementsEqual(old.discoveryPolicy.utf8), new.excludedEntries == old.excludedEntries,
              previous.offset == 0, next.offset == 0, next.hits.count >= previous.hits.count,
              Array(next.hits.prefix(previous.hits.count)) == previous.hits else {
            throw AtlasWorkspaceStreamError.invalidResponse
        }
        let before = [old.examinedFiles, old.incompleteFiles, old.unavailableFiles, old.unsupportedFiles,
            old.failedFiles, old.matchingFiles, old.retainedHits, old.matchesSeen, old.payloadBytes,
            old.scannedBytes, old.readCalls, old.interruptedCalls, old.peakBufferBytes, old.witnessBytes]
        let after = [new.examinedFiles, new.incompleteFiles, new.unavailableFiles, new.unsupportedFiles,
            new.failedFiles, new.matchingFiles, new.retainedHits, new.matchesSeen, new.payloadBytes,
            new.scannedBytes, new.readCalls, new.interruptedCalls, new.peakBufferBytes, new.witnessBytes]
        guard zip(before, after).allSatisfy({ $0.0 <= $0.1 }) else { throw AtlasWorkspaceStreamError.invalidResponse }
    }
}

struct AtlasWorkspaceStreamReport: Sendable {
    let summary: AtlasWorkspaceStreamSummary
    let hits: [AtlasWorkspaceStreamHit]
    init(summary: AtlasWorkspaceStreamSummary, hits: [AtlasWorkspaceStreamHit]) throws {
        guard UInt64(hits.count) == summary.retainedHits else { throw AtlasWorkspaceStreamError.invalidResponse }
        let prefix = try Self.validatePrefix(hits, summary: summary)
        guard prefix.files == summary.matchingFiles, prefix.bytes == summary.witnessBytes else {
            throw AtlasWorkspaceStreamError.invalidResponse
        }
        self.summary = summary; self.hits = hits
    }
    fileprivate static func validatePrefix(_ hits: [AtlasWorkspaceStreamHit],
        summary: AtlasWorkspaceStreamSummary) throws -> (files: UInt64, bytes: UInt64) {
        guard UInt64(hits.count) <= summary.retainedHits else { throw AtlasWorkspaceStreamError.invalidResponse }
        var files = Set<UInt64>(), bytes: UInt64 = 0
        for (index, hit) in hits.enumerated() {
            guard hit.index == UInt64(index) else { throw AtlasWorkspaceStreamError.invalidResponse }
            if index > 0, hits[index - 1].file == hit.file {
                let previous = hits[index - 1]
                guard hit.sameFile(as: previous), hit.occurrence == previous.occurrence + 1,
                      hit.range.start > previous.range.start else { throw AtlasWorkspaceStreamError.invalidResponse }
            } else {
                guard files.insert(hit.file).inserted, hit.occurrence == 1,
                      index == 0 || hits[index - 1].path.lexicographicallyPrecedes(hit.path) else {
                    throw AtlasWorkspaceStreamError.invalidResponse
                }
                bytes += hit.range.count
            }
        }
        guard UInt64(files.count) <= summary.matchingFiles, bytes <= summary.witnessBytes else {
            throw AtlasWorkspaceStreamError.invalidResponse
        }
        if UInt64(hits.count) == summary.retainedHits {
            guard UInt64(files.count) == summary.matchingFiles, bytes == summary.witnessBytes else {
                throw AtlasWorkspaceStreamError.invalidResponse
            }
        }
        return (UInt64(files.count), bytes)
    }
    func target(for hit: AtlasWorkspaceStreamHit) -> AtlasWorkspaceStreamTarget { .init(identity: summary.identity, hit: hit) }
    func admits(_ target: AtlasWorkspaceStreamTarget) -> Bool {
        target.identity == summary.identity && target.hit.index < UInt64(hits.count)
            && hits[Int(target.hit.index)] == target.hit
    }
}

struct AtlasWorkspaceStreamWitness: Sendable {
    let target: AtlasWorkspaceStreamTarget
    let text, originalHex: String
    static func decode(_ json: String?, report: AtlasWorkspaceStreamReport, root: String,
                       target: AtlasWorkspaceStreamTarget) throws -> Self {
        let w = try AtlasWorkspaceStreamWire.read(json, owner: target.identity.owner)
        do {
            try w.header(schema: "fcb.atlas-stream-search/1", command: "hit", owner: target.identity.owner)
            let hit = try w.object("hit")
            let row = try AtlasWorkspaceStreamHit.decode(hit, root: root, summary: report.summary)
            let text = try hit.text("text"), hex = try hit.text("original_hex")
            guard report.admits(target), try w.streamIdentity(owner: target.identity.owner, root: root) == target.identity,
                  row == target.hit, try w.text("needle").utf8.elementsEqual(report.summary.request.needle.utf8),
                  text.utf8.elementsEqual(report.summary.request.needle.utf8),
                  hex == AtlasObservedReaderWire.hex(AtlasObservedReaderSearchRequest.originalBytes(text, encoding: row.encoding)),
                  try w.number("additional_source_bytes_read") == 0, try w.number("additional_read_calls") == 0 else {
                throw AtlasWorkspaceStreamError.invalidResponse
            }
            return Self(target: target, text: text, originalHex: hex)
        } catch { throw AtlasWorkspaceStreamError.invalidResponse }
    }
}

/// Typed, bounded receipt parsing only. Raw path bytes remain separate from
/// escaped presentation labels; all source algorithms stay in the engine.
struct AtlasWorkspaceStreamWire: Decodable {
    struct Key: CodingKey {
        let stringValue: String
        var intValue: Int? { nil }
        init(_ value: String) { stringValue = value }
        init?(stringValue: String) { self.init(stringValue) }
        init?(intValue: Int) { return nil }
    }
    let fields: KeyedDecodingContainer<Key>
    init(from decoder: Decoder) throws { fields = try decoder.container(keyedBy: Key.self) }
    static func read(_ json: String?, owner: UInt64) throws -> Self {
        guard owner > 0, let json, json.utf8.count <= AtlasWorkspaceStreamLimits.responseBytes else {
            throw AtlasWorkspaceStreamError.invalidResponse
        }
        do {
            let w = try JSONDecoder().decode(Self.self, from: Data(json.utf8))
            guard try w.number("owner") == owner else { throw AtlasWorkspaceStreamError.invalidResponse }
            if try w.text("status") == "error" {
                guard ["fcb.atlas-session/1", "fcb.atlas-stream-search/1"].contains(try w.text("schema")) else {
                    throw AtlasWorkspaceStreamError.invalidResponse
                }
                let code = try w.object("error").text("code")
                guard !code.isEmpty, code.utf8.count <= 512,
                      code.unicodeScalars.allSatisfy({ $0.value >= 32 && $0.value != 127 }) else {
                    throw AtlasWorkspaceStreamError.invalidResponse
                }
                throw AtlasWorkspaceStreamError.engine(code)
            }
            guard try w.text("status") == "ok" else { throw AtlasWorkspaceStreamError.invalidResponse }
            if w.fields.contains(Key("error")) {
                guard try w.fields.decodeNil(forKey: Key("error")) else { throw AtlasWorkspaceStreamError.invalidResponse }
            }
            return w
        } catch let error as AtlasWorkspaceStreamError { throw error }
        catch { throw AtlasWorkspaceStreamError.invalidResponse }
    }
    func text(_ key: String) throws -> String { try fields.decode(String.self, forKey: Key(key)) }
    func flag(_ key: String) throws -> Bool { try fields.decode(Bool.self, forKey: Key(key)) }
    func number(_ key: String) throws -> UInt64 { try Self.number(text(key)) }
    static func number(_ text: String) throws -> UInt64 {
        guard let value = UInt64(text), String(value) == text else { throw AtlasWorkspaceStreamError.invalidResponse }
        return value
    }
    func optionalText(_ key: String) throws -> String? {
        guard fields.contains(Key(key)) else { throw AtlasWorkspaceStreamError.invalidResponse }
        return try fields.decodeIfPresent(String.self, forKey: Key(key))
    }
    func optionalNumber(_ key: String) throws -> UInt64? { try optionalText(key).map(Self.number) }
    func object(_ key: String) throws -> Self { try fields.decode(Self.self, forKey: Key(key)) }
    func rows(_ key: String, limit: Int) throws -> [Self] {
        var array = try fields.nestedUnkeyedContainer(forKey: Key(key))
        guard array.count.map({ $0 <= limit }) ?? true else { throw AtlasWorkspaceStreamError.invalidResponse }
        var result: [Self] = []
        while !array.isAtEnd {
            guard result.count < limit else { throw AtlasWorkspaceStreamError.invalidResponse }
            result.append(try array.decode(Self.self))
        }
        return result
    }
    func pathBytes() throws -> [UInt8] {
        let hex = try text("hex"), display = try text("display")
        guard try text("encoding") == "unix-bytes", !hex.isEmpty, hex.utf8.count % 2 == 0,
              hex.utf8.count <= 2 * AtlasObservedReaderLimits.maximumPathBytes, display.utf8.count <= 65_536,
              AtlasObservedReaderWire.validHex(hex, bytes: UInt64(hex.utf8.count / 2)) else {
            throw AtlasWorkspaceStreamError.invalidResponse
        }
        let raw = Array(hex.utf8)
        func digit(_ byte: UInt8) -> UInt8 { byte <= 57 ? byte - 48 : byte - 87 }
        return stride(from: 0, to: raw.count, by: 2).map { digit(raw[$0]) * 16 + digit(raw[$0 + 1]) }
    }
    /// Legacy terminal-only hosts may omit both flags. Partial new metadata or
    /// a provisional page can never cross into the actionable terminal path.
    func terminalState(expected: Bool, required: Bool) throws {
        let terminalPresent = fields.contains(Key("terminal"))
        let selectionPresent = fields.contains(Key("selection_available"))
        if !required && !terminalPresent && !selectionPresent { return }
        guard terminalPresent, selectionPresent,
              try flag("terminal") == expected, try flag("selection_available") == expected else {
            throw AtlasWorkspaceStreamError.invalidResponse
        }
    }
    func header(schema: String, command: String, owner: UInt64) throws {
        guard try text("schema") == schema, try text("status") == "ok", try text("command") == command,
              try number("owner") == owner else { throw AtlasWorkspaceStreamError.invalidResponse }
    }
    func streamIdentity(owner: UInt64, root: String) throws -> AtlasWorkspaceStreamIdentity {
        let manifest = try number("source_manifest"), layout = try number("layout_revision")
        let generation = try number("query_generation"), rawRoot = try object("root_path").pathBytes()
        guard try number("owner") == owner, manifest > 0, layout > 0, generation > 0,
              try number("last_attempt") >= generation,
              rawRoot.elementsEqual(root.utf8), try text("scope") == "catalogued-whole-files",
              try text("strategy") == "streaming-whole-file", try text("mode") == "exact-decoded-literal",
              try text("capture_kind") == "literal-witnesses-only",
              try text("source_observation") == "per-file-streams-not-atomic-workspace" else {
            throw AtlasWorkspaceStreamError.invalidResponse
        }
        for key in ["capture_sha256", "whole_file_sha256", "source_revision", "capture_byte_length"] where fields.contains(Key(key)) {
            guard try fields.decodeNil(forKey: Key(key)) else { throw AtlasWorkspaceStreamError.invalidResponse }
        }
        return .init(owner: owner, manifest: manifest, layout: layout, generation: generation, root: rawRoot)
    }
}
