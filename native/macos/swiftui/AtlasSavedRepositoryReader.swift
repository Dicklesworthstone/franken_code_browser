import Foundation

enum AtlasSavedReaderOrigin: Sendable {
    case member
    case relationship(generation: UInt64, link: AtlasSavedLink, side: AtlasSavedLinkSide)
    case hit(generation: UInt64, needle: String, hit: AtlasSavedHit)
    case expressionHit(generation: UInt64, expression: String, primaryNeedle: String, hit: AtlasSavedHit)
    var searchHit: AtlasSavedHit? {
        switch self {
        case .hit(_, _, let hit), .expressionHit(_, _, _, let hit): return hit
        case .member, .relationship: return nil
        }
    }
}

/// Original-byte evidence for one accepted saved-search occurrence. It does not
/// pretend to be a shaped text selection or a query in the independent reader.
struct AtlasSavedMatchEvidence: Sendable {
    let identity: AtlasReaderIdentity
    let start: UInt64
    let end: UInt64
    let originalHex: String
    var contextStart: UInt64 { start > 132 ? start - 132 : 0 }

    func validate(_ page: AtlasReaderPage) throws {
        guard page.identity.matches(identity), page.start <= start, start < end, end <= page.end,
              AtlasReaderWire.validHex(originalHex, bytes: end - start) else { throw AtlasSavedError.invalidResponse }
        let offset = Int(start - page.start) * 2
        let count = Int(end - start) * 2
        guard page.originalHex.utf8.dropFirst(offset).prefix(count).elementsEqual(originalHex.utf8) else {
            throw AtlasSavedError.invalidResponse
        }
    }

    /// Encoding the submitted literal is not source decoding or a geometry map.
    /// BOMs are never added or stripped from the needle: in-content U+FEFF can
    /// itself be a valid exact match. Swift canonical String equality is avoided.
    static func literalHex(_ needle: String, encoding: String) throws -> String {
        guard AtlasSavedSearchWire.validNeedle(needle) else { throw AtlasSavedError.invalidResponse }
        let bytes: [UInt8]
        switch encoding {
        case "utf8": bytes = Array(needle.utf8)
        case "utf16le":
            bytes = needle.utf16.flatMap { [UInt8(truncatingIfNeeded: $0), UInt8(truncatingIfNeeded: $0 >> 8)] }
        case "utf16be":
            bytes = needle.utf16.flatMap { [UInt8(truncatingIfNeeded: $0 >> 8), UInt8(truncatingIfNeeded: $0)] }
        default: throw AtlasSavedError.invalidResponse
        }
        return bytes.map { String(format: "%02x", $0) }.joined()
    }
}

struct AtlasSavedReaderReceipt: Sendable {
    let readerJSON: String
    let window: AtlasReaderPage
    let match: AtlasSavedMatchEvidence?

    static func decode(_ json: String?, saved: AtlasSavedIdentity, member: AtlasSavedMember,
        origin: AtlasSavedReaderOrigin, destination: UInt64) throws -> Self {
        let command: String
        switch origin {
        case .member, .hit: command = "open-reader"
        case .relationship: command = "relationships-open-reader"
        case .expressionHit: command = "expression-open-reader"
        }
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
            let request: AtlasReaderRequest
            let match: AtlasSavedMatchEvidence?
            let selectionGeneration: String
            if case .expressionHit(let generation, let expression, let primary, _) = origin {
                guard AtlasSavedSearchWire.validNeedle(expression),
                      object["selection_namespace"] as? String == "saved-expression",
                      (object["expression_generation"] as? String).flatMap(AtlasReaderWire.integer) == generation,
                      let acceptedPrimary = object["primary_needle"] as? String,
                      acceptedPrimary.utf8.elementsEqual(primary.utf8) else { throw AtlasSavedError.invalidResponse }
                selectionGeneration = "expression_generation"
            } else { selectionGeneration = "query_generation" }
            switch origin {
            case .member:
                guard object["selection"] is NSNull,
                      (object["reader_owner"] as? String).flatMap(AtlasReaderWire.integer) == destination else {
                    throw AtlasSavedError.invalidResponse
                }
                request = .window(offset: 0, bytes: 16 * 1024); match = nil
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
                request = .window(offset: 0, bytes: 16 * 1024); match = nil
            case .hit(let generation, let needle, let hit), .expressionHit(let generation, _, let needle, let hit):
                guard generation > 0, member.id == hit.member, member.file == hit.file, member.file == hit.revision,
                      member.path.bytes == hit.path.bytes, member.digest == hit.digest,
                      hit.start < hit.end, hit.end <= member.observedBytes, hit.end - hit.start <= 2048,
                      (object["reader_owner"] as? String).flatMap(AtlasReaderWire.integer) == destination,
                      let selected = object["selection"] as? [String: Any],
                      (selected[selectionGeneration] as? String).flatMap(AtlasReaderWire.integer) == generation,
                      (selected["hit_id"] as? String).flatMap(AtlasReaderWire.integer) == hit.id,
                      selected["domain"] as? String == "original-source-bytes",
                      let range = selected["original_range"] as? [String: Any],
                      (range["start"] as? String).flatMap(AtlasReaderWire.integer) == hit.start,
                      (range["end"] as? String).flatMap(AtlasReaderWire.integer) == hit.end,
                      let originalHex = selected["original_hex"] as? String,
                      AtlasReaderWire.validHex(originalHex, bytes: hit.end - hit.start) else {
                    throw AtlasSavedError.invalidResponse
                }
                let literal = try AtlasSavedMatchEvidence.literalHex(needle, encoding: info.identity.encoding)
                guard literal.utf8.elementsEqual(originalHex.utf8),
                      info.identity.encoding == "utf8" || (hit.start % 2 == 0 && hit.end % 2 == 0) else {
                    throw AtlasSavedError.invalidResponse
                }
                let evidence = AtlasSavedMatchEvidence(identity: info.identity, start: hit.start, end: hit.end, originalHex: originalHex)
                let contextEnd = min(member.observedBytes, hit.end + 132)
                request = .window(offset: evidence.contextStart, bytes: max(4, contextEnd - evidence.contextStart))
                match = evidence
            }
            let page = try AtlasReaderPage.decode(nested(windowObject), info: info, request: request)
            try match?.validate(page)
            return Self(readerJSON: readerJSON, window: page, match: match)
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
        try match?.validate(page)
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
    let match: AtlasSavedMatchEvidence?
    init(reader: AtlasPagedReaderModel, owner: UInt64, member: AtlasSavedMember, origin: AtlasSavedReaderOrigin,
        match: AtlasSavedMatchEvidence? = nil) {
        self.reader = reader; self.owner = owner; self.member = member; self.origin = origin; self.match = match
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
        case .hit(let generation, _, let hit):
            return "Saved search \(generation), occurrence \(hit.id) · exact original bytes \(hit.start)..<\(hit.end)"
        case .expressionHit(let generation, _, _, let hit):
            return "Saved expression \(generation), occurrence \(hit.id) · exact original bytes \(hit.start)..<\(hit.end)"
        }
    }
    var canUseMatch: Bool {
        guard let match, reader.canPin, let page = reader.page else { return false }
        return page.identity.matches(match.identity)
    }
    /// Recheck the exact independent reader after any Desk startup wait. The
    /// caller keeps this occurrence's frozen original range, not whichever page
    /// the user happens to be viewing by the time the import begins.
    func withMatchReader<Value: Sendable>(_ work: @escaping @Sendable (UInt64) throws -> Value) async throws -> Value {
        guard canUseMatch else { throw AtlasSavedError.unavailable }
        try Task.checkCancellation()
        return try await reader.withRetainedReader(work)
    }
}
