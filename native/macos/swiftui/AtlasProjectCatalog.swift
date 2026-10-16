import Foundation

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
    init(entries: [Entry], discoveryComplete: Bool, policy: String, requestedRoot: String? = nil) {
        self.entries = entries; self.discoveryComplete = discoveryComplete
        self.policy = policy; self.requestedRoot = requestedRoot
    }
    var openableCount: Int { entries.reduce(0) { $0 + ($1.sourcePath == nil ? 0 : 1) } }
    var summary: String {
        let unavailable = entries.count - openableCount
        var text = discoveryComplete ? "\(entries.count) catalogued files." : "Partial catalog: \(entries.count) known files; more may exist."
        if unavailable > 0 { text += " \(unavailable) filenames cannot be opened by this UTF-8 host." }
        return text + " Source previews are separate; files can be opened now."
    }

    static let maximumFiles = 20_000
    static let maximumResponseBytes = 16 * 1024 * 1024

    static func decode(_ data: Data, requestedRoot: String? = nil) throws -> Self {
        guard data.count <= maximumResponseBytes else { throw DecodeError.invalidResponse }
        do {
            let wire = try JSONDecoder().decode(Wire.self, from: data)
            guard wire.schema == "fcb.project-catalog/1", wire.status == "ok", wire.command == "catalog",
                  wire.identity_scope == "response-local", !wire.native_presented, !wire.source_payload_read,
                  wire.payload_bytes_read == "0", wire.read_calls == "0",
                  wire.world.w == 4096, wire.world.h == 4096,
                  try integer(wire.catalogued_files) == UInt64(wire.files.count),
                  !wire.policy.isEmpty, wire.policy.utf8.count <= 256 else { throw DecodeError.invalidResponse }
            var paths = Set<[UInt8]>(), ids = Set<UInt64>()
            var retainedBytes = 0
            var entries: [Entry] = []
            entries.reserveCapacity(wire.files.count)
            for file in wire.files {
                let id = try integer(file.file_id), bytes = try integer(file.observed_bytes)
                let raw = try decodePath(file.path.hex)
                guard id > 0, ids.insert(id).inserted, paths.insert(raw).inserted,
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
                policy: wire.policy, requestedRoot: requestedRoot)
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
        let world: World
        let files: [File]
        enum CodingKeys: String, CodingKey {
            case schema, status, command, identity_scope, payload_bytes_read, read_calls, catalogued_files, policy
            case native_presented, source_payload_read, discovery_complete, world, files
        }
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            schema = try c.decode(String.self, forKey: .schema); status = try c.decode(String.self, forKey: .status)
            command = try c.decode(String.self, forKey: .command); identity_scope = try c.decode(String.self, forKey: .identity_scope)
            payload_bytes_read = try c.decode(String.self, forKey: .payload_bytes_read); read_calls = try c.decode(String.self, forKey: .read_calls)
            catalogued_files = try c.decode(String.self, forKey: .catalogued_files); policy = try c.decode(String.self, forKey: .policy)
            native_presented = try c.decode(Bool.self, forKey: .native_presented); source_payload_read = try c.decode(Bool.self, forKey: .source_payload_read)
            discovery_complete = try c.decode(Bool.self, forKey: .discovery_complete); world = try c.decode(World.self, forKey: .world)
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
/// The first catalog presentation consumes it, including failure, so refresh,
/// another window or a project replacement cannot replay an old startup action.
struct AtlasStartupFileRequest {
    enum Outcome: Equatable {
        case none, open(UInt64), invalid, differentRoot, grantRequired, filtered, unavailable
        case notCatalogued(partial: Bool)
        var message: String? {
            switch self {
            case .none, .open: return nil
            case .invalid: return "The startup file request is invalid; no file was opened."
            case .differentRoot: return "The startup project changed; choose the file from this project's catalog."
            case .grantRequired: return "This build requires the native folder picker; command-line filenames do not grant access."
            case .filtered: return "The startup file is hidden by the current filter. Change the filter and select it explicitly."
            case .unavailable: return "The startup filename cannot be opened by this UTF-8 reader."
            case .notCatalogued(let partial):
                return partial ? "The startup file was not in the partial catalog. It may be unseen or excluded; refresh or choose a narrower project."
                    : "The startup file was not in the admitted catalog. It may have changed or been excluded by discovery policy."
            }
        }
    }
    private enum Pending { case none, invalid, file(root: String, bytes: [UInt8]) }
    private var pending: Pending

    init(arguments: [String]) { pending = Self.parse(arguments) }

    mutating func consume(catalog: AtlasProjectCatalog, visibleEntries: [AtlasProjectCatalog.Entry],
        permitsProjectArguments: Bool = true) -> Outcome {
        let request = pending
        pending = .none
        switch request {
        case .none: return .none
        case .invalid: return .invalid
        case .file(let root, let bytes):
            guard permitsProjectArguments else { return .grantRequired }
            guard catalog.requestedRoot?.utf8.elementsEqual(root.utf8) == true else { return .differentRoot }
            guard let entry = catalog.entries.first(where: { $0.rawPath == bytes }) else {
                return .notCatalogued(partial: !catalog.discoveryComplete)
            }
            guard entry.sourcePath?.utf8.elementsEqual(bytes) == true else { return .unavailable }
            guard visibleEntries.contains(where: { $0.id == entry.id && $0.rawPath == bytes }) else { return .filtered }
            return .open(entry.id)
        }
    }

    private static func parse(_ arguments: [String]) -> Pending {
        // Never read UserDefaults: a saved filename cannot authorize startup.
        guard arguments.dropFirst().contains("-fcb.file-hex") else { return .none }
        guard arguments.count <= 64 else { return .invalid }
        var total = 0
        for argument in arguments.dropFirst() {
            let count = argument.utf8.count
            guard count <= 32_768, count <= 65_536 - total else { return .invalid }
            total += count
        }
        var root: String?, hex: String?
        var i = 1
        while i < arguments.count {
            let key = arguments[i]; i += 1
            if key != "-fcb.root" && key != "-fcb.file-hex" { continue }
            guard i < arguments.count else { return .invalid }
            let value = arguments[i]; i += 1
            if key == "-fcb.root" {
                guard root == nil else { return .invalid }; root = value
            } else {
                guard hex == nil else { return .invalid }; hex = value
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
              String(bytes: raw, encoding: .utf8) != nil else { return .invalid }
        return .file(root: root, bytes: raw)
    }
}

/// Application-only, main-actor serialization of the process's single startup
/// action. Pure request state above is independently testable without a window.
@MainActor enum AtlasFileStartup {
    private static var pending = AtlasStartupFileRequest(arguments: ProcessInfo.processInfo.arguments)
    static func consume(catalog: AtlasProjectCatalog, visibleEntries: [AtlasProjectCatalog.Entry]) -> AtlasStartupFileRequest.Outcome {
#if FCB_APP_STORE
        return pending.consume(catalog: catalog, visibleEntries: visibleEntries, permitsProjectArguments: false)
#else
        return pending.consume(catalog: catalog, visibleEntries: visibleEntries)
#endif
    }
}
