import Foundation
import Dispatch

/// One native retained-search handle. All foreign use is worker-only and
/// serialized between incremental steps and reader imports; no UI waits on
/// this lock. Last-owner destruction queues bounded retirement off the UI.
final class AtlasSearchCaptureOwner: @unchecked Sendable {
    private static let retirement = DispatchQueue(label: "dev.frankencode.browser.search-retirement", qos: .utility)
    private let handle: UInt64
    private let foreign = NSLock()
    private let accessLock = NSLock()
    private var access: AtlasSearchAccessLease?
    private let close: @Sendable (UInt64) -> Bool
    private let retirementFailed: @Sendable () -> Void

    init(handle: UInt64, close: @escaping @Sendable (UInt64) -> Bool,
         retirementFailed: @escaping @Sendable () -> Void) throws {
        guard handle != 0 else { throw AtlasSearchError.unavailable }
        self.handle = handle
        self.close = close
        self.retirementFailed = retirementFailed
    }

    func call<T>(_ work: (UInt64) throws -> T) rethrows -> T {
        foreign.lock()
        defer { foreign.unlock() }
        return try work(handle)
    }

    /// Only a small lease reference crosses this lock, never a source operation.
    /// The originating coordinator attaches its root grant before UI delivery.
    func retainAccess(_ lease: AtlasSearchAccessLease) {
        accessLock.lock()
        if access == nil { access = lease }
        accessLock.unlock()
    }

    deinit {
        let handle = handle, close = close, failed = retirementFailed, access = access
        Self.retirement.async {
            withExtendedLifetime(access) {
                // The Rust registry caps retained atlas handles. Retry transient
                // registry contention, not arbitrary source or filesystem work.
                for attempt in 0..<8 {
                    if close(handle) { return }
                    if attempt < 7 { Thread.sleep(forTimeInterval: 0.001) }
                }
                failed()
            }
        }
    }
}

struct AtlasSearchCaptureIdentity: Sendable, Equatable {
    let owner, manifest, layout, generation: UInt64
}
struct AtlasSearchCaptureWitness: Sendable {
    let hit: SearchHit
    let file, revision: UInt64
    let pathHex: String
}

/// Read-only capability backed by one actual retained session and an immutable
/// page's witnesses, not by a digest alone or a process-global path registry.
/// A later query can release its own references without invalidating a reader
/// that already acquired an independent captured copy from Rust.
enum AtlasSearchCaptureFactory {
    typealias Import = @Sendable (UInt64, UInt64, UInt64, UInt64) throws -> String

    static func make(owner: AtlasSearchCaptureOwner, identity: AtlasSearchCaptureIdentity,
                     root: String, needle: String, witnesses: [AtlasSearchCaptureWitness],
                     importReader: @escaping Import) -> AtlasSearchCapture {
        AtlasSearchCapture(retainAccess: { owner.retainAccess($0) }, target: { hit in
            guard hit.id >= 0, hit.id < witnesses.count else { return nil }
            let witness = witnesses[hit.id]
            // Compare raw bytes, not Swift's canonically equivalent strings.
            guard witness.hit.id == hit.id,
                  witness.hit.sourcePath?.utf8.elementsEqual(hit.sourcePath?.utf8 ?? "".utf8) == true,
                  witness.hit.start == hit.start, witness.hit.end == hit.end,
                  witness.hit.captureSHA256 == hit.captureSHA256,
                  witness.hit.captureByteLength == hit.captureByteLength,
                  let path = hit.sourcePath, hit.start < hit.end else { return nil }
            return AtlasSearchCapturedHit(root: root, path: path, needle: needle,
                start: hit.start, end: hit.end, capturedBytes: hit.captureByteLength ?? 0,
                openReader: { reader in
                    guard reader > 0 else { throw AtlasSearchError.invalidRequest }
                    return try owner.call { handle in
                        let json = try importReader(handle, reader, identity.generation, UInt64(hit.id + 1))
                        return try validatedReader(json, receiver: reader, identity: identity, witness: witness)
                    }
                })
        })
    }

    /// Validate the cross-owner handoff before returning the original nested
    /// reader-info object to the existing reader decoder. No path is reopened,
    /// no source text is decoded here, and source IDs are never relabeled.
    static func validatedReader(_ json: String, receiver: UInt64,
                                identity: AtlasSearchCaptureIdentity,
                                witness: AtlasSearchCaptureWitness) throws -> String {
        guard receiver > 0, receiver != identity.owner,
              identity.owner > 0, identity.manifest > 0, identity.layout > 0, identity.generation > 0,
              witness.hit.id >= 0, witness.hit.id < 1000, witness.file > 0, witness.revision > 0,
              let length = witness.hit.captureByteLength, length <= 1024 * 1024,
              witness.hit.start < witness.hit.end, witness.hit.end <= length,
              let digest = witness.hit.captureSHA256, digest.utf8.count == 64,
              digest.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
              json.utf8.count <= 4 * 1024 * 1024, let data = json.data(using: .utf8) else {
            throw AtlasSearchError.invalidResponse
        }
        do {
            let wire = try JSONDecoder().decode(ImportWire.self, from: data)
            guard wire.schema == "fcb.atlas-search/1", wire.status == "ok", wire.command == "open-reader",
                  number(wire.owner) == identity.owner, number(wire.source_manifest) == identity.manifest,
                  number(wire.layout_revision) == identity.layout, number(wire.query_generation) == identity.generation,
                  number(wire.hit_id) == UInt64(witness.hit.id + 1), number(wire.file_id) == witness.file,
                  number(wire.source_revision) == witness.revision,
                  number(wire.original_range.start) == witness.hit.start,
                  number(wire.original_range.end) == witness.hit.end,
                  wire.path.encoding == "unix-bytes", wire.path.hex == witness.pathHex,
                  wire.capture_sha256 == witness.hit.captureSHA256,
                  number(wire.capture_byte_length) == witness.hit.captureByteLength,
                  wire.source_observation == "retained-search-capture", !wire.source_reopened,
                  number(wire.reader_owner) == receiver,
                  wire.reader.schema == "fcb.reader-session/1", wire.reader.status == "ok",
                  wire.reader.command == "info", number(wire.reader.owner) == receiver,
                  number(wire.reader.file_id).map({ $0 > 0 }) == true,
                  number(wire.reader.source_revision).map({ $0 > 0 }) == true,
                  number(wire.reader.captured_bytes) == witness.hit.captureByteLength,
                  wire.reader.path.encoding == "unix-bytes", wire.reader.path.hex == witness.pathHex,
                  wire.reader.capture_origin == "host-supplied",
                  wire.reader.initial_source_bytes_read == "0", wire.reader.initial_read_calls == "0",
                  wire.reader.additional_source_bytes_read == "0", !wire.reader.native_presented else {
                throw AtlasSearchError.invalidResponse
            }
            // Preserve all original reader fields (including optional capability
            // state). Only extract the nested object; do not rewrite path/IDs.
            guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let reader = object["reader"] as? [String: Any] else { throw AtlasSearchError.invalidResponse }
            return String(decoding: try JSONSerialization.data(withJSONObject: reader, options: [.sortedKeys]), as: UTF8.self)
        } catch { throw AtlasSearchError.invalidResponse }
    }

    private static func number(_ text: String) -> UInt64? {
        guard let value = UInt64(text), String(value) == text else { return nil }
        return value
    }
    private struct Path: Decodable { let encoding, hex: String }
    private struct Range: Decodable { let start, end: String }
    private struct ImportWire: Decodable {
        let schema, status, command, owner, source_manifest, layout_revision, query_generation: String
        let hit_id, file_id, source_revision, capture_sha256, capture_byte_length: String
        let original_range: Range
        let path: Path
        let source_observation, reader_owner: String
        let source_reopened: Bool
        let reader: Reader
    }
    private struct Reader: Decodable {
        let schema, status, command, owner, file_id, source_revision, captured_bytes: String
        let path: Path
        let capture_origin, initial_source_bytes_read, initial_read_calls, additional_source_bytes_read: String
        let native_presented: Bool
    }
}
