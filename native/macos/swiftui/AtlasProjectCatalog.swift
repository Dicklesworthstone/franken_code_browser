import Foundation

/// A directory within the already granted project, never a replacement root.
/// Identity stays in original Unix bytes; escaped labels are presentation only.
struct AtlasDirectoryFocus: Hashable, Sendable {
    enum ValidationError: Error { case invalidPath, limit }
    static let maximumPathBytes = 2048
    static let project = Self(validatedPath: [])
    let rawPath: [UInt8]

    init(rawPath: [UInt8]) throws {
        guard rawPath.count <= Self.maximumPathBytes else { throw ValidationError.limit }
        guard rawPath.isEmpty || Self.isRelativePath(rawPath) else { throw ValidationError.invalidPath }
        self.rawPath = rawPath
    }
    private init(validatedPath: [UInt8]) { rawPath = validatedPath }
    var isProject: Bool { rawPath.isEmpty }
    var parent: Self? {
        guard !isProject else { return nil }
        guard let slash = rawPath.lastIndex(of: 47) else { return .project }
        return Self(validatedPath: Array(rawPath[..<slash]))
    }
    static func containingFile(rawPath: [UInt8]) throws -> Self {
        guard rawPath.count <= 16_384 else { throw ValidationError.limit }
        guard Self.isRelativePath(rawPath) else { throw ValidationError.invalidPath }
        guard let slash = rawPath.lastIndex(of: 47) else { return .project }
        return try Self(rawPath: Array(rawPath[..<slash]))
    }
    func containsFile(_ path: [UInt8]) -> Bool {
        guard path.count <= 16_384, Self.isRelativePath(path) else { return false }
        return isProject || (path.count > rawPath.count + 1 && path.starts(with: rawPath)
            && path[rawPath.count] == 47)
    }
    var display: String { Self.display(rawPath: rawPath) }
    fileprivate static func display(rawPath: [UInt8]) -> String {
        guard !rawPath.isEmpty else { return "Project" }
        if let text = String(bytes: rawPath, encoding: .utf8) {
            return text.unicodeScalars.map { scalar in
                switch scalar.properties.generalCategory {
                case .control, .format, .lineSeparator, .paragraphSeparator:
                    return "\\u{\(String(scalar.value, radix: 16))}"
                default: return scalar.value == 92 ? "\\\\" : String(scalar)
                }
            }.joined()
        }
        return rawPath.map { byte in
            (32...126).contains(byte) && byte != 92 ? String(UnicodeScalar(byte))
                : "\\x" + (byte < 16 ? "0" : "") + String(byte, radix: 16)
        }.joined()
    }
    fileprivate static func isRelativePath(_ raw: [UInt8]) -> Bool {
        !raw.isEmpty && raw.first != 47 && !raw.contains(0)
            && !raw.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
            })
    }
}

/// One exact file within the granted project. Its containing directory is
/// presentation context, never a substitute for this catalog's raw identity.
struct AtlasFileFocus: Hashable, Sendable {
    enum ValidationError: Error { case invalidPath, limit }
    static let maximumPathBytes = 2048
    static let maximumComponents = 32
    let rawPath: [UInt8]
    let directory: AtlasDirectoryFocus

    init(rawPath: [UInt8]) throws {
        guard rawPath.count <= Self.maximumPathBytes,
              rawPath.split(separator: 47, omittingEmptySubsequences: false).count <= Self.maximumComponents else {
            throw ValidationError.limit
        }
        guard AtlasDirectoryFocus.isRelativePath(rawPath) else { throw ValidationError.invalidPath }
        directory = try AtlasDirectoryFocus.containingFile(rawPath: rawPath)
        self.rawPath = rawPath
    }
    var display: String { AtlasDirectoryFocus.display(rawPath: rawPath) }
}

/// The metadata scan's exact scope, independently of the current file-type
/// filter. A file and its parent folder remain distinct through all navigation.
enum AtlasCatalogTarget: Hashable, Sendable {
    case directory(AtlasDirectoryFocus)
    case file(AtlasFileFocus)
    static let project = Self.directory(.project)

    var directory: AtlasDirectoryFocus {
        switch self { case .directory(let value): return value; case .file(let value): return value.directory }
    }
    var file: AtlasFileFocus? { if case .file(let value) = self { return value }; return nil }
    var isProject: Bool { self == .project }
    var display: String {
        switch self { case .directory(let value): return value.display; case .file(let value): return value.display }
    }
    var parentDirectory: AtlasDirectoryFocus? {
        switch self { case .directory(let value): return value.parent; case .file(let value): return value.directory }
    }
    func containsFile(_ rawPath: [UInt8]) -> Bool {
        switch self {
        case .directory(let value): return value.containsFile(rawPath)
        case .file(let value): return value.rawPath == rawPath
        }
    }
}

/// First-use metadata from the shared Rust catalog. Source payload, syntax,
/// line counts and text previews are deliberately absent from this protocol.
struct AtlasProjectCatalog: Sendable {
    enum DecodeError: Error { case invalidResponse, limit }
    struct Entry: Sendable, Identifiable {
        let id: UInt64 // Local to this response, never a retained-reader handle.
        let rawPath: [UInt8]
        let displayPath: String
        let sourcePath: String?
        let observedBytes: UInt64
        let x, y, width, height: Double
    }
    let entries: [Entry]
    let discoveryComplete: Bool
    let policy: String
    /// Supplied by the requesting worker, NOT decoded from the response. This
    /// correlates startup selection with a catalog; it is not a native grant.
    let requestedRoot: String?
    let requestedTarget: AtlasCatalogTarget
    var requestedDirectory: AtlasDirectoryFocus { requestedTarget.directory }
    init(entries: [Entry], discoveryComplete: Bool, policy: String, requestedRoot: String? = nil,
        requestedDirectory: AtlasDirectoryFocus = .project) {
        self.init(entries: entries, discoveryComplete: discoveryComplete, policy: policy,
            requestedRoot: requestedRoot, requestedTarget: .directory(requestedDirectory))
    }
    init(entries: [Entry], discoveryComplete: Bool, policy: String, requestedRoot: String? = nil,
        requestedTarget: AtlasCatalogTarget) {
        self.entries = entries; self.discoveryComplete = discoveryComplete
        self.policy = policy; self.requestedRoot = requestedRoot; self.requestedTarget = requestedTarget
    }
    var openableCount: Int { entries.reduce(0) { $0 + ($1.sourcePath == nil ? 0 : 1) } }
    var summary: String {
        let unavailable = entries.count - openableCount
        var text = discoveryComplete ? "\(entries.count) catalogued files." : "Partial catalog: \(entries.count) known files; more may exist."
        if requestedTarget.file != nil {
            text = discoveryComplete ? "Single-file scope: \(entries.count) catalogued file."
                : "Single-file scope: discovery is incomplete; \(entries.count) known file."
        }
        if unavailable > 0 { text += " \(unavailable) filenames cannot be opened by this UTF-8 host." }
        return text + " Source previews are separate; files can be opened now."
    }

    static let maximumFiles = 20_000
    static let maximumResponseBytes = 16 * 1024 * 1024

    /// A requested policy binds the metadata receipt to its discovery scope.
    /// Snapshot-enabled hosts append the rule-observation fingerprint; retain
    /// that identity while checking the selected policy independently.
    func matchesDiscoveryPolicy(_ requested: String) -> Bool {
        Self.matchesDiscoveryPolicy(policy, requested: requested)
    }

    private static func matchesDiscoveryPolicy(_ reported: String, requested: String) -> Bool {
        switch requested {
        case "all-regular-files/no-rule-files-v1": return reported == requested
        case "repository-rules-v1":
            if reported == requested { return true }
            let prefix = requested + ":"
            guard reported.hasPrefix(prefix) else { return false }
            let fingerprint = reported.utf8.dropFirst(prefix.utf8.count)
            return fingerprint.count == 64 && fingerprint.allSatisfy {
                (48...57).contains($0) || (97...102).contains($0)
            }
        default: return false
        }
    }

    static func decode(_ data: Data, requestedRoot: String? = nil,
        requestedPolicy: String? = nil) throws -> Self {
        try decodeReport(data, requestedRoot: requestedRoot, requestedPolicy: requestedPolicy, target: .project)
    }

    static func decodeDirectory(_ data: Data, requestedRoot: String, directory: AtlasDirectoryFocus,
        requestedPolicy: String? = nil) throws -> Self {
        guard !directory.isProject, !requestedRoot.isEmpty, requestedRoot.utf8.count <= 16_384,
              !requestedRoot.utf8.contains(0) else { throw DecodeError.invalidResponse }
        return try decodeReport(data, requestedRoot: requestedRoot, requestedPolicy: requestedPolicy, target: .directory(directory))
    }

    static func decodeFile(_ data: Data, requestedRoot: String, file: AtlasFileFocus,
        requestedPolicy: String? = nil) throws -> Self {
        guard !requestedRoot.isEmpty, requestedRoot.utf8.count <= 16_384,
              !requestedRoot.utf8.contains(0) else { throw DecodeError.invalidResponse }
        return try decodeReport(data, requestedRoot: requestedRoot, requestedPolicy: requestedPolicy, target: .file(file))
    }

    private static func decodeReport(_ data: Data, requestedRoot: String?, requestedPolicy: String?,
        target: AtlasCatalogTarget) throws -> Self {
        guard data.count <= maximumResponseBytes else { throw DecodeError.invalidResponse }
        do {
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            switch target {
            case .directory(let directory) where directory.isProject:
                guard wire.schema == "fcb.project-catalog/1", wire.command == "catalog",
                      wire.file == nil else {
                    throw DecodeError.invalidResponse
                }
            case .directory(let directory):
                guard wire.schema == "fcb.project-directory-catalog/1", wire.command == "directory-catalog",
                      wire.file == nil, wire.path_basis == "project-root", let echoed = wire.directory,
                      echoed.encoding == "unix-bytes", !echoed.display.isEmpty, echoed.display.utf8.count <= 16_384,
                      try decodePath(echoed.hex) == directory.rawPath else { throw DecodeError.invalidResponse }
            case .file(let file):
                guard wire.schema == "fcb.project-file-catalog/1", wire.command == "file-catalog",
                      wire.directory == nil, wire.path_basis == "project-root", let echoed = wire.file,
                      echoed.encoding == "unix-bytes", !echoed.display.isEmpty, echoed.display.utf8.count <= 16_384,
                      try decodePath(echoed.hex) == file.rawPath, wire.files.count <= 1 else {
                    throw DecodeError.invalidResponse
                }
            }
            guard wire.status == "ok",
                  wire.identity_scope == "response-local", !wire.native_presented, !wire.source_payload_read,
                  wire.payload_bytes_read == "0", wire.read_calls == "0",
                  wire.world.w == 4096, wire.world.h == 4096,
                  try integer(wire.catalogued_files) == UInt64(wire.files.count),
                  !wire.policy.isEmpty, wire.policy.utf8.count <= 256,
                  requestedPolicy.map({ matchesDiscoveryPolicy(wire.policy, requested: $0) }) != false else {
                throw DecodeError.invalidResponse
            }
            var paths = Set<[UInt8]>(), ids = Set<UInt64>()
            var retainedBytes = 0
            var entries: [Entry] = []
            entries.reserveCapacity(wire.files.count)
            for file in wire.files {
                let id = try integer(file.file_id), bytes = try integer(file.observed_bytes)
                let raw = try decodePath(file.path.hex)
                guard id > 0, ids.insert(id).inserted, paths.insert(raw).inserted,
                      target.containsFile(raw),
                      file.path.encoding == "unix-bytes", file.path.display.utf8.count <= 65_536,
                      [file.x, file.y, file.w, file.h].allSatisfy({ $0.isFinite && $0 >= 0 }),
                      file.x + file.w <= 4096.000001, file.y + file.h <= 4096.000001 else {
                    throw DecodeError.invalidResponse
                }
                // Conservatively charge raw path, decoded path, display and
                // duplicate-detection keys before retaining native entries.
                let charge = 3 * raw.count + file.path.display.utf8.count + 256
                guard charge <= maximumResponseBytes - retainedBytes else { throw DecodeError.limit }
                retainedBytes += charge
                entries.append(Entry(id: id, rawPath: raw, displayPath: file.path.display,
                    sourcePath: String(bytes: raw, encoding: .utf8), observedBytes: bytes,
                    x: file.x, y: file.y, width: file.w, height: file.h))
            }
            return Self(entries: entries, discoveryComplete: wire.discovery_complete,
                policy: wire.policy, requestedRoot: requestedRoot, requestedTarget: target)
        } catch let error as DecodeError { throw error }
        catch { throw DecodeError.invalidResponse }
    }

    private static func integer(_ text: String) throws -> UInt64 {
        guard let n = UInt64(text), String(n) == text else { throw DecodeError.invalidResponse }
        return n
    }
    private static func decodePath(_ text: String) throws -> [UInt8] {
        let hex = Array(text.utf8)
        guard !hex.isEmpty, hex.count <= 32_768, hex.count.isMultiple(of: 2) else { throw DecodeError.invalidResponse }
        func nibble(_ b: UInt8) throws -> UInt8 {
            switch b { case 48...57: return b - 48; case 97...102: return b - 87; default: throw DecodeError.invalidResponse }
        }
        var raw: [UInt8] = []; raw.reserveCapacity(hex.count / 2)
        for i in stride(from: 0, to: hex.count, by: 2) { raw.append(try nibble(hex[i]) * 16 + nibble(hex[i + 1])) }
        guard !raw.contains(0), raw.first != 47,
              !raw.split(separator: 47, omittingEmptySubsequences: false).contains(where: {
                  $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
              }) else { throw DecodeError.invalidResponse }
        return raw
    }
    private struct Wire: Decodable {
        let schema, status, command, identity_scope, payload_bytes_read, read_calls, catalogued_files, policy: String
        let native_presented, source_payload_read, discovery_complete: Bool
        let directory, file: NativePath?
        let path_basis: String?
        let world: World
        let files: [File]
        enum CodingKeys: String, CodingKey {
            case schema, status, command, identity_scope, payload_bytes_read, read_calls, catalogued_files, policy
            case native_presented, source_payload_read, discovery_complete, world, files
            case directory, file, path_basis
        }
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            schema = try c.decode(String.self, forKey: .schema); status = try c.decode(String.self, forKey: .status)
            command = try c.decode(String.self, forKey: .command); identity_scope = try c.decode(String.self, forKey: .identity_scope)
            payload_bytes_read = try c.decode(String.self, forKey: .payload_bytes_read); read_calls = try c.decode(String.self, forKey: .read_calls)
            catalogued_files = try c.decode(String.self, forKey: .catalogued_files); policy = try c.decode(String.self, forKey: .policy)
            native_presented = try c.decode(Bool.self, forKey: .native_presented); source_payload_read = try c.decode(Bool.self, forKey: .source_payload_read)
            discovery_complete = try c.decode(Bool.self, forKey: .discovery_complete); world = try c.decode(World.self, forKey: .world)
            directory = try c.decodeIfPresent(NativePath.self, forKey: .directory)
            file = try c.decodeIfPresent(NativePath.self, forKey: .file)
            path_basis = try c.decodeIfPresent(String.self, forKey: .path_basis)
            var rows = try c.nestedUnkeyedContainer(forKey: .files), files: [File] = []
            if let count = rows.count, count > maximumFiles { throw DecodeError.limit }
            while !rows.isAtEnd {
                guard files.count < maximumFiles else { throw DecodeError.limit }
                files.append(try rows.decode(File.self))
            }
            self.files = files
        }
    }
    private struct World: Decodable { let w, h: Double }
    private struct File: Decodable { let file_id, observed_bytes: String; let path: NativePath; let x, y, w, h: Double }
    private struct NativePath: Decodable { let encoding, hex, display: String }
}

/// One launch's explicit filename selection, not a persisted preference or an
/// exact source anchor. Resolving it does no I/O and uses raw Unix path identity.
/// The established project consumes it once, including failure. An explicitly
/// named file does not depend on discovery limits, ignores or a display filter;
/// the retained reader still admits the actual file under this exact root.
struct AtlasStartupFileRequest {
    enum Outcome: Equatable {
        case none, open(path: String, line: UInt64?), invalid, differentRoot, grantRequired
        var message: String? {
            switch self {
            case .none, .open: return nil
            case .invalid: return "The startup file request is invalid; no file was opened."
            case .differentRoot: return "The startup project changed; choose the file from this project's catalog."
            case .grantRequired: return "This build requires the native folder picker; command-line filenames do not grant access."
            }
        }
    }
    private enum Pending { case none, invalid, file(root: String, path: String, line: UInt64?) }
    private var pending: Pending

    init(arguments: [String]) { pending = Self.parse(arguments) }
    var isPending: Bool { if case .none = pending { return false }; return true }

    mutating func consume(root currentRoot: String?,
        permitsProjectArguments: Bool = true) -> Outcome {
        let request = pending
        pending = .none
        switch request {
        case .none: return .none
        case .invalid: return .invalid
        case .file(let root, let path, let line):
            guard permitsProjectArguments else { return .grantRequired }
            guard currentRoot?.utf8.elementsEqual(root.utf8) == true else { return .differentRoot }
            return .open(path: path, line: line)
        }
    }

    private static func parse(_ arguments: [String]) -> Pending {
        // Never read UserDefaults: a saved filename cannot authorize startup.
        guard arguments.dropFirst().contains(where: { $0 == "-fcb.file-hex" || $0 == "-fcb.line" }) else { return .none }
        guard arguments.count <= 64 else { return .invalid }
        var total = 0
        for argument in arguments.dropFirst() {
            let count = argument.utf8.count
            guard count <= 32_768, count <= 65_536 - total else { return .invalid }
            total += count
        }
        var root: String?, hex: String?, line: UInt64?
        var i = 1
        while i < arguments.count {
            let key = arguments[i]; i += 1
            if key != "-fcb.root" && key != "-fcb.file-hex" && key != "-fcb.line" { continue }
            guard i < arguments.count else { return .invalid }
            let value = arguments[i]; i += 1
            if key == "-fcb.root" {
                guard root == nil else { return .invalid }; root = value
            } else if key == "-fcb.file-hex" {
                guard hex == nil else { return .invalid }; hex = value
            } else {
                guard line == nil, let number = UInt64(value), number > 0,
                      String(number) == value else { return .invalid }
                line = number
            }
        }
        guard let root, root.hasPrefix("/"), root.utf8.count <= 16_384, !root.utf8.contains(0),
              let hex, !hex.isEmpty, hex.utf8.count.isMultiple(of: 2) else { return .invalid }
        func nibble(_ byte: UInt8) -> UInt8? {
            switch byte { case 48...57: return byte - 48; case 97...102: return byte - 87; default: return nil }
        }
        var raw: [UInt8] = []; raw.reserveCapacity(hex.utf8.count / 2)
        var digits = hex.utf8.makeIterator()
        while let high = digits.next() {
            guard let low = digits.next(), let h = nibble(high), let l = nibble(low) else { return .invalid }
            raw.append(h * 16 + l)
        }
        // The CLI selects a file's immediate parent as its project. Accept only
        // one native component, never traversal, absolute paths or lossy UTF-8.
        guard !raw.contains(0), !raw.contains(47), raw != [46], raw != [46, 46],
              let path = String(bytes: raw, encoding: .utf8) else { return .invalid }
        return .file(root: root, path: path, line: line)
    }
}

/// Application-only, main-actor serialization of the process's single startup
/// action. Pure request state above is independently testable without a window.
@MainActor enum AtlasFileStartup {
    private static var pending = AtlasStartupFileRequest(arguments: ProcessInfo.processInfo.arguments)
    static var isPending: Bool { pending.isPending }
    static func consume(root: String) -> AtlasStartupFileRequest.Outcome {
#if FCB_APP_STORE
        return pending.consume(root: root, permitsProjectArguments: false)
#else
        return pending.consume(root: root)
#endif
    }
}
