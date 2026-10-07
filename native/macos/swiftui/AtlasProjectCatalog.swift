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
    var openableCount: Int { entries.reduce(0) { $0 + ($1.sourcePath == nil ? 0 : 1) } }
    var summary: String {
        let unavailable = entries.count - openableCount
        var text = discoveryComplete ? "\(entries.count) catalogued files." : "Partial catalog: \(entries.count) known files; more may exist."
        if unavailable > 0 { text += " \(unavailable) filenames cannot be opened by this UTF-8 host." }
        return text + " Source previews are separate; files can be opened now."
    }

    static let maximumFiles = 20_000
    static let maximumResponseBytes = 16 * 1024 * 1024

    static func decode(_ data: Data) throws -> Self {
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
            return Self(entries: entries, discoveryComplete: wire.discovery_complete, policy: wire.policy)
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
