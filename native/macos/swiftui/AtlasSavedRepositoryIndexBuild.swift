import Foundation

/// Publication limits are separate from source-search coverage. A member that
/// cannot be indexed remains eligible for exact saved-source scanning.
struct AtlasSavedIndexBuildLimits: Sendable, Equatable {
    let fileBytes: UInt64
    let sourceBytes: UInt64
    let grams: UInt64
    static let standard = Self(fileBytes: 256 * 1024, sourceBytes: 64 * 1024 * 1024, grams: 2 * 1024 * 1024)
    var isValid: Bool {
        fileBytes <= 256 * 1024 && sourceBytes <= 64 * 1024 * 1024 && grams <= 2 * 1024 * 1024
    }
}

/// The save panel supplies destination authority separately. Path strings and a
/// saved archive never authorize an automatic retry or an index attachment.
struct AtlasSavedIndexBuildRequest: Sendable {
    let identity: AtlasSavedIdentity
    let operation: UInt64
    let destination: String
    let limits: AtlasSavedIndexBuildLimits

    init(identity: AtlasSavedIdentity, operation: UInt64, destination: String,
        limits: AtlasSavedIndexBuildLimits = .standard) throws {
        guard operation > 0, Self.validDestination(destination), limits.isValid else {
            throw AtlasSavedError.invalidInput
        }
        self.identity = identity; self.operation = operation
        self.destination = destination; self.limits = limits
    }
    static func validDestination(_ path: String) -> Bool {
        let bytes = path.utf8
        guard bytes.first == 47, bytes.count > 1, bytes.count <= 16_384, !bytes.contains(0) else { return false }
        return !bytes.dropFirst().split(separator: 47, omittingEmptySubsequences: false).contains {
            $0.isEmpty || $0.elementsEqual([46]) || $0.elementsEqual([46, 46])
        }
    }
}

enum AtlasSavedIndexBuildEffect: String, Decodable, Sendable, CaseIterable {
    case none
    case incomplete = "destination-created-incomplete"
    case fileSyncUnconfirmed = "complete-file-sync-unconfirmed"
    case fileSyncRequested = "complete-file-sync-requested"
    case fileAndParentSyncRequested = "complete-file-and-parent-sync-requested"
    var completeFile: Bool {
        switch self {
        case .none, .incomplete: return false
        case .fileSyncUnconfirmed, .fileSyncRequested, .fileAndParentSyncRequested: return true
        }
    }
}

struct AtlasSavedIndexBuildCoverage: Sendable {
    let members, indexed, uncovered, unavailable, grams, sourceBytes: UInt64
}

/// A publication receipt remains meaningful after cancellation. Recovering it
/// repeats only the receipt read, never archive loading, construction or writes.
struct AtlasSavedIndexBuildReceipt: Sendable {
    let identity: AtlasSavedIdentity
    let operation: UInt64
    let destination: AtlasSavedPath
    let effect: AtlasSavedIndexBuildEffect
    let succeeded: Bool
    let bytesWritten, indexBytes, manifestBytes: UInt64
    let pin: String?
    let coverage: AtlasSavedIndexBuildCoverage?
    let coverageComplete: Bool
    let memberBytes, memberCalls: UInt64
    let attachEligible: Bool
    let errorCode: String?
    let nextAction: String?

    var message: String {
        if succeeded {
            return "Saved the search index. Attach it when ready to accelerate new exact literal searches."
        }
        let code = errorCode ?? "unknown outcome"
        switch effect {
        case .none:
            return "No new index file was created (\(code)). Choose a new filename for an explicit retry."
        case .incomplete:
            return "An incomplete index remains at the selected destination (\(code)). Inspect it before retrying and choose a new filename."
        case .fileSyncUnconfirmed:
            return "Complete index bytes were written, but file synchronization was not confirmed (\(code)). You can explicitly verify and attach that completed file."
        case .fileSyncRequested:
            return "Complete index bytes were written and file synchronization was requested (\(code)). You can explicitly verify and attach that completed file."
        case .fileAndParentSyncRequested:
            return "The completed index and parent directory were synchronized, but the operation reported \(code). You can explicitly verify and attach the completed file."
        }
    }

    static func decode(_ json: String?, request: AtlasSavedIndexBuildRequest) throws -> Self {
        guard let json, !json.isEmpty, json.utf8.count <= 256 * 1024 else {
            throw AtlasSavedError.unavailable
        }
        do {
            let data = Data(json.utf8)
            let w = try JSONDecoder().decode(Wire.self, from: data)
            let identity = try JSONDecoder().decode(AtlasSavedIdentity.self, from: data)
            guard identity == request.identity, w.schema == "fcb.saved-repository/1",
                  w.command == "index-build", w.status == "ok" || w.status == "error",
                  w.operation.value == request.operation, w.last_index_build_attempt.value == request.operation,
                  w.destination.bytes == Array(request.destination.utf8),
                  w.max_file_bytes.value == request.limits.fileBytes,
                  w.max_source_bytes.value == request.limits.sourceBytes, w.max_grams.value == request.limits.grams,
                  w.max_grams_per_file.value == 65_536, w.max_scratch_bytes.value == 1_048_576,
                  w.max_index_bytes.value == 25_165_824,
                  w.index_kind == "demand-paged-postings-v1",
                  w.index_pin_scope == "page-manifest-envelope-sha256",
                  !w.index_attached, w.accepted_index_unchanged, w.accepted_query_unchanged,
                  w.source_derived_sensitive, !w.encrypted, !w.overwritten, !w.power_loss_qualified,
                  w.index_bytes.value <= 25_165_824, w.index_manifest_bytes.value <= 4 * 1024 * 1024,
                  w.bytes_written.value <= w.index_bytes.value,
                  w.member_bytes_read.value <= request.limits.sourceBytes,
                  w.member_read_calls.value <= identity.captured * 1_048_576 else {
                throw AtlasSavedError.invalidResponse
            }
            let succeeded = w.status == "ok", error = w.error.value
            guard succeeded == (error == nil), !succeeded ||
                    w.effect == .fileSyncRequested || w.effect == .fileAndParentSyncRequested else {
                throw AtlasSavedError.invalidResponse
            }
            if let error {
                guard !error.code.isEmpty, error.code.utf8.count <= 512,
                      error.code.unicodeScalars.allSatisfy({ $0.value >= 32 && $0.value != 127 }),
                      !error.next_action.isEmpty, error.next_action.utf8.count <= 4096,
                      error.next_action.unicodeScalars.allSatisfy({ $0.value >= 32 && $0.value != 127 }) else {
                    throw AtlasSavedError.invalidResponse
                }
            }
            let pin = w.index_pin.value
            if w.index_bytes.value == 0 {
                guard pin == nil, w.index_manifest_bytes.value == 0 else { throw AtlasSavedError.invalidResponse }
            } else {
                guard let pin, AtlasSavedWire.digest(pin), w.index_manifest_bytes.value > 0,
                      w.index_manifest_bytes.value + 32 <= w.index_bytes.value else {
                    throw AtlasSavedError.invalidResponse
                }
            }
            guard w.effect != .none || w.bytes_written.value == 0,
                  w.effect == .none || (w.index_bytes.value > 0 && pin != nil),
                  !w.effect.completeFile || (w.index_bytes.value > 0 && w.bytes_written.value == w.index_bytes.value),
                  w.attach_eligible == (w.effect.completeFile && pin != nil) else {
                throw AtlasSavedError.invalidResponse
            }
            let coverage: AtlasSavedIndexBuildCoverage?
            let values = [w.index_members.value?.value, w.index_indexed_files.value?.value,
                w.index_uncovered_files.value?.value, w.index_unavailable_files.value?.value,
                w.index_unique_grams.value?.value, w.index_build_source_bytes.value?.value]
            if values.allSatisfy({ $0 == nil }) {
                guard !w.coverage_complete, w.index_bytes.value == 0 else { throw AtlasSavedError.invalidResponse }
                coverage = nil
            } else {
                guard let members = values[0], let indexed = values[1], let uncovered = values[2],
                      let unavailable = values[3], let grams = values[4], let sourceBytes = values[5],
                      members == identity.members, indexed <= identity.captured,
                      uncovered == identity.captured - indexed, unavailable == identity.members - identity.captured,
                      grams <= request.limits.grams, sourceBytes <= request.limits.sourceBytes,
                      w.coverage_complete == (identity.discoveryComplete && uncovered == 0 && unavailable == 0) else {
                    throw AtlasSavedError.invalidResponse
                }
                coverage = AtlasSavedIndexBuildCoverage(members: members, indexed: indexed,
                    uncovered: uncovered, unavailable: unavailable, grams: grams, sourceBytes: sourceBytes)
            }
            return Self(identity: identity, operation: w.operation.value, destination: w.destination,
                effect: w.effect, succeeded: succeeded, bytesWritten: w.bytes_written.value,
                indexBytes: w.index_bytes.value, manifestBytes: w.index_manifest_bytes.value, pin: pin,
                coverage: coverage, coverageComplete: w.coverage_complete,
                memberBytes: w.member_bytes_read.value, memberCalls: w.member_read_calls.value,
                attachEligible: w.attach_eligible, errorCode: error?.code, nextAction: error?.next_action)
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }

    private struct Failure: Decodable {
        let code, next_action: String
    }
    private struct Wire: Decodable {
        let schema, status, command: String
        let operation, last_index_build_attempt: AtlasSavedNumber
        let destination: AtlasSavedPath
        let max_file_bytes, max_source_bytes, max_grams: AtlasSavedNumber
        let max_grams_per_file, max_scratch_bytes, max_index_bytes: AtlasSavedNumber
        let effect: AtlasSavedIndexBuildEffect
        let bytes_written, index_bytes, index_manifest_bytes: AtlasSavedNumber
        let index_pin: AtlasSavedSearchOptional<String>
        let index_kind, index_pin_scope: String
        let index_members, index_indexed_files, index_uncovered_files: AtlasSavedSearchOptional<AtlasSavedNumber>
        let index_unavailable_files, index_unique_grams, index_build_source_bytes: AtlasSavedSearchOptional<AtlasSavedNumber>
        let coverage_complete, attach_eligible: Bool
        let member_bytes_read, member_read_calls: AtlasSavedNumber
        let index_attached, accepted_index_unchanged, accepted_query_unchanged: Bool
        let source_derived_sensitive, encrypted, overwritten, power_loss_qualified: Bool
        let error: AtlasSavedSearchOptional<Failure>
    }
}
