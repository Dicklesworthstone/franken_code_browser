import Foundation

private typealias ProjectPoll = @convention(c) (UnsafeMutableRawPointer?) -> Int32
@_silgen_name("fcb_project_catalog_with_policy_cancelable") private func projectMetadata(
    _ root: UnsafePointer<CChar>, _ maxFiles: UInt64, _ discoveryPolicy: UInt64,
    _ poll: ProjectPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_project_directory_catalog_cancelable") private func projectDirectoryMetadata(
    _ root: UnsafePointer<CChar>, _ directory: UnsafePointer<UInt8>?, _ directoryLength: UInt64,
    _ maxFiles: UInt64, _ discoveryPolicy: UInt64,
    _ poll: ProjectPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_atlas_layout_cancelable") private func projectCatalog(
    _ root: UnsafePointer<CChar>, _ poll: ProjectPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_source_document_cancelable") private func projectSource(
    _ path: UnsafePointer<CChar>, _ poll: ProjectPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_source_document_cached_cancelable") private func projectCachedSource(
    _ handle: UInt64, _ path: UnsafePointer<CChar>, _ key: UnsafeMutablePointer<UInt8>,
    _ poll: ProjectPoll?, _ context: UnsafeMutableRawPointer?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_source_cache_get") private func projectArtifact(
    _ handle: UInt64, _ key: UnsafePointer<CChar>, _ length: UnsafeMutablePointer<UInt64>) -> UnsafeMutablePointer<UInt8>?
@_silgen_name("fcb_source_cache_put") private func projectPutArtifact(
    _ handle: UInt64, _ key: UnsafePointer<CChar>, _ bytes: UnsafePointer<UInt8>, _ length: UInt64) -> Bool
@_silgen_name("fcb_source_cache_repair") private func projectRepairArtifact(
    _ handle: UInt64, _ key: UnsafePointer<CChar>, _ bytes: UnsafePointer<UInt8>, _ length: UInt64) -> Bool
@_silgen_name("fcb_source_cache_free") private func projectFreeArtifact(_ pointer: UnsafeMutablePointer<UInt8>?, _ length: UInt64)
@_silgen_name("fcb_free_string") private func projectFreeString(_ pointer: UnsafeMutablePointer<CChar>?)

/// Metadata values only. Color/native layout is constructed by the main actor.
struct AtlasProjectFile: Sendable {
    let path: String
    let x, y, w, h: Double
    let bytes, lineCount: Int
    let sourceLineCount: Int?
    let profileState: String
    let profile: [UInt8]
}
struct AtlasProjectSourcePacket: Sendable {
    let json: Data
    let key: [UInt8]?
}

/// The existing Rust services do all discovery, source validation, highlighting,
/// and disk-cache policy. These routines run only on AtlasProjectIO's worker.
/// A null source is unavailable, never a fabricated empty capture. A null atlas
/// is an error, never a successful empty project. No native fonts or tiles here.
enum AtlasProjectWorker {
    private static let poll: ProjectPoll = { context in
        guard let context else { return 1 }
        return Unmanaged<AtlasSearchCancellation>.fromOpaque(context).takeUnretainedValue().isCanceled ? 1 : 0
    }
    static func check(_ flag: AtlasSearchCancellation) throws {
        if flag.isCanceled { throw AtlasProjectIOError.canceled }
    }
    private static func validate(_ path: String) throws {
        if path.isEmpty || path.utf8.count > 16_384 || path.utf8.contains(0) { throw AtlasProjectIOError.invalidRequest }
    }
    /// Versioned metadata route: partial membership and unsupported native names
    /// remain explicit. No legacy path-only response is accepted as this report.
    static func catalogReport(root: String, directory: AtlasDirectoryFocus = .project,
        discoveryPolicy: AtlasDiscoveryPolicy = .repositoryRules,
        cancellation: AtlasSearchCancellation) throws -> AtlasProjectCatalog {
        try validate(root); try check(cancellation)
        let pointer = withExtendedLifetime(cancellation) {
            root.withCString { root in
                let context = Unmanaged.passUnretained(cancellation).toOpaque()
                if directory.isProject {
                    return projectMetadata(root, UInt64(AtlasProjectCatalog.maximumFiles), discoveryPolicy.rawValue, poll, context)
                }
                return directory.rawPath.withUnsafeBufferPointer {
                    projectDirectoryMetadata(root, $0.baseAddress, UInt64($0.count),
                        UInt64(AtlasProjectCatalog.maximumFiles), discoveryPolicy.rawValue, poll, context)
                }
            }
        }
        defer { projectFreeString(pointer) }
        try check(cancellation)
        guard let pointer else { throw AtlasProjectIOError.unavailable }
        let length = strnlen(pointer, AtlasProjectCatalog.maximumResponseBytes + 1)
        guard length <= AtlasProjectCatalog.maximumResponseBytes else { throw AtlasProjectIOError.limit }
        let data = Data(bytes: pointer, count: length)
        let report: AtlasProjectCatalog
        if directory.isProject {
            report = try AtlasProjectCatalog.decode(data, requestedRoot: root, requestedPolicy: discoveryPolicy.wireName)
        } else {
            report = try AtlasProjectCatalog.decodeDirectory(data, requestedRoot: root, directory: directory,
                requestedPolicy: discoveryPolicy.wireName)
        }
        try check(cancellation)
        return report
    }

    static func catalog(root: String, cancellation: AtlasSearchCancellation) throws -> [AtlasProjectFile] {
        try validate(root); try check(cancellation)
        let pointer = withExtendedLifetime(cancellation) {
            root.withCString { projectCatalog($0, poll, Unmanaged.passUnretained(cancellation).toOpaque()) }
        }
        defer { projectFreeString(pointer) }
        try check(cancellation)
        guard let pointer else { throw AtlasProjectIOError.unavailable }
        let data = Data(bytes: pointer, count: strlen(pointer))
        let files = try decodeCatalog(data)
        try check(cancellation)
        return files
    }
    static func source(path: String, handle: UInt64 = 0, cancellation: AtlasSearchCancellation) throws -> AtlasProjectSourcePacket? {
        try validate(path); try check(cancellation)
        var key = [UInt8](repeating: 0, count: 32)
        let pointer = withExtendedLifetime(cancellation) {
            path.withCString { path in
                if handle == 0 { return projectSource(path, poll, Unmanaged.passUnretained(cancellation).toOpaque()) }
                return key.withUnsafeMutableBufferPointer {
                    projectCachedSource(handle, path, $0.baseAddress!, poll, Unmanaged.passUnretained(cancellation).toOpaque())
                }
            }
        }
        defer { projectFreeString(pointer) }
        try check(cancellation)
        guard let pointer else { return nil }
        return AtlasProjectSourcePacket(json: Data(bytes: pointer, count: strlen(pointer)), key: handle == 0 ? nil : key)
    }
    static func artifact(handle: UInt64, key: String, limit: Int, cancellation: AtlasSearchCancellation) throws -> Data? {
        try check(cancellation)
        guard limit >= 0, limit <= 64 * 1024 * 1024, key.utf8.count == 64 else { throw AtlasProjectIOError.invalidRequest }
        var length: UInt64 = 0
        let pointer = key.withCString { projectArtifact(handle, $0, &length) }
        defer { projectFreeArtifact(pointer, length) }
        try check(cancellation)
        guard let pointer else { return nil }
        guard length <= UInt64(limit) else { throw AtlasProjectIOError.limit }
        return Data(bytes: pointer, count: Int(length))
    }

    static func storeArtifact(handle: UInt64, key: String, data: Data, repair: Bool,
                              cancellation: AtlasSearchCancellation) throws -> Bool {
        try check(cancellation)
        guard handle != 0, key.utf8.count == 64, key.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
              !data.isEmpty, data.count <= 64 * 1024 * 1024 else { throw AtlasProjectIOError.invalidRequest }
        let saved = key.withCString { key in
            data.withUnsafeBytes { bytes in
                let pointer = bytes.bindMemory(to: UInt8.self).baseAddress!
                return repair ? projectRepairArtifact(handle, key, pointer, UInt64(data.count))
                    : projectPutArtifact(handle, key, pointer, UInt64(data.count))
            }
        }
        // A foreign write already in flight can finish an immutable artifact;
        // cancellation still suppresses the source/document publication.
        try check(cancellation)
        return saved
    }

    static func decodeCatalog(_ data: Data) throws -> [AtlasProjectFile] {
        struct Envelope: Decodable { let files: [File] }
        struct File: Decodable {
            let path: String
            let x, y, w, h: Double
            let bytes, n: Int
            let source_lines: String?
            let profile_state: String
            let tex: String?
        }
        guard data.count <= 16 * 1024 * 1024,
              let envelope = try? JSONDecoder().decode(Envelope.self, from: data),
              envelope.files.count <= 65_536 else { throw AtlasProjectIOError.invalidResponse }
        var seen = Set<String>()
        return try envelope.files.map { file in
            guard !file.path.isEmpty, file.path.utf8.count <= 16_384, !file.path.utf8.contains(0),
                  !file.path.hasPrefix("/"), !file.path.split(separator: "/", omittingEmptySubsequences: false).contains(where: { $0.isEmpty || $0 == "." || $0 == ".." }),
                  seen.insert(file.path).inserted, file.bytes >= 0, (0...16_384).contains(file.n),
                  [file.x, file.y, file.w, file.h].allSatisfy({ $0.isFinite && $0 >= 0 }) else { throw AtlasProjectIOError.invalidResponse }
            let profile = file.tex.flatMap { Data(base64Encoded: $0) }.map(Array.init) ?? []
            guard profile.count == file.n * 2 else { throw AtlasProjectIOError.invalidResponse }
            let sourceLines = file.source_lines.flatMap(Int.init)
            if let count = file.source_lines, sourceLines.map({ $0 >= 0 && String($0) == count }) != true { throw AtlasProjectIOError.invalidResponse }
            return AtlasProjectFile(path: file.path, x: file.x, y: file.y, w: file.w, h: file.h,
                bytes: file.bytes, lineCount: file.n, sourceLineCount: sourceLines, profileState: file.profile_state, profile: profile)
        }
    }
}
