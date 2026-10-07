import Foundation

struct SearchHit: Identifiable, Hashable, Sendable {
    let id: Int
    /// Escaped presentation label is never used as a filesystem path.
    let path: String
    let sourcePath: String?
    let start: UInt64
    let end: UInt64
    let captureSHA256: String?
    let captureByteLength: UInt64?
    var fileName: String { (path as NSString).lastPathComponent }
    var folder: String { (path as NSString).deletingLastPathComponent }
}

enum AtlasSearchError: Error, Sendable {
    case unavailable, invalidResponse, canceled, invalidRequest, identityExhausted
    var message: String {
        switch self {
        case .canceled: return "Search canceled. In-flight reads may finish, but their results will not be shown."
        case .invalidRequest: return "Choose a project and enter 1–1024 UTF-8 bytes of search text without NUL characters."
        case .identityExhausted: return "This search session exhausted its request identities. Reopen the window before searching again."
        case .unavailable: return "Search unavailable. The project could not be read or the search exceeded its limits."
        case .invalidResponse: return "Search response could not be read. Results are unavailable, not an empty match set."
        }
    }
}

/// Progress is separate from completeness: a terminal query may be partial.
struct AtlasSearchProgress: Sendable {
    let isRunning: Bool
    let examinedFiles: Int
    let cataloguedFiles: Int
}

/// Adapts the shared engine's search envelope, without another search engine.
struct AtlasSearchReport: Sendable {
    let hits: [SearchHit]
    let complete: Bool
    let truncated: Bool
    let unavailableFiles: Int
    let unsupportedFiles: Int
    let matchesSeen: UInt64
    var progress: AtlasSearchProgress? = nil
    /// One identity for an append-only stream; legacy one-shot reports use nil.
    var streamID: UUID? = nil
    /// Optional immutable-source capability; legacy one-shot reports have none.
    var capture: AtlasSearchCapture? = nil
    var isInProgress: Bool { progress?.isRunning == true }

    var summary: String {
        let count = hits.count
        if let progress, progress.isRunning {
            return "Searching: \(count) exact matches found; \(progress.examinedFiles) of \(progress.cataloguedFiles) catalogued files examined. Results are provisional."
        }
        if complete {
            return count == 0 ? "No exact matches in the captured project."
                : "\(count) exact match\(count == 1 ? "" : "es") in the captured project."
        }
        var reasons: [String] = []
        if truncated { reasons.append("result limit reached") }
        if unavailableFiles > 0 { reasons.append("\(unavailableFiles) unavailable files") }
        if unsupportedFiles > 0 { reasons.append("\(unsupportedFiles) unsupported text files") }
        if reasons.isEmpty { reasons.append("project coverage incomplete") }
        return "Partial search: \(count) matches shown; \(reasons.joined(separator: ", ")). More matches may exist."
    }

    static func decode(_ json: String?) throws -> Self {
        guard let json else { throw AtlasSearchError.unavailable }
        guard let data = json.data(using: .utf8) else { throw AtlasSearchError.invalidResponse }
        do {
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            guard wire.schema == "fcb.cli/1", wire.status == "ok", wire.command == "search",
                  wire.scope == "workspace", wire.mode == "decoded-text-literal",
                  let matches = integer(wire.matches_seen) else { throw AtlasSearchError.invalidResponse }
            let hits = try wire.hits.enumerated().map { index, hit in
                guard let start = integer(hit.original_range.start),
                      let end = integer(hit.original_range.end), end >= start,
                      hit.path.encoding == "unix-bytes", let raw = unhex(hit.path.hex),
                      !raw.isEmpty, !raw.contains(0), raw.first != 47 else {
                    throw AtlasSearchError.invalidResponse
                }
                // Validate raw components before attempting UTF-8 conversion.
                guard !raw.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                    $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
                }) else { throw AtlasSearchError.invalidResponse }
                let digest = hit.capture_sha256
                let byteLength = hit.capture_byte_length.flatMap(integer)
                if digest != nil || hit.capture_byte_length != nil {
                    guard let digest, digest.utf8.count == 64,
                          digest.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
                          let byteLength, end <= byteLength else { throw AtlasSearchError.invalidResponse }
                }
                return SearchHit(id: index, path: hit.path.display,
                    sourcePath: String(bytes: raw, encoding: .utf8), start: start, end: end,
                    captureSHA256: digest, captureByteLength: byteLength)
            }
            guard matches >= UInt64(hits.count) else { throw AtlasSearchError.invalidResponse }
            return Self(hits: hits,
                complete: wire.workspace_complete && wire.discovery_complete && !wire.truncated
                    && wire.unavailable_files.isEmpty && wire.unsupported_text_files.isEmpty
                    && matches == UInt64(hits.count),
                truncated: wire.truncated, unavailableFiles: wire.unavailable_files.count,
                unsupportedFiles: wire.unsupported_text_files.count, matchesSeen: matches)
        } catch { throw AtlasSearchError.invalidResponse }
    }

    private static func integer(_ text: String) -> UInt64? {
        guard let value = UInt64(text), String(value) == text else { return nil }
        return value
    }
    private static func unhex(_ text: String) -> [UInt8]? {
        let input = Array(text.utf8)
        guard input.count <= 32_768, input.count.isMultiple(of: 2) else { return nil }
        func nibble(_ value: UInt8) -> UInt8? {
            switch value { case 48...57: return value - 48; case 97...102: return value - 87; default: return nil }
        }
        var bytes: [UInt8] = []
        bytes.reserveCapacity(input.count / 2)
        for index in stride(from: 0, to: input.count, by: 2) {
            guard let high = nibble(input[index]), let low = nibble(input[index + 1]) else { return nil }
            bytes.append(high * 16 + low)
        }
        return bytes
    }

    private struct Wire: Decodable {
        let schema, status, command, scope, mode: String
        let workspace_complete, discovery_complete, truncated: Bool
        let matches_seen: String
        let hits: [Hit]
        let unavailable_files, unsupported_text_files: [FileRecord]
    }
    private struct Hit: Decodable {
        let path: NativePath
        let original_range: OffsetRange
        let capture_sha256: String?
        let capture_byte_length: String?
    }
    private struct NativePath: Decodable { let encoding, hex, display: String }
    private struct OffsetRange: Decodable { let start, end: String }
    private struct FileRecord: Decodable { let file_id: String }
}

/// Validated, immutable input to the literal-search bridge. Admission does
/// not trim whitespace or normalize Unicode: those bytes are the query.
struct AtlasSearchInput: Sendable {
    let root: String
    let query: String

    init(root: String, query: String) throws {
        guard !root.isEmpty, root.utf8.count <= 16_384, !root.utf8.contains(0),
              !query.isEmpty, query.utf8.count <= 1_024, !query.utf8.contains(0) else {
            throw AtlasSearchError.invalidRequest
        }
        self.root = root
        self.query = query
    }
}

/// Identity of the visible source/search context, not just the project path.
/// Compare UTF-8 bytes: Swift String equality considers canonically equivalent
/// spellings equal, but exact search and Unix filenames must not do so.
struct AtlasSearchContext: Sendable {
    let root: String
    let query: String
    let loadGeneration: UUID
    let atlasRevision: UUID
    let scope: String
    let customExtensions: String

    func matches(_ other: Self) -> Bool {
        loadGeneration == other.loadGeneration && atlasRevision == other.atlasRevision
            && root.utf8.elementsEqual(other.root.utf8)
            && query.utf8.elementsEqual(other.query.utf8)
            && scope.utf8.elementsEqual(other.scope.utf8)
            && customExtensions.utf8.elementsEqual(other.customExtensions.utf8)
    }
}

/// Hit indices are only unique within one report. A delayed List selection
/// must not turn row 0 of an old report into row 0 of a new one.
struct AtlasSearchRowID: Hashable, Sendable {
    let report: UUID
    let hit: SearchHit.ID
}

enum AtlasSearchHitAvailability: Sendable {
    case ready, requiresVerification, stale, unavailable, unsupportedPath

    var allowsActivation: Bool {
        switch self {
        case .ready, .requiresVerification: return true
        case .stale, .unavailable, .unsupportedPath: return false
        }
    }

    var label: String {
        switch self {
        case .ready: return ""
        case .requiresVerification: return "Verify source on open"
        case .stale: return "Search again — results changed"
        case .unavailable: return "Exact location unavailable"
        case .unsupportedPath: return "Filename cannot be opened"
        }
    }

    var message: String {
        switch self {
        case .ready: return "Open the verified match in the captured source."
        case .requiresVerification: return "Read the source and verify its capture before selecting this match. Atlas highlighting is not required."
        case .stale: return "The project, capture, filter, or query changed. Search again before opening this result."
        case .unavailable: return "The captured source could not verify this match. The result is retained, but exact navigation is disabled. Use Open file to read it without match navigation."
        case .unsupportedPath: return "This filename cannot be opened by the UTF-8 reader. The search result is retained."
        }
    }
}

/// Report/context admission shared by rows and their activation handler.
/// A capture candidate authorizes verification, not an exact jump. AtlasMatch
/// must still verify the installed source, independently of overlay geometry.
struct AtlasSearchPresentation: Sendable {
    let id: UUID
    private let context: AtlasSearchContext
    private let hits: [SearchHit.ID: SearchHit]
    private let verifiedHitIDs: Set<SearchHit.ID>

    init(context: AtlasSearchContext, hits: [SearchHit], verifiedHitIDs: Set<SearchHit.ID>, id: UUID = UUID()) {
        self.id = id
        self.context = context
        // The decoder emits unique indices. Fail closed rather than trapping
        // or choosing one witness if another producer supplies duplicate IDs.
        self.hits = Dictionary(grouping: hits, by: \.id).compactMapValues {
            $0.count == 1 ? $0.first : nil
        }
        self.verifiedHitIDs = verifiedHitIDs
    }

    func rowID(for hit: SearchHit.ID) -> AtlasSearchRowID {
        AtlasSearchRowID(report: id, hit: hit)
    }

    func isCurrent(in current: AtlasSearchContext) -> Bool { context.matches(current) }

    func availability(for row: AtlasSearchRowID, in current: AtlasSearchContext) -> AtlasSearchHitAvailability {
        guard row.report == id, isCurrent(in: current) else { return .stale }
        guard let hit = hits[row.hit] else { return .unavailable }
        guard hit.sourcePath != nil else { return .unsupportedPath }
        guard captureCandidate(for: row, in: current) != nil else { return .unavailable }
        return verifiedHitIDs.contains(hit.id) ? .ready : .requiresVerification
    }

    /// Previously verified candidates only; never promotes a missing overlay
    /// to capture proof. The reader can use captureCandidate to verify on open.
    func hit(for row: AtlasSearchRowID, in current: AtlasSearchContext) -> SearchHit? {
        guard verifiedHitIDs.contains(row.hit) else { return nil }
        return captureCandidate(for: row, in: current)
    }

    /// Return the immutable witness for a current row. This does NOT establish
    /// that currently available source bytes agree with the search capture.
    func captureCandidate(for row: AtlasSearchRowID, in current: AtlasSearchContext) -> SearchHit? {
        guard row.report == id, isCurrent(in: current) else { return nil }
        return capturedHit(row.hit)
    }

    /// Next/previous exact-navigation candidate in the caller's visible order.
    /// Filtered-out, unavailable and ambiguous rows are never activated. A row
    /// from another report does not lend its integer ID to the new selection.
    /// This only chooses a witness; opening still verifies the source capture.
    func adjacentRow(in orderedHitIDs: [SearchHit.ID], after currentRow: AtlasSearchRowID?,
                     backwards: Bool, context current: AtlasSearchContext) -> AtlasSearchRowID? {
        guard isCurrent(in: current), !orderedHitIDs.isEmpty else { return nil }
        let selectedIndex = currentRow.flatMap { row in
            row.report == id ? orderedHitIDs.firstIndex(of: row.hit) : nil
        }
        var index = selectedIndex ?? (backwards ? 0 : orderedHitIDs.count - 1)
        for _ in orderedHitIDs.indices {
            if backwards { index = index == 0 ? orderedHitIDs.count - 1 : index - 1 }
            else { index = index == orderedHitIDs.count - 1 ? 0 : index + 1 }
            if capturedHit(orderedHitIDs[index]) != nil { return rowID(for: orderedHitIDs[index]) }
        }
        return nil
    }

    private func capturedHit(_ hitID: SearchHit.ID) -> SearchHit? {
        guard let hit = hits[hitID], hit.sourcePath != nil, let digest = hit.captureSHA256,
              digest.utf8.count == 64,
              digest.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
              let count = hit.captureByteLength, hit.start < hit.end, hit.end <= count else { return nil }
        return hit
    }

    /// Explicit file opening is distinct from an exact-hit jump. It still
    /// needs the current report/root, but does not borrow an unverified range.
    func filePath(for row: AtlasSearchRowID, in current: AtlasSearchContext) -> String? {
        guard row.report == id, isCurrent(in: current) else { return nil }
        return hits[row.hit]?.sourcePath
    }
}

/// Filename scope over the already captured atlas; no source reads or parsing.
enum AtlasFileScope: String, CaseIterable, Identifiable {
    case all = "All files", markdown = "Markdown", python = "Python", rust = "Rust", custom = "Extension…"
    var id: String { rawValue }
    func includes(_ path: String, custom: String = "") -> Bool {
        let name = (path as NSString).lastPathComponent.lowercased()
        let ext = (name as NSString).pathExtension
        switch self {
        case .all: return true
        case .markdown: return ["md", "markdown", "mdown"].contains(ext)
        case .python: return ["py", "pyi", "pyw"].contains(ext)
        case .rust: return ext == "rs" || name == "cargo.toml" || name == "cargo.lock" || name == "rust-toolchain.toml" || name == "rust-toolchain"
        case .custom:
            let allowed = custom.lowercased().split { $0 == "," || $0.isWhitespace }
                .map { $0.trimmingCharacters(in: CharacterSet(charactersIn: ".")) }.filter { !$0.isEmpty }
            return allowed.contains(ext)
        }
    }
}

/// Immutable grant reference, attached by the request owner before delivery.
/// Retention confers no authority to paths outside that request's root.
final class AtlasSearchAccessLease: @unchecked Sendable {
    let reference: AnyObject?
    init(_ reference: AnyObject?) { self.reference = reference }
}
struct AtlasSearchCapturedHit: Sendable {
    let id = UUID()
    let root, path, needle: String
    let start, end, capturedBytes: UInt64
    let openReader: @Sendable (UInt64) throws -> String
}
struct AtlasSearchCapture: Sendable {
    let retainAccess: @Sendable (AtlasSearchAccessLease) -> Void
    let target: @Sendable (SearchHit) -> AtlasSearchCapturedHit?
}
