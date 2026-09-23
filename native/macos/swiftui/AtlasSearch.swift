import Foundation

struct SearchHit: Identifiable, Hashable {
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

enum AtlasSearchError: Error {
    case unavailable, invalidResponse
    var message: String {
        switch self {
        case .unavailable: return "Search unavailable. The project could not be read or the search exceeded its limits."
        case .invalidResponse: return "Search response could not be read. Results are unavailable, not an empty match set."
        }
    }
}

/// Adapts the shared engine's search envelope, without another search engine.
struct AtlasSearchReport {
    let hits: [SearchHit]
    let complete: Bool
    let truncated: Bool
    let unavailableFiles: Int
    let unsupportedFiles: Int
    let matchesSeen: UInt64

    var summary: String {
        let count = hits.count
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
