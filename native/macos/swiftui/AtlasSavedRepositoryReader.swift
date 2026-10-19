import Foundation

enum AtlasSavedReaderOrigin: Sendable {
    case member
    case relationship(generation: UInt64, link: AtlasSavedLink, side: AtlasSavedLinkSide)
}

struct AtlasSavedReaderReceipt: Sendable {
    let readerJSON: String
    let window: AtlasReaderPage

    static func decode(_ json: String?, saved: AtlasSavedIdentity, member: AtlasSavedMember,
        origin: AtlasSavedReaderOrigin, destination: UInt64) throws -> Self {
        let command: String
        switch origin { case .member: command = "open-reader"; case .relationship: command = "relationships-open-reader" }
        let data = try AtlasSavedWire.admit(json, handle: saved.owner, command: command, identity: saved)
        struct Common: Decodable {
            let member, file_id, source_revision: AtlasSavedNumber
            let source_digest, source_observation: String
            let live_source_reopened: Bool
        }
        do {
            let w = try JSONDecoder().decode(Common.self, from: data)
            guard member.captured, destination > 0, destination != saved.owner,
                  w.member.value == member.id, w.file_id.value == member.file, w.source_revision.value == member.file,
                  w.source_digest == member.digest, w.source_observation == "verified-saved-member",
                  !w.live_source_reopened,
                  let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let readerObject = object["reader"] as? [String: Any],
                  let windowObject = object["window"] as? [String: Any] else { throw AtlasSavedError.invalidResponse }
            func nested(_ object: [String: Any]) throws -> String {
                String(decoding: try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]), as: UTF8.self)
            }
            let readerJSON = try nested(readerObject)
            let info = try AtlasReaderInfo.decodeRetainedInitialization(readerJSON, handle: destination, pathBytes: member.path.bytes)
            guard info.identity.capturedBytes == member.observedBytes, info.captureSHA256 == member.digest else {
                throw AtlasSavedError.invalidResponse
            }
            switch origin {
            case .member:
                guard object["selection"] is NSNull,
                      (object["reader_owner"] as? String).flatMap(AtlasReaderWire.integer) == destination else {
                    throw AtlasSavedError.invalidResponse
                }
            case .relationship(let generation, let link, let side):
                guard (object["relationship_generation"] as? String).flatMap(AtlasReaderWire.integer) == generation,
                      (object["link_id"] as? String).flatMap(AtlasReaderWire.integer) == link.id,
                      object["side"] as? String == side.rawValue,
                      object["evidence_precision"] as? String == AtlasSavedWire.evidencePrecision,
                      object["fragment_resolution"] as? String == (link.fragmentUnresolved ? "unresolved" : "none"),
                      object["offset"] as? String == "0",
                      (object["member_bytes_read"] as? String).flatMap(AtlasReaderWire.integer) == member.observedBytes,
                      let callsText = object["member_read_calls"] as? String, let calls = AtlasReaderWire.integer(callsText),
                      (member.observedBytes == 0 && calls == 0) || (member.observedBytes > 0 && calls > 0 && calls <= 1_048_576) else {
                    throw AtlasSavedError.invalidResponse
                }
                if side == .evidence {
                    guard member.id == link.sourceMember, member.path.bytes == link.sourcePath.bytes,
                          member.digest == link.sourceDigest, member.observedBytes == link.evidenceEnd,
                          let selected = object["selection"] as? [String: Any],
                          selected["start"] as? String == "0",
                          (selected["end"] as? String).flatMap(AtlasReaderWire.integer) == member.observedBytes else {
                        throw AtlasSavedError.invalidResponse
                    }
                } else {
                    guard link.canOpenTarget, member.id == link.targetMember,
                          member.path.bytes == link.targetPath?.bytes, object["selection"] is NSNull else {
                        throw AtlasSavedError.invalidResponse
                    }
                }
            }
            let page = try AtlasReaderPage.decode(nested(windowObject), info: info, request: .window(offset: 0, bytes: 16 * 1024))
            return Self(readerJSON: readerJSON, window: page)
        } catch let error as AtlasSavedError { throw error }
        catch { throw AtlasSavedError.invalidResponse }
    }
    func validateInitialPage(_ page: AtlasReaderPage?) throws {
        guard let page, page.identity.matches(window.identity), page.start == window.start else {
            throw AtlasSavedError.invalidResponse
        }
        let bytes = min(page.end, window.end) - page.start
        guard page.originalHex.utf8.prefix(Int(bytes) * 2).elementsEqual(window.originalHex.utf8.prefix(Int(bytes) * 2)) else {
            throw AtlasSavedError.invalidResponse
        }
    }
}

/// The initializer runs on the reader worker. Its bounded receipt crosses back
/// under this lock; no worker mutates observable native state.
final class AtlasSavedReaderReceiptBox: @unchecked Sendable {
    private let lock = NSLock()
    private var value: AtlasSavedReaderReceipt?
    func set(_ receipt: AtlasSavedReaderReceipt) throws {
        lock.lock(); defer { lock.unlock() }
        guard value == nil else { throw AtlasSavedError.invalidResponse }
        value = receipt
    }
    func accepted() throws -> AtlasSavedReaderReceipt {
        lock.lock(); defer { lock.unlock() }
        guard let value else { throw AtlasSavedError.invalidResponse }
        return value
    }
}

@MainActor final class AtlasSavedReaderCapture: Identifiable {
    let id = UUID()
    let reader: AtlasPagedReaderModel
    let owner: UInt64
    let member: AtlasSavedMember
    let origin: AtlasSavedReaderOrigin
    init(reader: AtlasPagedReaderModel, owner: UInt64, member: AtlasSavedMember, origin: AtlasSavedReaderOrigin) {
        self.reader = reader; self.owner = owner; self.member = member; self.origin = origin
    }
    var evidenceLabel: String {
        switch origin {
        case .member: return "Exact saved member · \(member.observedBytes) original bytes"
        case .relationship(_, _, .evidence):
            return "Captured document evidence: original bytes 0..<\(member.observedBytes). Individual link positions are not established."
        case .relationship(_, let link, .target):
            return link.fragmentUnresolved
                ? "Captured target opened at byte 0. Its URL fragment is unresolved."
                : "Captured target opened at byte 0."
        }
    }
}
