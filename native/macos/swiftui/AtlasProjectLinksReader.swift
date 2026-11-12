import Foundation

/// Validate the graph owner, selected side and exact capture before admitting
/// a separate reader. Documentation evidence identifies the whole document;
/// qualified target fragments or exact lexical import evidence supply offsets.
struct AtlasProjectLinkReaderReceipt: Sendable {
    let info: AtlasReaderInfo
    let fragment: AtlasProjectLinkFragment?
    let initialOriginalOffset: UInt64
    let navigationDescription: String

    static func decode(_ json: String?, index: AtlasPreparedIndex, generation: UInt64,
        link: AtlasProjectLink, side: AtlasProjectLinkSide, destination: UInt64) throws -> Self {
        guard destination > 0, destination != index.basis.owner,
              side == .evidence || link.canOpenTarget else { throw AtlasProjectLinksError.invalidInput }
        let file = side == .evidence ? link.source : link.target!
        let projection = link.projection
        let w = try AtlasProjectLinksWire.decode(json, projection: projection)
        try AtlasProjectLinksWire.identity(w, basis: index.basis, generation: generation,
            command: projection.command("open-reader"), projection: projection)
        guard try w.number("link_id") == link.id, try w.text("side") == side.rawValue,
              try w.number("file_id") == file.id, try w.number("source_revision") == file.revision,
              try AtlasProjectLinkPath(w.object("path")).bytes == file.path.bytes,
              try w.text("source_sha256") == file.digest, try w.number("source_bytes") == file.capturedBytes,
              try w.text("source_observation") == "retained-index-capture", try !w.flag("source_reopened"),
              try w.number("additional_source_bytes_read") == 0, try w.number("reader_owner") == destination,
              let json, let data = json.data(using: .utf8),
              let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let nested = object["reader"] as? [String: Any] else { throw AtlasProjectLinksError.invalidResponse }
        let readerJSON = String(decoding: try JSONSerialization.data(withJSONObject: nested, options: [.sortedKeys]), as: UTF8.self)
        let info = try AtlasReaderInfo.decodeRetainedInitialization(readerJSON, handle: destination, pathBytes: file.path.bytes)
        guard info.identity.capturedBytes == file.capturedBytes, info.captureSHA256 == file.digest else {
            throw AtlasProjectLinksError.invalidResponse
        }
        if projection == .documentation {
            guard try w.text("evidence_precision") == "whole-captured-markdown-document",
                  try w.text("destination_suffix").utf8.elementsEqual(link.destinationSuffix.utf8),
                  try w.text("fragment_resolution") == link.fragmentResolution else {
                throw AtlasProjectLinksError.invalidResponse
            }
            let fragment = try AtlasProjectLinkFragment.decode(w, link: link, side: side, info: info)
            return Self(info: info, fragment: fragment, initialOriginalOffset: fragment.initialOriginalOffset,
                navigationDescription: fragment.navigationDescription)
        }
        try AtlasProjectImportEvidence.validateRoot(w)
        let repeated = try AtlasProjectLink(w.object("relationship"), projection: .imports)
        let evidence = try AtlasProjectLinkRange(w.object("evidence_range"))
        let offset = try w.number("initial_original_offset")
        guard repeated == link, evidence == link.evidenceRange,
              offset == (side == .evidence ? evidence.start : 0),
              try w.absent(["fragment_status", "fragment_suffix", "fragment_slug", "target_range",
                            "destination_suffix", "fragment_resolution"]) else { throw AtlasProjectLinksError.invalidResponse }
        return Self(info: info, fragment: nil, initialOriginalOffset: offset,
            navigationDescription: side == .evidence
                ? "Opened the exact import evidence at original bytes \(evidence.start)..<\(evidence.end) in this capture."
                : "Opened the retained catalog target candidate at the beginning. Compiler resolution is not established.")
    }
}

/// Captured heading and source-line outcomes from the shared service. This
/// validates a receipt; it does not decode URI identities or derive byte spans.
struct AtlasProjectLinkFragment: Sendable {
    let status: String
    let slug: String?
    let initialOriginalOffset: UInt64

    var navigationDescription: String {
        switch status {
        case "none": "Opened at the beginning of the captured file."
        case "document-start": "The empty fragment selects the captured document's beginning."
        case "resolved": "Opened at heading #\(slug ?? "") in this capture."
        case "resolved-lines": "Opened at \(AtlasSourceLineFragment(slug)?.label ?? "the requested source lines") in this capture."
        case "missing-line": "The requested source line range is absent from this capture. Opened at the beginning."
        case "missing-heading": "The requested heading is absent from this capture. Opened at the beginning."
        case "ambiguous": "Multiple captured headings share this identity. Opened at the beginning."
        case "limited": "Fragment navigation reached a source, document or memory limit. Opened at the beginning."
        case "unsupported": "The fragment's source position could not be verified in this capture. Opened at the beginning."
        case "invalid": "The link fragment is invalid. Opened at the beginning."
        default: "The destination fragment is unqualified. Opened at the beginning."
        }
    }

    static func decode(_ w: AtlasIndexWire, link: AtlasProjectLink, side: AtlasProjectLinkSide,
        info: AtlasReaderInfo) throws -> Self {
        let fields = ["fragment_status", "fragment_suffix", "fragment_slug", "target_range", "initial_original_offset"]
        let present = fields.filter { w.fields.contains(AtlasIndexWire.Key($0)) }
        let expected: String
        if side == .target, let start = link.destinationSuffix.firstIndex(of: "#") {
            expected = String(link.destinationSuffix[start...])
        } else { expected = "" }
        // An older engine's inert receipt may open the file, never invent a
        // heading. Partial new receipts cannot acquire that compatibility path.
        if present.isEmpty {
            return Self(status: expected.isEmpty ? "none" : "unqualified", slug: nil, initialOriginalOffset: 0)
        }
        guard present.count == fields.count,
              try w.text("fragment_suffix").utf8.elementsEqual(expected.utf8) else {
            throw AtlasProjectLinksError.invalidResponse
        }
        let status = try w.text("fragment_status"), slug = try w.optionalText("fragment_slug")
        let offset = try w.number("initial_original_offset")
        if let slug {
            guard !slug.isEmpty, slug.utf8.count <= 4096,
                  slug.unicodeScalars.allSatisfy({ $0.value >= 32 && !(127...159).contains($0.value) }) else {
                throw AtlasProjectLinksError.invalidResponse
            }
        }
        switch status {
        case "none":
            guard expected.isEmpty, slug == nil else { throw AtlasProjectLinksError.invalidResponse }
        case "document-start":
            guard expected == "#", slug == nil else { throw AtlasProjectLinksError.invalidResponse }
        case "resolved", "missing-heading", "ambiguous", "limited", "unsupported":
            guard expected.hasPrefix("#"), expected.utf8.count > 1, slug != nil else {
                throw AtlasProjectLinksError.invalidResponse
            }
        case "resolved-lines", "missing-line":
            guard side == .target, expected.hasPrefix("#"), expected.utf8.count > 1,
                  AtlasSourceLineFragment(slug) != nil,
                  AtlasSourceLineFragment.admitsIdentity(info.identity) else {
                throw AtlasProjectLinksError.invalidResponse
            }
        case "invalid":
            guard expected.hasPrefix("#"), expected.utf8.count > 1, slug == nil else {
                throw AtlasProjectLinksError.invalidResponse
            }
        default: throw AtlasProjectLinksError.invalidResponse
        }
        if status == "resolved" {
            let range = try w.object("target_range"), start = try range.number("start"), end = try range.number("end")
            guard side == .target, info.identity.encoding == "utf8", info.identity.capturedBytes <= 262_144,
                  start < end, end <= info.identity.capturedBytes, offset == start else {
                throw AtlasProjectLinksError.invalidResponse
            }
        } else if status == "resolved-lines" {
            let range = try w.object("target_range"), start = try range.number("start"), end = try range.number("end")
            guard AtlasSourceLineFragment.admits(.init(start: start, end: end), in: info.identity), offset == start else {
                throw AtlasProjectLinksError.invalidResponse
            }
        } else {
            guard offset == 0, try w.absent(["target_range"]) else { throw AtlasProjectLinksError.invalidResponse }
        }
        return Self(status: status, slug: slug, initialOriginalOffset: offset)
    }
}

/// One worker-to-main receipt, bound to a single reader-open attempt. The lock
/// never covers foreign work and the value is immutable after publication.
final class AtlasProjectLinkReaderReceiptBox: @unchecked Sendable {
    private let lock = NSLock()
    private var value: AtlasProjectLinkReaderReceipt?
    func set(_ receipt: AtlasProjectLinkReaderReceipt) throws {
        lock.lock(); defer { lock.unlock() }
        guard value == nil else { throw AtlasProjectLinksError.invalidResponse }
        value = receipt
    }
    func accepted() throws -> AtlasProjectLinkReaderReceipt {
        lock.lock(); defer { lock.unlock() }
        guard let value else { throw AtlasProjectLinksError.invalidResponse }
        return value
    }
}

@MainActor final class AtlasProjectLinkReaderCapture: Identifiable {
    let id = UUID()
    let reader: AtlasPagedReaderModel
    let owner: UInt64
    let path: AtlasProjectLinkPath
    let link: AtlasProjectLink
    let side: AtlasProjectLinkSide
    let fragment: AtlasProjectLinkFragment?
    let navigationDescription: String
    init(reader: AtlasPagedReaderModel, owner: UInt64, path: AtlasProjectLinkPath,
         link: AtlasProjectLink, side: AtlasProjectLinkSide, fragment: AtlasProjectLinkFragment?, navigationDescription: String) {
        self.reader = reader; self.owner = owner; self.path = path; self.link = link; self.side = side; self.fragment = fragment
        self.navigationDescription = navigationDescription
    }
    var evidenceLabel: String {
        side == .evidence ? link.evidenceLabel
            : (link.projection == .imports ? "Captured catalog target candidate · " : "Exact captured target document · ") + navigationDescription
    }
}
