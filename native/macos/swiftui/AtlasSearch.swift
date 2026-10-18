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
    case unavailable, invalidResponse, canceled, invalidRequest, invalidScope, invalidPathScope, identityExhausted
    var message: String {
        switch self {
        case .canceled: return "Search canceled. In-flight reads may finish, but their results will not be shown."
        case .invalidRequest: return "Choose a project and enter 1–1024 UTF-8 bytes of search text without NUL characters."
        case .invalidScope: return "Choose up to 32 file extensions using letters, digits, +, - or _. Separate extensions with commas or spaces."
        case .invalidPathScope: return "Enter a project-relative file or folder path of 1–4096 UTF-8 bytes. Do not use leading/trailing slashes, empty components, or '.'/'..' components."
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
    var indexStatus: AtlasSearchIndexStatus? = nil
    var fileScope: AtlasSearchFileScope = .all
    var excludedFiles: Int = 0
    var pathScope: AtlasSearchPathScope = .workspace
    var isInProgress: Bool { progress?.isRunning == true }
    var isWorkspaceScope: Bool { fileScope.isAll && pathScope.isWorkspace }

    var summary: String {
        if indexStatus?.building == true, let progress {
            return "Preparing captured index: \(progress.examinedFiles) of \(progress.cataloguedFiles) catalogued files examined. Query results are not ready yet."
        }
        return resultSummary
            + (isWorkspaceScope ? "" : " \(excludedFiles) catalogued files outside this scope.")
            + (indexStatus == nil ? "" : " Captured index; Refresh to include file edits.")
    }
    private var resultSummary: String {
        let count = hits.count
        if let progress, progress.isRunning {
            let membership = isWorkspaceScope ? "catalogued" : "eligible"
            return "Searching: \(count) exact matches found; \(progress.examinedFiles) of \(progress.cataloguedFiles) \(membership) files examined. Results are provisional."
        }
        if complete {
            let scope = pathScope.isWorkspace ? (fileScope.isAll ? "captured project" : "selected file types")
                : "selected \(pathScope.kind == .file ? "file" : "folder") \(String(reflecting: pathScope.path))"
            return count == 0 ? "No exact matches in the \(scope)."
                : "\(count) exact match\(count == 1 ? "" : "es") in the \(scope)."
        }
        var reasons: [String] = []
        if truncated { reasons.append("result limit reached") }
        if unavailableFiles > 0 { reasons.append("\(unavailableFiles) unavailable files") }
        if unsupportedFiles > 0 { reasons.append("\(unsupportedFiles) unsupported text files") }
        if reasons.isEmpty { reasons.append(isWorkspaceScope ? "project coverage incomplete" : "scope coverage incomplete") }
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
    let fileScope: AtlasSearchFileScope
    let pathScope: AtlasSearchPathScope
    var isWorkspaceScope: Bool { fileScope.isAll && pathScope.isWorkspace }

    init(root: String, query: String, fileScope: AtlasSearchFileScope = .all,
         pathScope: AtlasSearchPathScope = .workspace) throws {
        guard !root.isEmpty, root.utf8.count <= 16_384, !root.utf8.contains(0),
              !query.isEmpty, query.utf8.count <= 1_024, !query.utf8.contains(0) else {
            throw AtlasSearchError.invalidRequest
        }
        self.root = root
        self.query = query
        self.fileScope = fileScope
        self.pathScope = pathScope
    }
    func requireLegacyScope() throws {
        guard pathScope.isWorkspace else { throw AtlasSearchError.invalidPathScope }
        guard fileScope.isAll else { throw AtlasSearchError.invalidScope }
    }
}

/// Exact relative location within the frozen catalog. This narrows a query;
/// it never confers a new root grant, follows a live selection or normalizes a
/// native pathname. Directory prefixes end at a slash, not an arbitrary byte.
struct AtlasSearchPathScope: Equatable, Sendable {
    enum Kind: String, CaseIterable, Identifiable, Sendable {
        case workspace, directory, file
        var id: String { rawValue }
        var label: String { switch self { case .workspace: return "Workspace"; case .directory: return "Folder"; case .file: return "File" } }
        var code: UInt64 { switch self { case .workspace: return 0; case .directory: return 1; case .file: return 2 } }
    }
    let kind: Kind
    let path: String
    let pathHex: String
    static let workspace = Self()
    var isWorkspace: Bool { kind == .workspace }

    private init() { kind = .workspace; path = ""; pathHex = "" }
    init(kind: Kind, path: String) throws {
        guard path.utf8.count <= 4096,
              (kind == .workspace ? path.isEmpty : !path.isEmpty) else {
            throw AtlasSearchError.invalidPathScope
        }
        let raw = Array(path.utf8)
        if kind != .workspace {
            guard !raw.contains(0),
                  !raw.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                      $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
                  }) else { throw AtlasSearchError.invalidPathScope }
        }
        self.kind = kind
        self.path = path
        let digits = Array("0123456789abcdef".utf8)
        pathHex = String(decoding: raw.flatMap { [digits[Int($0 >> 4)], digits[Int($0 & 15)]] }, as: UTF8.self)
    }
    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.kind == rhs.kind && lhs.path.utf8.elementsEqual(rhs.path.utf8)
    }
    func withCString<Result>(_ body: (UInt64, UnsafePointer<CChar>?) throws -> Result) rethrows -> Result {
        if isWorkspace { return try body(0, nil) }
        return try path.withCString { try body(kind.code, $0) }
    }
    func admitsRawPath(_ raw: [UInt8]) -> Bool {
        if isWorkspace { return true }
        let prefix = Array(path.utf8)
        if kind == .file { return raw == prefix }
        return raw.count > prefix.count && raw.starts(with: prefix) && raw[prefix.count] == 47
    }
    func validateEcho(kind: String?, path: String?, hex: String?) throws {
        if kind == nil && path == nil && hex == nil {
            guard isWorkspace else { throw AtlasSearchError.invalidResponse }
            return
        }
        guard kind == self.kind.rawValue, let path, path.utf8.elementsEqual(self.path.utf8),
              hex == pathHex else { throw AtlasSearchError.invalidResponse }
    }
}

/// Frozen admission scope shared by live and captured-index queries. These are
/// bounded ASCII filename keys, never a glob, path grant or Unicode text query.
/// Empty included lists deliberately select no files; they never mean "all".
struct AtlasSearchFileScope: Equatable, Sendable {
    let extensions: String?
    let filenames: String?
    static let all = Self(extensions: nil, filenames: nil)
    var isAll: Bool { extensions == nil && filenames == nil }
    var wireKind: String { isAll ? "all" : "file-types" }

    private init(extensions: String?, filenames: String?) {
        self.extensions = extensions; self.filenames = filenames
    }

    init(extensions: [String], filenames: [String] = []) throws {
        self.extensions = try Self.canonical(extensions, filenames: false)
        self.filenames = try Self.canonical(filenames, filenames: true)
    }

    private static func canonical(_ values: [String], filenames: Bool) throws -> String {
        guard values.count <= 32 else { throw AtlasSearchError.invalidScope }
        var keys: [String] = []
        for value in values {
            let raw = value.utf8
            guard !raw.isEmpty, raw.count <= 64 else { throw AtlasSearchError.invalidScope }
            let lowered = raw.map { (65...90).contains($0) ? $0 + 32 : $0 }
            guard lowered.allSatisfy({
                (97...122).contains($0) || (48...57).contains($0)
                    || $0 == 43 || $0 == 45 || $0 == 95 || (filenames && $0 == 46)
            }), !filenames || (lowered != [46] && lowered != [46, 46]) else {
                throw AtlasSearchError.invalidScope
            }
            keys.append(String(decoding: lowered, as: UTF8.self))
        }
        let joined = Set(keys).sorted().joined(separator: ",")
        guard joined.utf8.count <= 1024 else { throw AtlasSearchError.invalidScope }
        return joined
    }

    /// Both non-null pointers remain borrowed only for the synchronous call.
    /// A non-null empty pair retains the distinction from the legacy all scope.
    func withCStrings<Result>(_ body: (UnsafePointer<CChar>?, UnsafePointer<CChar>?) throws -> Result) rethrows -> Result {
        guard let extensions, let filenames else { return try body(nil, nil) }
        return try extensions.withCString { ext in
            try filenames.withCString { names in try body(ext, names) }
        }
    }

    /// Validate a returned row against the admitted predicate without treating
    /// its escaped label or a lossy UTF-8 conversion as a native filename.
    func admitsRawPath(_ raw: [UInt8]) -> Bool {
        guard let extensions, let filenames else { return true }
        guard let last = raw.split(separator: 47).last else { return false }
        let name = last.map { (65...90).contains($0) ? $0 + 32 : $0 }
        if filenames.split(separator: ",").contains(where: { $0.utf8.elementsEqual(name) }) { return true }
        guard let dot = name.lastIndex(of: 46), dot > 0, dot + 1 < name.count else { return false }
        return extensions.split(separator: ",").contains { $0.utf8.elementsEqual(name[(dot + 1)...]) }
    }

    /// Legacy all-files envelopes may omit this additive scope metadata. A
    /// scoped request requires the exact echoed predicate on every page.
    func eligibleFiles(kind: String?, extensions: String?, filenames: String?,
                       eligible: String?, excluded: String?, catalogued: Int,
                       pathScoped: Bool = false) throws -> Int {
        if kind == nil && extensions == nil && filenames == nil && eligible == nil && excluded == nil {
            guard isAll && !pathScoped else { throw AtlasSearchError.invalidResponse }
            return catalogued
        }
        guard kind == wireKind, extensions == (self.extensions ?? ""), filenames == (self.filenames ?? ""),
              let eligible, let included = UInt64(eligible), String(included) == eligible,
              let excluded, let omitted = UInt64(excluded), String(omitted) == excluded,
              included <= UInt64(catalogued), omitted == UInt64(catalogued) - included,
              !isAll || pathScoped || omitted == 0 else { throw AtlasSearchError.invalidResponse }
        return Int(included)
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
    var pathKind: String = "workspace"
    var path: String = ""

    func matches(_ other: Self) -> Bool {
        loadGeneration == other.loadGeneration && atlasRevision == other.atlasRevision
            && root.utf8.elementsEqual(other.root.utf8)
            && query.utf8.elementsEqual(other.query.utf8)
            && scope.utf8.elementsEqual(other.scope.utf8)
            && customExtensions.utf8.elementsEqual(other.customExtensions.utf8)
            && pathKind.utf8.elementsEqual(other.pathKind.utf8)
            && path.utf8.elementsEqual(other.path.utf8)
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

/// File-type selection shared by atlas membership and search admission.
enum AtlasFileScope: String, CaseIterable, Identifiable {
    case all = "All files", markdown = "Markdown", python = "Python", rust = "Rust", custom = "Extension…"
    var id: String { rawValue }
    func searchScope(custom: String = "") throws -> AtlasSearchFileScope {
        switch self {
        case .all: return .all
        case .markdown: return try AtlasSearchFileScope(extensions: ["md", "markdown", "mdown"])
        case .python: return try AtlasSearchFileScope(extensions: ["py", "pyi", "pyw"])
        case .rust:
            return try AtlasSearchFileScope(extensions: ["rs"],
                filenames: ["cargo.toml", "cargo.lock", "rust-toolchain.toml", "rust-toolchain"])
        case .custom:
            guard custom.utf8.count <= 1024 else { throw AtlasSearchError.invalidScope }
            let tokens = custom.split { $0 == "," || $0.isWhitespace }
                .map { $0.trimmingCharacters(in: CharacterSet(charactersIn: ".")) }
            return try AtlasSearchFileScope(extensions: tokens)
        }
    }
    func includes(_ path: String, custom: String = "") -> Bool {
        // Displayed membership uses the same bounded ASCII predicate as the
        // engine request. Unicode folding must not admit files search excludes.
        guard let scope = try? searchScope(custom: custom) else { return false }
        return scope.admitsRawPath(Array(path.utf8))
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

/// Index construction and query verification are distinct work. In captured
/// mode the source universe is frozen until explicit refresh, never silently
/// presented as a fresh observation of the working tree.
struct AtlasSearchIndexStatus: Sendable {
    let building: Bool
    let reused: Bool
    let capturedFiles: Int
    let capturedBytes: UInt64
    let skippedFiles: Int
    let verifiedFiles: Int
    let verificationBytes: UInt64
}
