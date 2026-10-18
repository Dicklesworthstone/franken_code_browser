import Foundation

/// Freeze the complete UTF-8 source already accepted by the atlas/text reader.
/// This value never retains AtlasSource's mutable lazy presentation caches and
/// never reconstructs source from a decoded page, highlighted runs or glyphs.
/// The lexical label supplies provenance only; importing does no source I/O.
struct AtlasSuppliedReaderSource: Sendable {
    let root: String
    let path: String
    let text: String
    let input: AtlasReaderInput

    init(root: String, path: String, text: String) throws {
        input = try AtlasReaderInput(root: root, path: path)
        self.root = root
        self.path = path
        self.text = text
    }

    func matches(root: String, path: String) -> Bool {
        self.root.utf8.elementsEqual(root.utf8) && self.path.utf8.elementsEqual(path.utf8)
    }

    /// Called on the existing bounded reader worker. Count only up to the
    /// admission boundary before allocating a second buffer. String UTF-8 is
    /// lossless for the full UTF-8 captures accepted by the atlas/text routes,
    /// including BOM, embedded NUL, CRLF and decomposed Unicode.
    private func bytes(limit: UInt64) throws -> Data {
        let maximum = Int(min(limit, AtlasReaderLimits.captureBytes))
        guard text.utf8.prefix(maximum + 1).count <= maximum else {
            throw AtlasReaderError.unavailable
        }
        return Data(text.utf8)
    }

    private func verified(_ response: AtlasReaderTransport.SuppliedResponse,
                          handle: UInt64, byteCount: UInt64) throws -> String {
        let info = try AtlasReaderInfo.decode(response.json, handle: handle, path: input.fullPath)
        guard info.origin == "host-supplied", info.identity.encoding == "utf8",
              info.identity.capturedBytes == byteCount,
              AtlasReaderWire.validHex(response.sha256, bytes: 32) else {
            throw AtlasReaderError.invalidResponse
        }
        let data = try AtlasReaderWire.admit(response.json, handle: handle)
        let receipt: Receipt
        do { receipt = try JSONDecoder().decode(Receipt.self, from: data) }
        catch { throw AtlasReaderError.invalidResponse }
        guard receipt.capture_sha256 == response.sha256,
              receipt.initial_source_bytes_read == "0", receipt.initial_read_calls == "0",
              let json = response.json else { throw AtlasReaderError.invalidResponse }
        return json
    }

    private struct Receipt: Decodable {
        let capture_sha256, initial_source_bytes_read, initial_read_calls: String
        enum CodingKeys: CodingKey {
            case capture_sha256, initial_source_bytes_read, initial_read_calls,
                 accepted_query_generation, accepted_outline_generation, accepted_document_generation
        }
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            capture_sha256 = try c.decode(String.self, forKey: .capture_sha256)
            initial_source_bytes_read = try c.decode(String.self, forKey: .initial_source_bytes_read)
            initial_read_calls = try c.decode(String.self, forKey: .initial_read_calls)
            // A supplied source starts a new reader namespace. Missing fields
            // are not an explicit statement that old selection authority is absent.
            for key in [CodingKeys.accepted_query_generation, .accepted_outline_generation,
                        .accepted_document_generation] {
                guard c.contains(key), try c.decodeNil(forKey: key) else {
                    throw AtlasReaderError.invalidResponse
                }
            }
        }
    }

    fileprivate func open(handle: UInt64, label: String, limit: UInt64,
                          using supplied: AtlasReaderTransport) -> String? {
        guard label.utf8.elementsEqual(input.fullPath.utf8), let openBytes = supplied.openBytes else { return nil }
        do {
            let raw = try bytes(limit: limit)
            guard let result = openBytes(handle, label, raw) else { return nil }
            return try verified(result, handle: handle, byteCount: UInt64(raw.count))
        } catch { return nil }
    }
}

extension AtlasReaderTransport {
    /// Only initialization changes. Navigation, search, outline, Markdown and
    /// pinning use this reader's independently owned Rust capture afterward.
    /// A missing capability or rejected receipt cannot invoke live open.
    func importing(_ source: AtlasSuppliedReaderSource) -> Self {
        Self(create: create, open: { handle, path, limit in
            source.open(handle: handle, label: path, limit: limit, using: self)
        }, window: window, lines: lines, cancel: cancel, close: close,
        retirementFailed: retirementFailed, fork: fork, openBytes: openBytes)
    }
}
