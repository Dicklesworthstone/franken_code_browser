import Foundation

/// Validate the graph owner, selected side and exact capture before admitting
/// a separate reader. Documentation evidence identifies the whole document;
/// destination fragments remain explicitly unqualified and never become jumps.
enum AtlasProjectLinkReaderReceipt {
    static func decode(_ json: String?, index: AtlasPreparedIndex, generation: UInt64,
        link: AtlasProjectLink, side: AtlasProjectLinkSide, destination: UInt64) throws -> AtlasReaderInfo {
        guard destination > 0, destination != index.basis.owner,
              side == .evidence || link.canOpenTarget else { throw AtlasProjectLinksError.invalidInput }
        let file = side == .evidence ? link.source : link.target!
        let w = try AtlasProjectLinksWire.decode(json)
        try AtlasProjectLinksWire.identity(w, basis: index.basis, generation: generation, command: "links-open-reader")
        guard try w.number("link_id") == link.id, try w.text("side") == side.rawValue,
              try w.number("file_id") == file.id, try w.number("source_revision") == file.revision,
              try AtlasProjectLinkPath(w.object("path")).bytes == file.path.bytes,
              try w.text("source_sha256") == file.digest, try w.number("source_bytes") == file.capturedBytes,
              try w.text("source_observation") == "retained-index-capture", try !w.flag("source_reopened"),
              try w.number("additional_source_bytes_read") == 0, try w.number("reader_owner") == destination,
              try w.text("evidence_precision") == "whole-captured-markdown-document",
              try w.text("destination_suffix").utf8.elementsEqual(link.destinationSuffix.utf8),
              try w.text("fragment_resolution") == link.fragmentResolution,
              let json, let data = json.data(using: .utf8),
              let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let nested = object["reader"] as? [String: Any] else { throw AtlasProjectLinksError.invalidResponse }
        let readerJSON = String(decoding: try JSONSerialization.data(withJSONObject: nested, options: [.sortedKeys]), as: UTF8.self)
        let info = try AtlasReaderInfo.decodeRetainedInitialization(readerJSON, handle: destination, pathBytes: file.path.bytes)
        guard info.identity.capturedBytes == file.capturedBytes, info.captureSHA256 == file.digest else {
            throw AtlasProjectLinksError.invalidResponse
        }
        return info
    }
}

@MainActor final class AtlasProjectLinkReaderCapture: Identifiable {
    let id = UUID()
    let reader: AtlasPagedReaderModel
    let owner: UInt64
    let path: AtlasProjectLinkPath
    let link: AtlasProjectLink
    let side: AtlasProjectLinkSide
    init(reader: AtlasPagedReaderModel, owner: UInt64, path: AtlasProjectLinkPath,
         link: AtlasProjectLink, side: AtlasProjectLinkSide) {
        self.reader = reader; self.owner = owner; self.path = path; self.link = link; self.side = side
    }
    var evidenceLabel: String {
        side == .evidence ? link.evidenceLabel
            : "Exact captured target document · destination fragments are unqualified; opened from the beginning"
    }
}
