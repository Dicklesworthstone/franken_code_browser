import Foundation
#if canImport(CryptoKit)
import CryptoKit
#endif

/// Discovery scope is an explicit user choice, independent of file-type and
/// path filters. It never changes a root grant or enables links/special files.
enum AtlasDiscoveryPolicy: UInt64, Sendable {
    case repositoryRules = 0
    case includeExcluded = 1

    var wireName: String {
        self == .repositoryRules ? "repository-rules-v1" : "all-regular-files/no-rule-files-v1"
    }
    var includesExcluded: Bool { self == .includeExcluded }
    /// Retained atlas receipts can include the frozen rule-observation hash.
    /// A policy switch needs a new owner; that hash stays exact for its pages.
    func admitsAtlasReceipt(_ name: String) -> Bool {
        if name == wireName { return true }
        guard self == .repositoryRules else { return false }
        let prefix = wireName + ":"
        guard name.hasPrefix(prefix) else { return false }
        let fingerprint = name.utf8.dropFirst(prefix.utf8.count)
        return fingerprint.count == 64 && fingerprint.allSatisfy {
            (48...57).contains($0) || (97...102).contains($0)
        }
    }
    func admits(_ name: String, legacyDefault: Bool = false) -> Bool {
        name == wireName || (legacyDefault && self == .repositoryRules
            && name == "product-defaults/no-rule-files-v1")
    }
}

struct SearchHit: Identifiable, Hashable, Sendable {
    let id: Int
    /// Escaped presentation label is never used as a filesystem path.
    let path: String
    let sourcePath: String?
    let start: UInt64
    let end: UInt64
    let captureSHA256: String?
    let captureByteLength: UInt64?
    var structure: AtlasSearchStructure? = nil
    /// Engine-supplied source spelling. This authorizes an exact retained-file
    /// Find after a case-insensitive workspace query; it is never normalized.
    var actualText: String? = nil
    /// Bounded presentation from this hit's retained capture. Navigation still
    /// uses the occurrence identity and original range, never this preview.
    var excerpt: AtlasSearchExcerpt? = nil
    var fileName: String { (path as NSString).lastPathComponent }
    var folder: String { (path as NSString).deletingLastPathComponent }
}

struct AtlasSearchExcerpt: Hashable, Sendable {
    static let maximumBytes = 8192
    let text: String
    let originalStart, originalEnd, matchStart, matchEnd: UInt64
    let hasReplacements, clippedStart, clippedEnd: Bool
    /// Prepared on the worker so drawing a row never scans source context.
    let leading, matched, trailing: String
    let retainedByteCount: Int

    fileprivate init(text: String, originalStart: UInt64, originalEnd: UInt64,
                     matchStart: UInt64, matchEnd: UInt64, before: String,
                     match: String, after: String, hasReplacements: Bool,
                     clippedStart: Bool, clippedEnd: Bool) {
        self.text = text; self.originalStart = originalStart; self.originalEnd = originalEnd
        self.matchStart = matchStart; self.matchEnd = matchEnd
        self.hasReplacements = hasReplacements; self.clippedStart = clippedStart; self.clippedEnd = clippedEnd
        func lineBreaks(_ value: String) -> String {
            value.replacingOccurrences(of: "\r\n", with: "\n").replacingOccurrences(of: "\r", with: "\n")
        }
        func visible(_ value: String) -> String {
            lineBreaks(value).replacingOccurrences(of: "\n", with: " ↵ ")
                .replacingOccurrences(of: "\t", with: "⇥").replacingOccurrences(of: "\0", with: "␀")
        }
        let prefixLine = lineBreaks(before).components(separatedBy: "\n").last ?? ""
        let suffixLine = lineBreaks(after).components(separatedBy: "\n").first ?? ""
        let prefix = String(prefixLine.suffix(48)), suffix = String(suffixLine.prefix(80))
        let middle = String(match.prefix(256))
        leading = (clippedStart || !prefix.utf8.elementsEqual(before.utf8) ? "…" : "") + visible(prefix)
        matched = visible(middle) + (middle.utf8.elementsEqual(match.utf8) ? "" : "…")
        trailing = visible(suffix) + (clippedEnd || !suffix.utf8.elementsEqual(after.utf8) ? "…" : "")
        retainedByteCount = text.utf8.count + leading.utf8.count + matched.utf8.count + trailing.utf8.count
    }
    static func == (a: Self, b: Self) -> Bool {
        a.originalStart == b.originalStart && a.originalEnd == b.originalEnd
            && a.matchStart == b.matchStart && a.matchEnd == b.matchEnd
            && a.hasReplacements == b.hasReplacements && a.clippedStart == b.clippedStart
            && a.clippedEnd == b.clippedEnd && a.text.utf8.elementsEqual(b.text.utf8)
    }
    func hash(into hasher: inout Hasher) {
        hasher.combine(originalStart); hasher.combine(originalEnd)
        hasher.combine(matchStart); hasher.combine(matchEnd)
        hasher.combine(hasReplacements); hasher.combine(clippedStart); hasher.combine(clippedEnd)
        for byte in text.utf8 { hasher.combine(byte) }
    }
    func matchesSourceText(_ sourceText: String) -> Bool {
        text.utf8.dropFirst(Int(matchStart)).prefix(Int(matchEnd - matchStart))
            .elementsEqual(sourceText.utf8)
    }
}

/// Mandatory occurrence witnesses retain their existing allowance. Optional
/// preview text has its own fixed allowance so it can never starve exact hits.
/// These are retained-text bounds, not a total or peak process-memory claim.
enum AtlasSearchRetention {
    static let witnessBytes = 8 * 1024 * 1024
    static let excerptBytes = 2 * 1024 * 1024
}

/// The shared Rust decoder proves original-byte mappings. Native validation
/// checks the bounded receipt and exact spelling without inventing a decoder.
struct AtlasSearchExcerptWire: Decodable, Equatable {
    struct Range: Decodable, Equatable { let start, end: String }
    let text: String
    let original_range, match_utf8_range: Range
    let has_replacements, clipped_start, clipped_end: Bool

    func validated(hitStart: UInt64, hitEnd: UInt64, capturedBytes: UInt64,
                   actualText: String?) throws -> AtlasSearchExcerpt {
        func number(_ text: String) throws -> UInt64 {
            guard let value = UInt64(text), String(value) == text else { throw AtlasSearchError.invalidResponse }
            return value
        }
        let start = try number(original_range.start), end = try number(original_range.end)
        let lower = try number(match_utf8_range.start), upper = try number(match_utf8_range.end)
        let count = text.utf8.count
        guard start <= hitStart, hitStart < hitEnd, hitEnd <= end, end <= capturedBytes,
              end - start <= UInt64(AtlasSearchExcerpt.maximumBytes),
              count <= AtlasSearchExcerpt.maximumBytes, lower < upper, upper <= UInt64(count),
              (clipped_start ? start > 0 : start <= 3), clipped_end == (end < capturedBytes) else {
            throw AtlasSearchError.invalidResponse
        }
        let bytes = Array(text.utf8), from = Int(lower), to = Int(upper)
        guard let before = String(bytes: bytes[..<from], encoding: .utf8),
              let match = String(bytes: bytes[from..<to], encoding: .utf8),
              let after = String(bytes: bytes[to...], encoding: .utf8),
              actualText == nil || match.utf8.elementsEqual(actualText!.utf8) else {
            throw AtlasSearchError.invalidResponse
        }
        return AtlasSearchExcerpt(text: text, originalStart: start, originalEnd: end,
            matchStart: lower, matchEnd: upper, before: before, match: match, after: after,
            hasReplacements: has_replacements, clippedStart: clipped_start, clippedEnd: clipped_end)
    }
    static func == (a: Self, b: Self) -> Bool {
        a.original_range == b.original_range && a.match_utf8_range == b.match_utf8_range
            && a.has_replacements == b.has_replacements && a.clipped_start == b.clipped_start
            && a.clipped_end == b.clipped_end && a.text.utf8.elementsEqual(b.text.utf8)
    }
}

/// Search boundary checks only. Matching and Unicode lowercase policy remain
/// in Rust; native code retains the spelling and coordinates that Rust proved.
enum AtlasSearchText {
    static let maximumActualTextBytes = 4096
    static func wireMode(caseSensitive: Bool) -> String {
        caseSensitive ? "exact-decoded-literal" : "unicode-lowercase-literal"
    }
    static func validActualText(_ text: String) -> Bool {
        !text.isEmpty && text.utf8.count <= maximumActualTextBytes && !text.utf8.contains(0)
    }
    static func accepts(actualText: String?, needle: String, caseSensitive: Bool) -> Bool {
        guard let actualText else { return caseSensitive } // Legacy exact envelopes.
        return validActualText(actualText)
            && (!caseSensitive || actualText.utf8.elementsEqual(needle.utf8))
    }
    static func sameActualText(_ lhs: String?, _ rhs: String?) -> Bool {
        switch (lhs, rhs) {
        case (.none, .none): return true
        case (.some(let a), .some(let b)): return a.utf8.elementsEqual(b.utf8)
        default: return false
        }
    }
}

/// Each mode is a distinct shared-engine request. Whole-token occurrences
/// include comments and literals; declarations remain heuristic candidates.
enum AtlasSearchMode: String, CaseIterable, Identifiable, Sendable {
    case text, expression, symbols, headings, occurrences
    var id: String { rawValue }
    var isTextual: Bool { self == .text || self == .expression }
    var supportsNameMatching: Bool { self == .symbols || self == .headings }
    func effectiveNameMode(_ supplied: AtlasSearchNameMode) -> AtlasSearchNameMode {
        self == .occurrences ? .exact : (isTextual ? .contains : supplied)
    }
    var label: String {
        switch self {
        case .text: return "Text"
        case .expression: return "Expression"
        case .symbols: return "Symbols"
        case .headings: return "Headings"
        case .occurrences: return "Occurrences"
        }
    }
    var code: UInt64 {
        switch self { case .text: return 0; case .symbols: return 1; case .headings: return 2; case .expression: return 3; case .occurrences: return 4 }
    }
    var wireMode: String {
        switch self {
        case .text: return "exact-decoded-literal"
        case .expression: return "decoded-text-expression"
        case .symbols: return "symbol-name"
        case .headings: return "markdown-heading"
        case .occurrences: return "identifier-occurrence"
        }
    }
    var evidence: String {
        switch self {
        case .text: return "exact-decoded-literal"
        case .expression: return "decoded-text-expression"
        case .symbols: return "heuristic-outline-candidate"
        case .headings: return "fmd-heading"
        case .occurrences: return "whole-token-text-candidate"
        }
    }
    var structureFileByteLimit: UInt64 {
        switch self {
        case .symbols: return 1024 * 1024
        case .headings: return 262_144
        case .occurrences: return 32 * 1024 * 1024
        case .text, .expression: return 0
        }
    }
    /// The atlas captures at most 1 MiB per file even when the shared extractor
    /// can analyze larger Reader and saved-repository captures.
    var atlasStructureFileByteLimit: UInt64 {
        min(structureFileByteLimit, 1024 * 1024)
    }
    var resultLabel: String {
        switch self {
        case .text: return "exact matches"
        case .expression: return "expression matches"
        case .symbols: return "symbol candidates"
        case .headings: return "Markdown headings"
        case .occurrences: return "whole-token occurrences"
        }
    }
    var explanation: String {
        switch self {
        case .text: return "Exact, case-sensitive decoded text in the admitted files."
        case .expression: return "Quoted phrases, required terms, -excluded terms, path: and lang: filters. All terms apply to the same captured file; results select the first positive term."
        case .symbols: return "Case-sensitive heuristic declaration candidates in Rust, Python, JavaScript, TypeScript, Go, C/C++ and Swift. Compiler resolution and references are not claimed."
        case .headings: return "Case-sensitive Markdown heading names from the shared parser, with canonical slugs and original source ranges."
        case .occurrences: return "Exact, case-sensitive whole-token names, including comments and literals. Shared token rules preserve Unicode spelling without normalization; results do not establish compiler-resolved references. Atlas captures admit up to 1 MiB per file; Reader and saved-repository captures admit up to 32 MiB. Unsupported text encoding remains outside coverage."
        }
    }
    var placeholder: String {
        switch self {
        case .text: return "Text in project…"
        case .expression: return "\"pub fn\" Result -unsafe lang:rust"
        case .symbols: return "Declaration name…"
        case .headings: return "Markdown heading…"
        case .occurrences: return "Exact identifier name…"
        }
    }
}

/// This identifies the shared engine's policy. Native code does not implement
/// token boundaries or normalize an identifier before sending it to Rust.
enum AtlasSearchOccurrencePolicy {
    static let tokenPolicy = "ascii-alphanumeric-underscore-dollar-plus-nonascii-nonwhitespace-noncontrol-v1"
    static let nameSyntax = "single-token-no-leading-ascii-digit"
    static func queryFailure(in data: Data) -> AtlasSearchError? {
        struct Envelope: Decodable {
            struct Failure: Decodable { let code: String }
            let schema, status: String
            let error: Failure
        }
        guard let value = try? JSONDecoder().decode(Envelope.self, from: data),
              ["fcb.atlas-search/1", "fcb.atlas-session/1"].contains(value.schema),
              value.status == "error", value.error.code == "REFERENCE_INVALID_NAME" else { return nil }
        return .query(value.error.code)
    }
}

enum AtlasSearchNameMode: String, CaseIterable, Identifiable, Sendable {
    case exact, prefix, contains
    var id: String { rawValue }
    var label: String {
        switch self { case .exact: return "Exact name"; case .prefix: return "Starts with"; case .contains: return "Contains" }
    }
    var code: UInt64 {
        switch self { case .exact: return 0; case .prefix: return 1; case .contains: return 2 }
    }
}

struct AtlasSearchStructure: Hashable, Sendable {
    let name, kind, language, evidence: String
    let candidate: UInt64
    let headingSlug: String?
    let evidenceStart, evidenceEnd: UInt64
    /// One-based match ordinal across the frozen captured-index query. Legacy
    /// and live extraction do not provide a cross-window occurrence identity.
    var occurrence: UInt64? = nil
    var mode: AtlasSearchMode {
        evidence == AtlasSearchMode.occurrences.evidence ? .occurrences : (headingSlug == nil ? .symbols : .headings)
    }
    var label: String {
        if mode == .occurrences { return "Whole-token text occurrence · comments and literals included" }
        if let headingSlug { return "Markdown heading · #\(headingSlug)" }
        return "\(language) · \(kind) · heuristic candidate"
    }
    static func == (a: Self, b: Self) -> Bool {
        a.name.utf8.elementsEqual(b.name.utf8) && a.kind == b.kind
            && a.language == b.language && a.evidence == b.evidence && a.candidate == b.candidate
            && AtlasSearchText.sameActualText(a.headingSlug, b.headingSlug)
            && a.evidenceStart == b.evidenceStart && a.evidenceEnd == b.evidenceEnd
            && a.occurrence == b.occurrence
    }
    func hash(into hasher: inout Hasher) {
        hasher.combine(name.utf8.count)
        for byte in name.utf8 { hasher.combine(byte) }
        hasher.combine(kind); hasher.combine(language); hasher.combine(evidence); hasher.combine(candidate)
        hasher.combine(headingSlug != nil)
        if let headingSlug { for byte in headingSlug.utf8 { hasher.combine(byte) } }
        hasher.combine(evidenceStart); hasher.combine(evidenceEnd)
        hasher.combine(occurrence)
    }
}

/// These counters describe bounded extraction coverage, not compiler semantics.
struct AtlasSearchStructureCoverage: Equatable, Sendable {
    let fileByteLimit: UInt64
    let unsupportedFiles, extractionLimitedFiles, metadataOmitted, metadataBytes: Int

    func advances(_ previous: Self) -> Bool {
        fileByteLimit == previous.fileByteLimit
            && unsupportedFiles >= previous.unsupportedFiles
            && extractionLimitedFiles >= previous.extractionLimitedFiles
            && metadataOmitted >= previous.metadataOmitted
            && metadataBytes >= previous.metadataBytes
    }
}

enum AtlasSearchError: Error, Sendable {
    case unavailable, invalidResponse, canceled, invalidRequest, invalidScope, invalidPathScope, invalidVisibleScope, identityExhausted, unsupportedMatching, unsupportedCounting, unsupportedDiscoveryPolicy, invalidCatalogScope, unsupportedCatalogScope
    case query(String)
    var message: String {
        switch self {
        case .canceled: return "Search canceled. In-flight reads may finish, but their results will not be shown."
        case .invalidRequest: return "Choose a project and enter 1–1024 UTF-8 bytes of search text without NUL characters."
        case .invalidVisibleScope: return "This view contains too many or unsupported file paths. Zoom in, then use the current view again."
        case .invalidScope: return "Choose up to 32 file extensions using letters, digits, +, - or _. Separate extensions with commas or spaces."
        case .invalidPathScope: return "Enter a project-relative file or folder path of 1–4096 UTF-8 bytes. Do not use leading/trailing slashes, empty components, or '.'/'..' components."
        case .identityExhausted: return "This search session exhausted its request identities. Reopen the window before searching again."
        case .unsupportedMatching: return "This search connection does not support case-insensitive text. Turn on Match case to search."
        case .unsupportedCounting: return "This search connection cannot honor the requested count policy. Reconnect to search."
        case .unsupportedDiscoveryPolicy: return "This search connection cannot include ignored and generated files. Reconnect to search the selected discovery scope."
        case .invalidCatalogScope: return "Choose a project-relative file or folder of at most 2,048 original path bytes, with no empty or dot components and at most 32 file or 31 folder components."
        case .unsupportedCatalogScope: return "This search connection cannot discover the selected file or folder independently. Reconnect before searching this scope."
        case .unavailable: return "Search unavailable. The project could not be read or the search exceeded its limits."
        case .invalidResponse: return "Search response could not be read. Results are unavailable, not an empty match set."
        case .query(let code):
            switch code {
            case "REFERENCE_INVALID_NAME": return "Enter one identifier token of at most 1,024 UTF-8 bytes, without whitespace. ASCII token characters are letters, digits, _ and $; a name cannot begin with an ASCII digit. The shared token rules preserve non-ASCII spelling."
            case "QUERY_EMPTY": return "Include a positive search term or quoted phrase in the expression."
            case "QUERY_SYNTAX_ERROR": return "The expression has invalid syntax. Check quoted phrases, escapes, and nonempty path: or lang: filters."
            case "QUERY_REGEX_UNQUALIFIED": return "Regular expressions are not supported. Use literal terms, quoted phrases, exclusions, and path: or lang: filters."
            case "QUERY_NEEDLE_TOO_LONG": return "Keep the expression within 1,024 UTF-8 bytes."
            case "QUERY_LIMIT_EXCEEDED": return "The expression exceeds its supported limits, including at most 64 terms and filters."
            default: return "The search expression was refused (\(code))."
            }
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
    var countAllMatches: Bool = false
    var matchCountComplete: Bool = false
    var progress: AtlasSearchProgress? = nil
    /// One identity for an append-only stream; legacy one-shot reports use nil.
    var streamID: UUID? = nil
    /// Optional immutable-source capability; legacy one-shot reports have none.
    var capture: AtlasSearchCapture? = nil
    var indexStatus: AtlasSearchIndexStatus? = nil
    var fileScope: AtlasSearchFileScope = .all
    var excludedFiles: Int = 0
    var pathScope: AtlasSearchPathScope = .workspace
    var mode: AtlasSearchMode = .text
    var nameMode: AtlasSearchNameMode = .contains
    var structureCoverage: AtlasSearchStructureCoverage? = nil
    var caseSensitive: Bool = true
    /// Requested visible paths absent from the search service's independent
    /// catalog. These are not excluded files or proof of an empty region.
    var visibleMissingFiles: Int = 0
    var expression: AtlasSearchExpression? = nil
    var discoveryPolicy: AtlasDiscoveryPolicy = .repositoryRules
    var catalogScope: AtlasSearchCatalogScope = .workspace
    var discoveryCoverage: AtlasSearchDiscoveryCoverage? = nil
    /// One bounded result window over the same explicitly retained index.
    var resultWindow: AtlasSearchResultWindow? = nil
    var isInProgress: Bool { progress?.isRunning == true }
    var isWorkspaceScope: Bool { fileScope.isAll && pathScope.isWorkspace }
    var resultLabel: String { mode == .text && !caseSensitive ? "case-insensitive matches" : mode.resultLabel }

    var summary: String {
        if let indexStatus, indexStatus.building, let progress {
            return "Preparing captured index for \(indexStatus.captureScopeDescription): \(progress.examinedFiles) of \(progress.cataloguedFiles) eligible files examined."
                + (indexStatus.captureMissingVisibleFiles == 0 ? "" : " \(indexStatus.captureMissingVisibleFiles) requested files are outside this search catalog.")
                + " Query results are not ready yet."
                + (discoveryPolicy.includesExcluded ? " Ignored and generated files included." : "")
        }
        return resultSummary
            + (isWorkspaceScope ? "" : " \(excludedFiles) catalogued files outside this scope.")
            + (visibleMissingFiles == 0 ? "" : " \(visibleMissingFiles) requested files are outside this search catalog.")
            + ((expression?.excludedFiles ?? 0) == 0 ? "" : " \(expression?.excludedFiles ?? 0) files excluded by expression path/language filters.")
            + (discoveryPolicy.includesExcluded ? " Ignored and generated files included." : "")
            + (indexStatus == nil ? "" : " Captured index; Refresh to include file edits.")
            + ((indexStatus?.uncapturedFiles ?? 0) == 0 ? "" : " \(indexStatus?.uncapturedFiles ?? 0) eligible files were not captured. Refresh to capture the current scope within its limits.")
    }
    private var resultSummary: String {
        if catalogScope.kind == .file, hits.isEmpty, let explanation = discoveryCoverage?.fileExplanation(policy: discoveryPolicy) {
            return explanation
        }
        let count = hits.count
        let nounCount = countAllMatches || resultWindow != nil ? matchesSeen : UInt64(count)
        let noun: String
        switch mode {
        case .text:
            noun = caseSensitive ? (nounCount == 1 ? "exact match" : "exact matches")
                : (nounCount == 1 ? "case-insensitive match" : "case-insensitive matches")
        case .expression: noun = nounCount == 1 ? "expression match" : "expression matches"
        case .symbols: noun = nounCount == 1 ? "symbol candidate" : "symbol candidates"
        case .headings: noun = nounCount == 1 ? "heading" : "headings"
        case .occurrences: noun = nounCount == 1 ? "whole-token occurrence" : "whole-token occurrences"
        }
        let qualification = mode == .symbols
            ? " Heuristic declaration candidates; compiler resolution and reference completeness are not claimed."
            : (mode == .occurrences ? " Exact token spelling, including comments and literals; compiler resolution is not claimed."
                : (mode.isTextual && !caseSensitive ? " Compared using Unicode lowercase; source spelling is preserved." : ""))
        if let window = resultWindow {
            let shown = window.rangeLabel
            if let progress, progress.isRunning {
                return "Finding result window from match \(window.offset + 1): \(matchesSeen) occurrences verified; \(progress.examinedFiles) of \(progress.cataloguedFiles) eligible files examined."
                    + qualification
            }
            let found = matchCountComplete ? "\(matchesSeen) \(noun)" : "At least \(matchesSeen) \(noun)"
            var result = "\(found); \(shown)."
            if !complete {
                result += " Search coverage is partial."
                if let coverage = structureCoverage {
                    if unavailableFiles > 0 { result += " \(unavailableFiles) files with unavailable or incomplete analysis." }
                    if coverage.unsupportedFiles > 0 {
                        let reason = mode == .occurrences ? "files with unsupported text encoding" : "unsupported languages"
                        result += " \(coverage.unsupportedFiles) \(reason) within that total."
                    }
                    if coverage.extractionLimitedFiles > 0 { result += " \(coverage.extractionLimitedFiles) files reached extraction limits." }
                    if coverage.metadataOmitted > 0 { result += " \(coverage.metadataOmitted) matching results omitted by metadata limits." }
                } else {
                    if unavailableFiles > 0 { result += " \(unavailableFiles) unavailable files." }
                    if unsupportedFiles > 0 { result += " \(unsupportedFiles) unsupported text files." }
                }
            }
            if window.hasMore { result += " Next results opens the following window from these same captures." }
            return result + qualification
        }
        if let progress, progress.isRunning {
            let membership = isWorkspaceScope ? "catalogued" : "eligible"
            let found = countAllMatches
                ? "\(matchesSeen) matches counted so far; first \(count) retained"
                : "\(count) \(noun) found"
            return "Searching: \(found); \(progress.examinedFiles) of \(progress.cataloguedFiles) \(membership) files examined. Results are provisional." + qualification
        }
        if complete {
            let scope = pathScope.isWorkspace ? (fileScope.isAll ? "captured project" : "selected file types")
                : pathScope.selectionDescription
            if countAllMatches && matchCountComplete {
                if matchesSeen == 0 { return "No \(noun) in the \(scope)." + qualification }
                let retained = matchesSeen > UInt64(count) ? "; first \(count) retained" : ""
                return "\(matchesSeen) \(noun) in the \(scope)\(retained)." + qualification
            }
            return (count == 0 ? "No \(noun) in the \(scope)." : "\(count) \(noun) in the \(scope).") + qualification
        }
        var reasons: [String] = []
        if truncated && !countAllMatches { reasons.append("result limit reached") }
        if truncated && countAllMatches { reasons.append("total count is not complete") }
        if let coverage = structureCoverage {
            if unavailableFiles > 0 { reasons.append("\(unavailableFiles) files with unavailable or incomplete analysis") }
            if coverage.unsupportedFiles > 0 {
                let reason = mode == .occurrences ? "files with unsupported text encoding" : "unsupported languages"
                reasons.append("\(coverage.unsupportedFiles) \(reason) within that total")
            }
            if coverage.extractionLimitedFiles > 0 { reasons.append("\(coverage.extractionLimitedFiles) files reached extraction limits") }
            if coverage.metadataOmitted > 0 { reasons.append("\(coverage.metadataOmitted) matching results omitted by metadata limits") }
        } else {
            if unavailableFiles > 0 { reasons.append("\(unavailableFiles) unavailable files") }
            if unsupportedFiles > 0 { reasons.append("\(unsupportedFiles) unsupported text files") }
        }
        if reasons.isEmpty { reasons.append(isWorkspaceScope ? "project coverage incomplete" : "scope coverage incomplete") }
        let found = countAllMatches
            ? "\(matchesSeen) \(noun) counted; first \(count) retained"
            : "\(count) \(noun) shown"
        return "Partial search: \(found); \(reasons.joined(separator: ", ")). More matches may exist." + qualification
    }

    static func decode(_ json: String?) throws -> Self {
        guard let json else { throw AtlasSearchError.unavailable }
        guard let data = json.data(using: .utf8) else { throw AtlasSearchError.invalidResponse }
        do {
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            guard wire.schema == "fcb.cli/1", wire.status == "ok", wire.command == "search",
                  wire.scope == "workspace", wire.mode == "decoded-text-literal",
                  wire.case_sensitive != false,
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
        let case_sensitive: Bool?
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

/// Metadata discovery is bounded independently inside this exact scope.
/// It never changes the granted project root or the root-relative result paths.
struct AtlasSearchCatalogScope: Equatable, Sendable {
    enum Kind: String, Sendable { case workspace, directory, file }
    static let maximumDirectoryBytes = 2048
    static let maximumDirectoryComponents = 31
    static let maximumFileBytes = 2048
    static let maximumFileComponents = 32
    static let workspace = Self(validatedKind: .workspace, rawPath: [])
    let kind: Kind
    let rawPath: [UInt8]

    init(directory: [UInt8]) throws {
        try self.init(kind: directory.isEmpty ? .workspace : .directory, rawPath: directory)
    }
    init(file: [UInt8]) throws { try self.init(kind: .file, rawPath: file) }
    init(kind: Kind, rawPath: [UInt8]) throws {
        let byteLimit = kind == .file ? Self.maximumFileBytes : Self.maximumDirectoryBytes
        let componentLimit = kind == .file ? Self.maximumFileComponents : Self.maximumDirectoryComponents
        guard rawPath.count <= byteLimit,
              (kind == .workspace ? rawPath.isEmpty : !rawPath.isEmpty),
              rawPath.split(separator: 47, omittingEmptySubsequences: false).count <= componentLimit,
              rawPath.isEmpty || (!rawPath.contains(0)
                && !rawPath.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                    $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
                })) else { throw AtlasSearchError.invalidCatalogScope }
        self.kind = kind; self.rawPath = rawPath
    }
    private init(validatedKind: Kind, rawPath: [UInt8]) { kind = validatedKind; self.rawPath = rawPath }
    var isWorkspace: Bool { kind == .workspace }
    var directory: [UInt8] { kind == .directory ? rawPath : [] }
    var file: [UInt8] { kind == .file ? rawPath : [] }
    var pathHex: String {
        let digits = Array("0123456789abcdef".utf8)
        return String(decoding: rawPath.flatMap { [digits[Int($0 >> 4)], digits[Int($0 & 15)]] }, as: UTF8.self)
    }
    var directoryHex: String { kind == .directory ? pathHex : "" }
    func containsFile(_ raw: [UInt8]) -> Bool {
        switch kind {
        case .workspace: return true
        case .file: return raw == rawPath
        case .directory:
            return raw.count > rawPath.count + 1 && raw.starts(with: rawPath) && raw[rawPath.count] == 47
        }
    }
}

/// These immutable discovery counters are distinct from query/type exclusions
/// and source-capture failures. A skipped selected file is never a zero-match proof.
struct AtlasSearchDiscoveryCoverage: Equatable, Sendable {
    let excludedEntries, unavailableEntries, symlinksNotFollowed, specialObjects: UInt64

    init(excluded: String, unavailable: String, symlinks: String, special: String) throws {
        func number(_ text: String) throws -> UInt64 {
            guard let value = UInt64(text), String(value) == text else { throw AtlasSearchError.invalidResponse }
            return value
        }
        excludedEntries = try number(excluded); unavailableEntries = try number(unavailable)
        symlinksNotFollowed = try number(symlinks); specialObjects = try number(special)
    }
    func fileExplanation(policy: AtlasDiscoveryPolicy) -> String? {
        var messages: [String] = []
        if excludedEntries > 0 {
            messages.append("The selected file was excluded from discovery; its source was not searched.")
            if policy == .repositoryRules {
                messages.append("If a repository ignore rule or generated-file policy excluded it, try Include excluded files.")
            }
        }
        if unavailableEntries > 0 {
            messages.append("Some entries needed to discover the selected file were unavailable. Search coverage is incomplete.")
        }
        if symlinksNotFollowed > 0 {
            messages.append("A symlink in the selected path was not followed; the file was not searched.")
        }
        if specialObjects > 0 {
            messages.append("The selected path is a special filesystem object and was not searched.")
        }
        return messages.isEmpty ? nil : messages.joined(separator: " ")
    }
}

/// Opening fixes this discovery identity for every later retained response.
/// Legacy receipts may omit the entire scope or the additive file field only
/// for workspace/directory discovery; subsequent phases preserve that shape.
struct AtlasSearchCatalogReceipt: Equatable, Sendable {
    struct Path: Decodable, Sendable { let encoding, hex, display: String }
    static let legacyWorkspace = Self(scope: .workspace, hasEcho: false, hasFileEcho: false, discoveryCoverage: nil)
    let scope: AtlasSearchCatalogScope
    let hasEcho: Bool
    let hasFileEcho: Bool
    let discoveryCoverage: AtlasSearchDiscoveryCoverage?

    /// Decodable optionals cannot distinguish missing keys from JSON null.
    /// Preserve key presence independently from the validated path payload.
    @discardableResult
    static func validateFieldPresence(in data: Data) throws -> Fields {
        try JSONDecoder().decode(Fields.self, from: data)
    }
    struct Fields: Decodable, Sendable {
        enum Key: String, CodingKey {
            case discovery_scope, discovery_directory, discovery_file, path_basis
            case discovery_excluded_entries, discovery_unavailable_entries
            case discovery_symlinks_not_followed, discovery_special_objects
        }
        let hasEcho, hasFileEcho: Bool
        let discoveryCoverage: AtlasSearchDiscoveryCoverage?
        init(from decoder: Decoder) throws {
            let fields = try decoder.container(keyedBy: Key.self)
            let base: [Key] = [.discovery_scope, .discovery_directory, .path_basis]
            let count = base.filter { fields.contains($0) }.count
            hasEcho = count == base.count
            hasFileEcho = fields.contains(.discovery_file)
            guard count == 0 || hasEcho, hasEcho || !hasFileEcho else {
                throw AtlasSearchError.invalidResponse
            }
            let coverageKeys: [Key] = [.discovery_excluded_entries, .discovery_unavailable_entries,
                .discovery_symlinks_not_followed, .discovery_special_objects]
            let coverageCount = coverageKeys.filter { fields.contains($0) }.count
            guard coverageCount == 0 || (hasEcho && coverageCount == coverageKeys.count) else {
                throw AtlasSearchError.invalidResponse
            }
            if coverageCount == 0 {
                discoveryCoverage = nil
            } else {
                discoveryCoverage = try AtlasSearchDiscoveryCoverage(
                    excluded: fields.decode(String.self, forKey: .discovery_excluded_entries),
                    unavailable: fields.decode(String.self, forKey: .discovery_unavailable_entries),
                    symlinks: fields.decode(String.self, forKey: .discovery_symlinks_not_followed),
                    special: fields.decode(String.self, forKey: .discovery_special_objects))
            }
            if hasEcho {
                let kind = try fields.decode(String.self, forKey: .discovery_scope)
                _ = try fields.decode(String.self, forKey: .path_basis)
                _ = try fields.decodeIfPresent(Path.self, forKey: .discovery_directory)
                _ = try fields.decodeIfPresent(Path.self, forKey: .discovery_file)
                guard kind != "file" || (hasFileEcho && discoveryCoverage != nil) else { throw AtlasSearchError.invalidResponse }
            }
        }
    }
    private init(scope: AtlasSearchCatalogScope, hasEcho: Bool, hasFileEcho: Bool,
                 discoveryCoverage: AtlasSearchDiscoveryCoverage?) {
        self.scope = scope; self.hasEcho = hasEcho; self.hasFileEcho = hasFileEcho
        self.discoveryCoverage = discoveryCoverage
    }
    init(scope: AtlasSearchCatalogScope, kind: String?, directory: Path?, file: Path? = nil,
         pathBasis: String?, hasFileEcho: Bool = false,
         discoveryCoverage: AtlasSearchDiscoveryCoverage? = nil, requiresEcho: Bool = false) throws {
        if kind == nil && directory == nil && file == nil && pathBasis == nil && !hasFileEcho {
            guard scope.isWorkspace, !requiresEcho, discoveryCoverage == nil else { throw AtlasSearchError.invalidResponse }
            self.init(scope: scope, hasEcho: false, hasFileEcho: false, discoveryCoverage: nil)
            return
        }
        guard pathBasis == "project-root", kind == scope.kind.rawValue else {
            throw AtlasSearchError.invalidResponse
        }
        func matches(_ path: Path?) -> Bool {
            guard let path else { return false }
            return path.encoding == "unix-bytes" && path.hex.utf8.elementsEqual(scope.pathHex.utf8)
                && !path.display.isEmpty && path.display.utf8.count <= 16_384
        }
        switch scope.kind {
        case .workspace:
            guard directory == nil, file == nil else { throw AtlasSearchError.invalidResponse }
        case .directory:
            guard file == nil, matches(directory) else { throw AtlasSearchError.invalidResponse }
        case .file:
            guard hasFileEcho, discoveryCoverage != nil, directory == nil, matches(file) else { throw AtlasSearchError.invalidResponse }
        }
        self.init(scope: scope, hasEcho: true, hasFileEcho: hasFileEcho, discoveryCoverage: discoveryCoverage)
    }
    func validate(kind: String?, directory: Path?, file: Path? = nil,
                  pathBasis: String?, hasFileEcho: Bool = false,
                  discoveryCoverage: AtlasSearchDiscoveryCoverage? = nil) throws {
        guard try Self(scope: scope, kind: kind, directory: directory, file: file,
            pathBasis: pathBasis, hasFileEcho: hasFileEcho,
            discoveryCoverage: discoveryCoverage, requiresEcho: hasEcho) == self else {
            throw AtlasSearchError.invalidResponse
        }
    }
}

/// Validated, immutable input to the admitted search mode. Admission does
/// not trim whitespace or normalize Unicode: those bytes are the query.
/// A native capability for one immutable captured-index session and its exact
/// query policy. It grants no filesystem access and cannot prepare a replacement
/// index. UUID is private session identity, not a substitute for wire validation.
struct AtlasSearchResultWindowSource: Sendable {
    let index: UUID
    let root, query: String
    let fileScope: AtlasSearchFileScope
    let pathScope: AtlasSearchPathScope
    let mode: AtlasSearchMode
    let nameMode: AtlasSearchNameMode
    let caseSensitive, countAllMatches: Bool
    let discoveryPolicy: AtlasDiscoveryPolicy
    let catalogScope: AtlasSearchCatalogScope

    init(index: UUID, input: AtlasSearchInput, countAllMatches: Bool) {
        self.index = index; root = input.root; query = input.query
        fileScope = input.fileScope; pathScope = input.pathScope; mode = input.mode
        nameMode = input.nameMode
        caseSensitive = input.caseSensitive; self.countAllMatches = countAllMatches
        discoveryPolicy = input.discoveryPolicy
        catalogScope = input.catalogScope
    }
    func matches(_ input: AtlasSearchInput, countAllMatches: Bool) -> Bool {
        root.utf8.elementsEqual(input.root.utf8) && query.utf8.elementsEqual(input.query.utf8)
            && fileScope == input.fileScope && pathScope == input.pathScope && mode == input.mode
            && nameMode == input.nameMode
            && caseSensitive == input.caseSensitive && self.countAllMatches == countAllMatches
            && discoveryPolicy == input.discoveryPolicy && catalogScope == input.catalogScope
    }
}

struct AtlasSearchResultWindowRequest: Sendable {
    let source: AtlasSearchResultWindowSource
    let offset: UInt64
}

struct AtlasSearchResultWindow: Sendable {
    static let capacity: UInt64 = 1000
    let source: AtlasSearchResultWindowSource
    let offset, skipped: UInt64
    let retained: Int
    let hasMore: Bool
    let nextOffset: UInt64?
    /// Structural metadata limits can omit rows inside a covered window. The
    /// following request advances by match ordinals, not by displayed rows.
    var covered: UInt64? = nil
    var coveredMatches: UInt64 { covered ?? UInt64(retained) }

    var previous: AtlasSearchResultWindowRequest? {
        guard offset > 0 else { return nil }
        return AtlasSearchResultWindowRequest(source: source,
            offset: offset > Self.capacity ? offset - Self.capacity : 0)
    }
    var next: AtlasSearchResultWindowRequest? {
        guard hasMore, let nextOffset, coveredMatches > 0, nextOffset > offset else { return nil }
        return AtlasSearchResultWindowRequest(source: source, offset: nextOffset)
    }
    var rangeLabel: String {
        if coveredMatches > UInt64(retained) {
            return "showing \(retained) retained matches from matches \(offset + 1)–\(offset + coveredMatches)"
        }
        guard retained > 0 else { return "no retained matches in this window" }
        return "showing matches \(offset + 1)–\(offset + UInt64(retained))"
    }
}

struct AtlasSearchInput: Sendable {
    let root: String
    let query: String
    let fileScope: AtlasSearchFileScope
    let pathScope: AtlasSearchPathScope
    let mode: AtlasSearchMode
    let nameMode: AtlasSearchNameMode
    let caseSensitive: Bool
    /// The UI always freezes an explicit policy. nil preserves existing callers:
    /// exact text counts when its route supports counting; legacy transports
    /// and other modes stay bounded. Explicit choices are never substituted.
    let countAllMatches: Bool?
    let discoveryPolicy: AtlasDiscoveryPolicy
    let catalogScope: AtlasSearchCatalogScope
    let resultWindow: AtlasSearchResultWindowRequest?
    var countsLiveMatches: Bool { mode.isTextual && (countAllMatches ?? (mode == .text && caseSensitive)) }
    func countsIndexedMatches(supportsCounting: Bool) -> Bool {
        countAllMatches ?? (mode == .text && caseSensitive && supportsCounting)
    }
    var isWorkspaceScope: Bool { fileScope.isAll && pathScope.isWorkspace }

    init(root: String, query: String, fileScope: AtlasSearchFileScope = .all,
         pathScope: AtlasSearchPathScope = .workspace,
         mode: AtlasSearchMode = .text, nameMode: AtlasSearchNameMode = .contains,
         caseSensitive: Bool = true, countAllMatches: Bool? = nil,
         discoveryPolicy: AtlasDiscoveryPolicy = .repositoryRules,
         catalogScope: AtlasSearchCatalogScope = .workspace,
         resultWindow: AtlasSearchResultWindowRequest? = nil) throws {
        guard !root.isEmpty, root.utf8.count <= 16_384, !root.utf8.contains(0),
              !query.isEmpty, query.utf8.count <= 1_024, !query.utf8.contains(0) else {
            throw AtlasSearchError.invalidRequest
        }
        guard pathScope.isContained(in: catalogScope) else { throw AtlasSearchError.invalidCatalogScope }
        self.root = root
        self.query = query
        self.fileScope = fileScope
        self.pathScope = pathScope
        self.mode = mode
        self.nameMode = mode.effectiveNameMode(nameMode)
        self.caseSensitive = mode.isTextual ? caseSensitive : true
        self.countAllMatches = countAllMatches
        self.discoveryPolicy = discoveryPolicy
        self.catalogScope = catalogScope
        self.resultWindow = resultWindow
        if let resultWindow {
            guard resultWindow.offset <= 32 * 1024 * 1024,
                  resultWindow.source.matches(self,
                    countAllMatches: self.countAllMatches ?? resultWindow.source.countAllMatches) else {
                throw AtlasSearchError.invalidRequest
            }
        }
    }
    func requireLegacyScope() throws {
        guard catalogScope.isWorkspace else { throw AtlasSearchError.unsupportedCatalogScope }
        guard resultWindow == nil else { throw AtlasSearchError.unavailable }
        guard discoveryPolicy == .repositoryRules else { throw AtlasSearchError.unsupportedDiscoveryPolicy }
        guard mode == .text else { throw AtlasSearchError.invalidRequest }
        guard caseSensitive else { throw AtlasSearchError.unsupportedMatching }
        guard countAllMatches == nil else { throw AtlasSearchError.unsupportedCounting }
        guard pathScope.isWorkspace else { throw AtlasSearchError.invalidPathScope }
        guard fileScope.isAll else { throw AtlasSearchError.invalidScope }
    }
}

/// Exact relative location within the frozen catalog. This narrows a query;
/// it never confers a new root grant, follows a live selection or normalizes a
/// native pathname. Directory prefixes end at a slash, not an arbitrary byte.
struct AtlasSearchPathScope: Equatable, Sendable {
    enum Kind: String, CaseIterable, Identifiable, Sendable {
        case workspace, directory, file, visible
        var id: String { rawValue }
        var label: String { switch self { case .workspace: return "Workspace"; case .directory: return "Folder"; case .file: return "File"; case .visible: return "Visible atlas files" } }
        var code: UInt64 { switch self { case .workspace: return 0; case .directory: return 1; case .file: return 2; case .visible: return 3 } }
    }
    static let maximumVisibleFiles = 20_000
    static let maximumVisibleWireBytes = 1_048_576
    static let maximumVisiblePathBytes = 4096
    let kind: Kind
    let path: String
    let pathHex: String
    private let visiblePaths: [[UInt8]]?
    let visibleIdentity: String?
    private let visiblePathBytes: Int
    static let workspace = Self()
    var isWorkspace: Bool { kind == .workspace }
    var isVisible: Bool { kind == .visible }
    var visibleCount: Int? { visiblePaths?.count }
    var selectionDescription: String {
        switch kind {
        case .workspace: return "captured project"
        case .directory: return "selected folder \(String(reflecting: path))"
        case .file: return "selected file \(String(reflecting: path))"
        case .visible: return "frozen atlas region (\(visibleCount ?? 0) files)"
        }
    }

    private init() {
        kind = .workspace; path = ""; pathHex = ""
        visiblePaths = nil; visibleIdentity = nil; visiblePathBytes = 0
    }
    init(kind: Kind, path: String) throws {
        guard kind != .visible, path.utf8.count <= 4096,
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
        visiblePaths = nil; visibleIdentity = nil; visiblePathBytes = 0
        let digits = Array("0123456789abcdef".utf8)
        pathHex = String(decoding: raw.flatMap { [digits[Int($0 >> 4)], digits[Int($0 & 15)]] }, as: UTF8.self)
    }
    /// Worker-only construction of a frozen raw-path predicate. Exact native
    /// bytes, including undecodable names and Unix backslashes, survive intact.
    /// The canonical hex payload is hashed once and echoed by the shared host.
    init(visiblePaths input: [[UInt8]]) throws {
        guard input.count <= Self.maximumVisibleFiles else { throw AtlasSearchError.invalidVisibleScope }
        var inputBytes = 0
        for raw in input {
            guard !raw.isEmpty, raw.count <= Self.maximumVisiblePathBytes, !raw.contains(0),
                  !raw.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                      $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
                  }) else { throw AtlasSearchError.invalidVisibleScope }
            inputBytes += 2 * raw.count + (inputBytes == 0 ? 0 : 1)
            guard inputBytes <= Self.maximumVisibleWireBytes else { throw AtlasSearchError.invalidVisibleScope }
        }
        let ordered = input.sorted { $0.lexicographicallyPrecedes($1) }
        var paths: [[UInt8]] = []; paths.reserveCapacity(ordered.count)
        var rawBytes = 0, wireBytes = 0
        for raw in ordered where paths.last != raw {
            wireBytes += 2 * raw.count + (paths.isEmpty ? 0 : 1)
            rawBytes += raw.count; paths.append(raw)
        }
        let digits = Array("0123456789abcdef".utf8)
        var wire: [UInt8] = []; wire.reserveCapacity(wireBytes)
        for (index, raw) in paths.enumerated() {
            if index > 0 { wire.append(44) }
            for byte in raw { wire.append(digits[Int(byte >> 4)]); wire.append(digits[Int(byte & 15)]) }
        }
        kind = .visible; path = String(decoding: wire, as: UTF8.self); pathHex = ""
        visiblePaths = paths; visiblePathBytes = rawBytes
        #if canImport(CryptoKit)
        visibleIdentity = SHA256.hash(data: Data(wire)).map { String(format: "%02x", $0) }.joined()
        #else
        // The native app uses the macOS platform hash. Keep unrelated portable
        // model fixtures usable without installing or reimplementing crypto.
        // A host without that capability must never widen a visible request.
        throw AtlasSearchError.unavailable
        #endif
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
        if let visiblePaths {
            var lower = 0, upper = visiblePaths.count
            while lower < upper {
                let middle = lower + (upper - lower) / 2
                if visiblePaths[middle].lexicographicallyPrecedes(raw) { lower = middle + 1 }
                else { upper = middle }
            }
            return lower < visiblePaths.count && visiblePaths[lower] == raw
        }
        let prefix = Array(path.utf8)
        if kind == .file { return raw == prefix }
        return raw.count > prefix.count && raw.starts(with: prefix) && raw[prefix.count] == 47
    }
    /// A narrow metadata catalog cannot silently serve a wider query. Visible
    /// membership is checked using the exact frozen paths, including raw names.
    func isContained(in catalog: AtlasSearchCatalogScope) -> Bool {
        if catalog.isWorkspace { return true }
        if let visiblePaths { return visiblePaths.allSatisfy(catalog.containsFile) }
        if kind == .file { return catalog.containsFile(Array(path.utf8)) }
        if kind == .directory {
            guard catalog.kind == .directory else { return false }
            let raw = Array(path.utf8)
            return raw == catalog.directory || catalog.containsFile(raw)
        }
        return false
    }
    func validateEcho(kind: String?, path: String?, hex: String?, regionSHA256: String? = nil,
        regionFiles: String? = nil, regionPathBytes: String? = nil) throws {
        if isVisible {
            guard kind == "visible", path == "", hex == "", regionSHA256 == visibleIdentity,
                  regionFiles == String(visibleCount ?? 0), regionPathBytes == String(visiblePathBytes) else {
                throw AtlasSearchError.invalidResponse
            }
            return
        }
        guard regionSHA256 == nil, regionFiles == nil, regionPathBytes == nil else { throw AtlasSearchError.invalidResponse }
        if kind == nil && path == nil && hex == nil {
            guard isWorkspace else { throw AtlasSearchError.invalidResponse }
            return
        }
        guard kind == self.kind.rawValue, let path, path.utf8.elementsEqual(self.path.utf8),
              hex == pathHex else { throw AtlasSearchError.invalidResponse }
    }
    /// Membership counts remain in their own domain: missing requested paths
    /// must not be hidden among type exclusions or complete zero-hit results.
    func validateRegionCoverage(catalogued: String?, missing: String?, eligible: Int,
        searchCatalogFiles: Int, allFileTypes: Bool, complete: Bool) throws -> Int {
        guard eligible >= 0, searchCatalogFiles >= 0 else { throw AtlasSearchError.invalidResponse }
        guard isVisible else {
            guard catalogued == nil, missing == nil else { throw AtlasSearchError.invalidResponse }
            return 0
        }
        guard let catalogued, let known = UInt64(catalogued), String(known) == catalogued,
              let missing, let absent = UInt64(missing), String(absent) == missing,
              known <= UInt64(searchCatalogFiles), absent <= UInt64(visibleCount ?? 0),
              known <= UInt64(visibleCount ?? 0), known + absent == UInt64(visibleCount ?? 0),
              UInt64(eligible) <= known, !allFileTypes || UInt64(eligible) == known,
              !complete || absent == 0 else { throw AtlasSearchError.invalidResponse }
        return Int(absent)
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
    var mode: AtlasSearchMode = .text
    var nameMode: AtlasSearchNameMode = .contains
    var caseSensitive: Bool = true
    var countAllMatches: Bool? = nil
    var discoveryPolicy: AtlasDiscoveryPolicy = .repositoryRules
    var catalogKind: AtlasSearchCatalogScope.Kind = .workspace
    var catalogPath: [UInt8] = []

    func matches(_ other: Self) -> Bool {
        loadGeneration == other.loadGeneration && atlasRevision == other.atlasRevision
            && mode == other.mode && nameMode == other.nameMode && caseSensitive == other.caseSensitive
            && countAllMatches == other.countAllMatches && discoveryPolicy == other.discoveryPolicy
            && catalogKind == other.catalogKind && catalogPath == other.catalogPath
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
enum AtlasFileScope: String, CaseIterable, Identifiable, Sendable {
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
    var mode: AtlasSearchMode = .text
    var caseSensitive: Bool = true
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
    var captureFileScope: AtlasSearchFileScope = .all
    var capturePathScope: AtlasSearchPathScope = .workspace
    var captureEligibleFiles: Int? = nil
    var captureMissingVisibleFiles: Int = 0
    var uncapturedFiles: Int = 0

    var captureScopeDescription: String {
        let location = capturePathScope.isWorkspace ? "workspace" : capturePathScope.selectionDescription
        guard !captureFileScope.isAll else { return location }
        var filters: [String] = []
        if let extensions = captureFileScope.extensions, !extensions.isEmpty {
            filters.append("extensions " + extensions.split(separator: ",").map { ".\($0)" }.joined(separator: ", "))
        }
        if let filenames = captureFileScope.filenames, !filenames.isEmpty {
            filters.append("filenames " + filenames.replacingOccurrences(of: ",", with: ", "))
        }
        return location + (filters.isEmpty ? " with no included file types" : " with " + filters.joined(separator: "; "))
    }
}
