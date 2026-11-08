import Foundation

enum AtlasSnapshotExportError: Error, Sendable {
    case invalidInput, notStarted, unverifiedOutcome
}

struct AtlasSnapshotExportLimits: Sendable, Equatable {
    let files: UInt64
    let fileBytes: UInt64
    let sourceBytes: UInt64
    let respectIgnores: Bool
    static let standard = Self(files: 20_000, fileBytes: 1024 * 1024,
        sourceBytes: 64 * 1024 * 1024, respectIgnores: true)
    var isValid: Bool {
        (1...65_536).contains(files) && fileBytes <= 1024 * 1024 && sourceBytes <= 64 * 1024 * 1024
    }
}

/// An immutable explicit export request. Native grants are retained separately;
/// these strings never create authority or authorize an automatic retry.
struct AtlasSnapshotExportRequest: Sendable {
    let root: String
    let destination: String
    let limits: AtlasSnapshotExportLimits
    init(root: String, destination: String, limits: AtlasSnapshotExportLimits = .standard) throws {
        guard Self.isAbsolutePath(root), Self.isAbsolutePath(destination), destination != "/",
              !root.utf8.elementsEqual(destination.utf8), limits.isValid else {
            throw AtlasSnapshotExportError.invalidInput
        }
        self.root = root; self.destination = destination; self.limits = limits
    }
    static func isAbsolutePath(_ path: String) -> Bool {
        let bytes = path.utf8
        guard bytes.first == 47, bytes.count <= 16_384, !bytes.contains(0) else { return false }
        if path == "/" { return true }
        return !bytes.dropFirst().split(separator: 47, omittingEmptySubsequences: false).contains {
            $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
        }
    }
}

struct AtlasSnapshotExportTransport: Sendable {
    let save: @Sendable (AtlasSnapshotExportRequest, AtlasSearchCancellation) throws -> String?
}

enum AtlasSnapshotExportEffect: String, Decodable, Sendable {
    case none
    case incomplete = "destination-created-incomplete"
    case written = "complete-file-sync-unconfirmed"
    case synced = "complete-file-sync-requested"
}

struct AtlasSnapshotExportSummary: Sendable {
    let digest: String
    let discoveryComplete: Bool
    let knownFiles, capturedFiles, unavailableFiles, sourceBytes, archiveBytes: UInt64
    var complete: Bool { discoveryComplete && capturedFiles == knownFiles }
}

/// The writer's effect survives error/cancellation. An unverified response is
/// never represented by this type and must never be translated into `none`.
struct AtlasSnapshotExportReceipt: Sendable {
    static let maximumResponseBytes = 8 * 1024 * 1024
    let effect: AtlasSnapshotExportEffect
    let summary: AtlasSnapshotExportSummary?
    let errorCode: String?
    var succeeded: Bool { summary != nil && errorCode == nil && effect == .synced }
    var message: String {
        if let summary, succeeded {
            let result = summary.complete ? "Saved repository" : "Saved a partial repository archive"
            return "\(result): \(summary.capturedFiles) of \(summary.knownFiles) known files captured, \(summary.unavailableFiles) unavailable; \(summary.sourceBytes) source bytes in a \(summary.archiveBytes)-byte archive."
                + (summary.discoveryComplete ? "" : " Discovery was incomplete; additional files may be absent.")
        }
        let code = errorCode ?? "unverified"
        switch effect {
        case .none:
            return "No new archive was created (\(code)). Existing destinations are never overwritten. Choose a new filename for an explicit retry."
        case .incomplete:
            return "The selected destination was created but contains an incomplete archive (\(code)). Inspect it before retrying; choose a new filename."
        case .written:
            return "Complete archive bytes were written, but synchronization was not confirmed (\(code)). Inspect the selected destination before retrying; choose a new filename."
        case .synced:
            return "The writer reported completed archive bytes and synchronization, but the operation returned an error (\(code)). Inspect the selected destination before retrying."
        }
    }

    static func decode(_ json: String?, request: AtlasSnapshotExportRequest) throws -> Self {
        guard let json, !json.isEmpty, json.utf8.count <= maximumResponseBytes else {
            throw AtlasSnapshotExportError.unverifiedOutcome
        }
        do {
            let w = try JSONDecoder().decode(Wire.self, from: Data(json.utf8))
            guard w.schema == "fcb.cli/1" else { throw AtlasSnapshotExportError.unverifiedOutcome }
            if w.status == "error" {
                guard let error = w.error, w.complete == false, error.subsystem == "snapshot", !error.retryable,
                      !error.code.isEmpty, error.code.utf8.count <= 512,
                      error.code.utf8.allSatisfy({ (33...126).contains($0) }),
                      w.command == nil || w.command == "snapshot-save",
                      w.destination == nil, w.snapshot_digest == nil, w.source_scope == nil,
                      w.live_roots_accessed == nil, w.discovery_complete == nil,
                      w.known_files == nil, w.captured_files == nil, w.unavailable_files_count == nil,
                      w.captured_bytes == nil, w.archive_bytes == nil,
                      w.source_contains_plaintext == nil, w.power_loss_qualified == nil,
                      w.rule_files_enabled == nil, w.payload_bytes_read == nil, w.read_calls == nil,
                      w.policy == nil else { throw AtlasSnapshotExportError.unverifiedOutcome }
                return Self(effect: w.effect, summary: nil, errorCode: error.code)
            }
            guard w.status == "ok", w.command == "snapshot-save", w.effect == .synced, w.error == nil,
                  let destination = w.destination, destination.bytes == Array(request.destination.utf8),
                  let digest = w.snapshot_digest, digest.utf8.count == 64,
                  digest.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
                  w.source_scope == "saved-observations-only", w.live_roots_accessed == true,
                  w.source_contains_plaintext == true, w.power_loss_qualified == false,
                  w.rule_files_enabled == request.limits.respectIgnores,
                  let policy = w.policy, !policy.isEmpty, policy.utf8.count <= 4096,
                  let discovery = w.discovery_complete,
                  let known = w.known_files?.value, let captured = w.captured_files?.value,
                  let unavailable = w.unavailable_files_count?.value, let bytes = w.captured_bytes?.value,
                  let archive = w.archive_bytes?.value, let reads = w.payload_bytes_read?.value,
                  w.read_calls != nil,
                  known <= request.limits.files, captured <= known, unavailable == known - captured,
                  bytes <= request.limits.sourceBytes, bytes <= captured * request.limits.fileBytes,
                  reads >= bytes, reads <= request.limits.sourceBytes,
                  archive > 0, archive <= 80 * 1024 * 1024, archive >= bytes else {
                throw AtlasSnapshotExportError.unverifiedOutcome
            }
            let summary = AtlasSnapshotExportSummary(digest: digest, discoveryComplete: discovery,
                knownFiles: known, capturedFiles: captured, unavailableFiles: unavailable,
                sourceBytes: bytes, archiveBytes: archive)
            guard w.complete == nil || w.complete == summary.complete else { throw AtlasSnapshotExportError.unverifiedOutcome }
            return Self(effect: w.effect, summary: summary, errorCode: nil)
        } catch { throw AtlasSnapshotExportError.unverifiedOutcome }
    }

    private struct Number: Decodable {
        let value: UInt64
        init(from decoder: Decoder) throws {
            let text = try decoder.singleValueContainer().decode(String.self)
            guard let value = UInt64(text), String(value) == text else { throw AtlasSnapshotExportError.unverifiedOutcome }
            self.value = value
        }
    }
    private struct Path: Decodable {
        let bytes: [UInt8]
        private enum CodingKeys: CodingKey { case encoding, hex, display }
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            let hex = try c.decode(String.self, forKey: .hex)
            guard try c.decode(String.self, forKey: .encoding) == "unix-bytes",
                  try c.decode(String.self, forKey: .display).utf8.count <= 65_536,
                  !hex.isEmpty, hex.utf8.count <= 32_768, hex.utf8.count.isMultiple(of: 2) else {
                throw AtlasSnapshotExportError.unverifiedOutcome
            }
            let digits = Array(hex.utf8)
            func nibble(_ byte: UInt8) throws -> UInt8 {
                switch byte {
                case 48...57: return byte - 48
                case 97...102: return byte - 87
                default: throw AtlasSnapshotExportError.unverifiedOutcome
                }
            }
            var bytes: [UInt8] = []; bytes.reserveCapacity(digits.count / 2)
            for at in stride(from: 0, to: digits.count, by: 2) {
                bytes.append(try (nibble(digits[at]) << 4) | nibble(digits[at + 1]))
            }
            guard bytes.first == 47, !bytes.contains(0) else { throw AtlasSnapshotExportError.unverifiedOutcome }
            self.bytes = bytes
        }
    }
    private struct Wire: Decodable {
        let schema, status: String
        let command: String?
        let effect: AtlasSnapshotExportEffect
        let complete: Bool?
        let destination: Path?
        let snapshot_digest, source_scope, policy: String?
        let live_roots_accessed, discovery_complete, source_contains_plaintext, power_loss_qualified, rule_files_enabled: Bool?
        let known_files, captured_files, unavailable_files_count, captured_bytes, archive_bytes, payload_bytes_read, read_calls: Number?
        let error: Failure?
    }
    private struct Failure: Decodable {
        let code, subsystem: String
        let retryable: Bool
    }
}
