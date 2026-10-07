import Foundation

/// Capture capabilities pin the frozen INDEX, not its replaceable query buffer.
/// A refresh creates another owner; old selected sources remain independently
/// importable until their own references retire. No live-source fallback exists.
enum AtlasIndexedSource {
    typealias Import = @Sendable (UInt64, UInt64, UInt64, UInt64, UInt64) throws -> String?
    static func capture(owner: AtlasSearchCaptureOwner, basis: AtlasIndexedBasis, root: String,
                        needle: String, hits: [AtlasIndexedHit], importReader: @escaping Import) -> AtlasSearchCapture {
        AtlasSearchCapture(retainAccess: { owner.retainAccess($0) }, target: { candidate in
            guard hits.indices.contains(candidate.id), hits[candidate.id].matches(candidate),
                  let path = candidate.sourcePath, let bytes = candidate.captureByteLength else { return nil }
            let row = hits[candidate.id]
            return AtlasSearchCapturedHit(root: root, path: path, needle: needle,
                start: candidate.start, end: candidate.end, capturedBytes: bytes, openReader: { reader in
                    guard reader > 0, reader != basis.owner else { throw AtlasSearchError.invalidRequest }
                    return try owner.call { handle in
                        guard handle == basis.owner else { throw AtlasSearchError.invalidResponse }
                        let json = try importReader(handle, reader, basis.index, row.file, row.revision)
                        return try validate(json, reader: reader, basis: basis, row: row)
                    }
                })
        })
    }
    static func validate(_ json: String?, reader: UInt64, basis: AtlasIndexedBasis, row: AtlasIndexedHit) throws -> String {
        guard reader != 0, reader != basis.owner, let json, json.utf8.count <= 4 * 1024 * 1024 else { throw AtlasIndexWire.invalid() }
        let w = try AtlasIndexWire.decode(json)
        try basis.validate(w, command: "index-open-reader")
        let path = try w.object("path"), info = try w.object("reader"), readerPath = try info.object("path")
        try info.header(schema: "fcb.reader-session/1", command: "info", owner: reader)
        guard try w.text("selection_namespace") == "index-source",
              try w.absent(["query_generation", "hit_id", "original_range"]),
              try w.number("file_id") == row.file, try w.number("source_revision") == row.revision,
              try w.number("capture_byte_length") == row.hit.captureByteLength,
              try w.text("capture_sha256") == row.hit.captureSHA256,
              try path.text("encoding") == "unix-bytes", try path.text("hex") == row.pathHex,
              try w.text("source_observation") == "retained-index-capture", try !w.flag("source_reopened"),
              try w.number("reader_owner") == reader,
              try info.number("file_id") > 0, try info.number("source_revision") > 0,
              try info.number("captured_bytes") == row.hit.captureByteLength,
              try readerPath.text("encoding") == "unix-bytes", try readerPath.text("hex") == row.pathHex,
              try info.text("capture_origin") == "host-supplied", try !info.flag("native_presented"),
              try info.number("initial_source_bytes_read") == 0, try info.number("initial_read_calls") == 0,
              try info.number("additional_source_bytes_read") == 0 else { throw AtlasIndexWire.invalid() }
        // Extract, never relabel, the original independent receiver metadata.
        // The ordinary reader decoder validates encoding/path/window semantics.
        guard let object = try JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any],
              let info = object["reader"] as? [String: Any] else { throw AtlasIndexWire.invalid() }
        return String(decoding: try JSONSerialization.data(withJSONObject: info, options: [.sortedKeys]), as: UTF8.self)
    }
}
