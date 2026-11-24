import Foundation
import Dispatch

/// The native atlas registry admits four owners. Failed retirement continues
/// to consume admission, including when an injected create closure has no cap.
/// Reserve before creating a handle, so refusal never strands an unowned one.
private final class AtlasSearchCapturePermit: @unchecked Sendable {
    private static let state = State()
    private var acquired = false
    private final class State: @unchecked Sendable {
        let lock = NSLock()
        var count = 0
    }
    init?() {
        Self.state.lock.lock(); defer { Self.state.lock.unlock() }
        guard Self.state.count < 4 else { return nil }
        Self.state.count += 1
        acquired = true
    }
    deinit {
        guard acquired else { return }
        Self.state.lock.lock(); defer { Self.state.lock.unlock() }
        Self.state.count -= 1
    }
}

/// Preserve the native handle and root grant after a refused close. Explicit
/// opening or retirement triggers one bounded retry pass, never a polling loop.
private final class AtlasSearchCaptureRetirement: @unchecked Sendable {
    static let shared = AtlasSearchCaptureRetirement()
    private let queue = DispatchQueue(label: "dev.frankencode.browser.search-retirement", qos: .utility)
    private let retryLock = NSLock()
    private var retryScheduled = false // At most one queued pass behind an active drain.
    private var quarantine: [Record] = [] // Queue-owned; at most four permits.

    final class Record: @unchecked Sendable {
        let handle: UInt64
        let closeHandle: @Sendable (UInt64) -> Bool
        let failed: @Sendable () -> Void
        let access: AtlasSearchAccessLease?
        let permit: AtlasSearchCapturePermit
        init(handle: UInt64, close: @escaping @Sendable (UInt64) -> Bool,
             failed: @escaping @Sendable () -> Void, access: AtlasSearchAccessLease?,
             permit: AtlasSearchCapturePermit) {
            self.handle = handle; closeHandle = close; self.failed = failed
            self.access = access; self.permit = permit
        }
        func close() -> Bool {
            withExtendedLifetime(access) {
                for attempt in 0..<8 {
                    if closeHandle(handle) { return true }
                    if attempt < 7 { Thread.sleep(forTimeInterval: 0.001) }
                }
                failed()
                return false
            }
        }
    }

    func retire(_ record: Record) {
        queue.async { [self, record] in
            drain()
            if !record.close() { quarantine.append(record) }
        }
    }

    func retry() {
        retryLock.lock()
        guard !retryScheduled else { retryLock.unlock(); return }
        retryScheduled = true
        retryLock.unlock()
        queue.async { [self] in
            // A new opening during this pass can require revisiting a record
            // already refused. Allow one queued follow-up behind this drain.
            retryLock.lock(); retryScheduled = false; retryLock.unlock()
            drain()
        }
    }

    private func drain() {
        let pending = quarantine
        quarantine = []
        for record in pending where !record.close() { quarantine.append(record) }
    }
}

/// One native retained-search handle. All foreign use is worker-only and
/// serialized between incremental steps and reader imports; no UI waits on
/// this lock. Last-owner destruction queues bounded retirement off the UI.
final class AtlasSearchCaptureOwner: @unchecked Sendable {
    private let handle: UInt64
    private let foreign = NSLock()
    private let accessLock = NSLock()
    private var access: AtlasSearchAccessLease?
    private let permit: AtlasSearchCapturePermit
    private let close: @Sendable (UInt64) -> Bool
    private let retirementFailed: @Sendable () -> Void

    init(create: @Sendable () -> UInt64, close: @escaping @Sendable (UInt64) -> Bool,
         retirementFailed: @escaping @Sendable () -> Void) throws {
        AtlasSearchCaptureRetirement.shared.retry()
        guard let permit = AtlasSearchCapturePermit() else { throw AtlasSearchError.unavailable }
        let handle = create()
        guard handle != 0 else { throw AtlasSearchError.unavailable }
        self.handle = handle
        self.permit = permit
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
        AtlasSearchCaptureRetirement.shared.retire(.init(handle: handle, close: close,
            failed: retirementFailed, access: access, permit: permit))
    }
}

struct AtlasSearchCaptureIdentity: Sendable, Equatable {
    let owner, manifest, layout, generation: UInt64
    var catalogReceipt: AtlasSearchCatalogReceipt = .legacyWorkspace
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
                     caseSensitive: Bool = true, mode requestedMode: AtlasSearchMode? = nil,
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
                  witness.hit.structure == hit.structure,
                  AtlasSearchText.sameActualText(witness.hit.actualText, hit.actualText),
                  let path = hit.sourcePath, hit.start < hit.end else { return nil }
            let mode = requestedMode ?? witness.hit.structure?.mode ?? .text
            guard (mode.isTextual ? witness.hit.structure == nil : witness.hit.structure?.mode == mode),
                  mode != .expression || hit.actualText != nil,
                  !mode.isTextual || AtlasSearchText.accepts(actualText: hit.actualText,
                needle: needle, caseSensitive: caseSensitive) else { return nil }
            if mode == .occurrences {
                guard caseSensitive, let structure = witness.hit.structure,
                      structure.name.utf8.elementsEqual(needle.utf8),
                      structure.kind == "identifier-occurrence", structure.language == "text",
                      structure.headingSlug == nil,
                      structure.evidenceStart == hit.start, structure.evidenceEnd == hit.end,
                      (hit.captureByteLength ?? UInt64.max) <= mode.atlasStructureFileByteLimit else { return nil }
            }
            // The exact reader proves the source spelling at the selected
            // original-byte range, even when the workspace query ignored case.
            let exactNeedle = mode.isTextual ? (hit.actualText ?? needle) : needle
            return AtlasSearchCapturedHit(root: root, path: path, needle: exactNeedle,
                start: hit.start, end: hit.end, capturedBytes: hit.captureByteLength ?? 0,
                openReader: { reader in
                    guard reader > 0 else { throw AtlasSearchError.invalidRequest }
                    return try owner.call { handle in
                        let json = try importReader(handle, reader, identity.generation, UInt64(hit.id + 1))
                        return try validatedReader(json, receiver: reader, identity: identity, witness: witness)
                    }
                }, mode: mode, caseSensitive: caseSensitive)
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
            let catalogFields = try AtlasSearchCatalogReceipt.validateFieldPresence(in: data)
            let wire = try JSONDecoder().decode(ImportWire.self, from: data)
            try identity.catalogReceipt.validate(kind: wire.discovery_scope,
                directory: wire.discovery_directory, file: wire.discovery_file,
                pathBasis: wire.path_basis, hasFileEcho: catalogFields.hasFileEcho,
                discoveryCoverage: catalogFields.discoveryCoverage)
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
        let discovery_scope, path_basis: String?
        let discovery_directory, discovery_file: AtlasSearchCatalogReceipt.Path?
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
