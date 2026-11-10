import Foundation

struct AtlasSnapshotComparisonReaderReceipt: Sendable {
    let readerInfo: AtlasReaderInfo
    let comparisonInfo: AtlasSnapshotComparisonInfo

    static func decode(_ json: String?, identity: AtlasSnapshotComparisonIdentity, generation: UInt64,
        row: AtlasSnapshotComparisonRow, side: AtlasSnapshotComparisonSide, destination: UInt64,
        previousIO: AtlasSnapshotComparisonIOCounts) throws -> Self {
        do {
            guard generation > 0, destination > 0, destination != identity.owner, row.canOpen(side),
                  let member = row.member(side), let bytes = member.bytes, let digest = member.digest else {
                throw AtlasSnapshotComparisonError.invalidInput
            }
            let wire = try AtlasSnapshotComparisonWire.admit(json, owner: identity.owner, command: "open-reader")
            let comparison = try wire.info()
            guard comparison.identity == identity, comparison.acceptedGeneration == generation,
                  comparison.pendingGeneration == nil, comparison.io.follows(previousIO),
                  try wire.number("comparison_generation") == generation, try wire.number("entry_id") == row.id,
                  try wire.text("side") == side.rawValue, try wire.number("member") == member.ordinal,
                  try wire.number("file_id") == member.file, try wire.number("source_revision") == member.revision,
                  try AtlasSnapshotComparisonPath(wire.object("path"), absolute: false).bytes == row.path.bytes,
                  try wire.text("archive_digest") == identity.archive(side).digest,
                  try wire.text("source_digest") == digest, try wire.number("captured_bytes") == bytes,
                  try wire.number("reader_owner") == destination,
                  try wire.text("source_observation") == "verified-saved-member" else {
                throw AtlasSnapshotComparisonError.invalidResponse
            }
            let read = try wire.number("member_bytes_read"), calls = try wire.number("member_read_calls")
            guard read == bytes, calls <= 1_048_576,
                  bytes == 0 ? calls == 0 : calls > 0 else { throw AtlasSnapshotComparisonError.invalidResponse }
            let oldBytes = side == .before ? previousIO.beforeBytes : previousIO.afterBytes
            let oldCalls = side == .before ? previousIO.beforeCalls : previousIO.afterCalls
            let newBytes = side == .before ? comparison.io.beforeBytes : comparison.io.afterBytes
            let newCalls = side == .before ? comparison.io.beforeCalls : comparison.io.afterCalls
            guard newBytes >= oldBytes, newBytes - oldBytes == read,
                  newCalls >= oldCalls, newCalls - oldCalls == calls,
                  side == .before
                    ? comparison.io.afterBytes == previousIO.afterBytes && comparison.io.afterCalls == previousIO.afterCalls
                    : comparison.io.beforeBytes == previousIO.beforeBytes && comparison.io.beforeCalls == previousIO.beforeCalls,
                  let json, let object = try JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any],
                  let nested = object["reader"] as? [String: Any] else {
                throw AtlasSnapshotComparisonError.invalidResponse
            }
            let readerJSON = String(decoding: try JSONSerialization.data(withJSONObject: nested, options: [.sortedKeys]), as: UTF8.self)
            let reader = try AtlasReaderInfo.decodeRetainedInitialization(readerJSON, handle: destination, pathBytes: row.path.bytes)
            guard reader.identity.capturedBytes == bytes, reader.captureSHA256 == digest else {
                throw AtlasSnapshotComparisonError.invalidResponse
            }
            return Self(readerInfo: reader, comparisonInfo: comparison)
        } catch let error as AtlasSnapshotComparisonError { throw error }
        catch { throw AtlasSnapshotComparisonError.invalidResponse }
    }
}

/// One bounded handoff from the reader worker back to its MainActor owner.
/// It carries validated values only; neither archive descriptor escapes here.
final class AtlasSnapshotComparisonReaderDelivery: @unchecked Sendable {
    private let lock = NSLock()
    private var receipt: AtlasSnapshotComparisonReaderReceipt?
    func publish(_ value: AtlasSnapshotComparisonReaderReceipt) throws {
        lock.lock(); defer { lock.unlock() }
        guard receipt == nil else { throw AtlasSnapshotComparisonError.invalidResponse }
        receipt = value
    }
    func take() throws -> AtlasSnapshotComparisonReaderReceipt {
        lock.lock(); defer { lock.unlock() }
        guard let receipt else { throw AtlasSnapshotComparisonError.invalidResponse }
        self.receipt = nil
        return receipt
    }
}

@MainActor final class AtlasSnapshotComparisonReaderCapture: Identifiable {
    let id = UUID()
    let reader: AtlasPagedReaderModel
    let owner, generation: UInt64
    let row: AtlasSnapshotComparisonRow
    let side: AtlasSnapshotComparisonSide
    let archiveDigest: String
    var path: AtlasSnapshotComparisonPath { row.path }
    var evidenceLabel: String {
        "\(side.label) · verified saved member from archive \(archiveDigest.prefix(12)); exact original bytes, no live source reopened"
    }
    init(reader: AtlasPagedReaderModel, identity: AtlasSnapshotComparisonIdentity, generation: UInt64,
        row: AtlasSnapshotComparisonRow, side: AtlasSnapshotComparisonSide) {
        self.reader = reader; owner = identity.owner; self.generation = generation
        self.row = row; self.side = side; archiveDigest = identity.archive(side).digest
    }
}
