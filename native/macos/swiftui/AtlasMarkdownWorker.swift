import Foundation

/// Narrow transport seam for the existing retained-reader ABI. Tests inject
/// transport failures; shipping calls always use AtlasNativeMarkdownBridge.
protocol AtlasMarkdownBridge {
    func create() -> UInt64
    func close(_ handle: UInt64) -> Bool
    func open(_ handle: UInt64, path: String, limit: UInt64) -> String?
    func copy(_ handle: UInt64, count: UInt64) -> String?
    func prepare(_ handle: UInt64, width: UInt64) -> String?
    func window(_ handle: UInt64, first: UInt64) -> String?
    func headings(_ handle: UInt64, first: UInt64) -> String?
}

enum AtlasMarkdownWorker {
    /// Synchronous HOST-WORKER work, invoked through the bounded native I/O
    /// scheduler. All pages share ONE retained reader capture and generation.
    /// A new preview is accepted only after byte-for-byte comparison with the
    /// displayed source. No partial preview is returned under a complete label.
    /// The backend handle closes on this worker on every terminal path, before
    /// releasing the caller's native root-access lease or delivering the packet.
    static func load(_ request: AtlasMarkdownRequest, bridge: any AtlasMarkdownBridge,
                     canceled: () -> Bool = { false }) throws -> AtlasMarkdownDocument {
        try check(canceled)
        let handle = bridge.create()
        guard handle != 0 else { throw AtlasMarkdownError.unavailable }
        let result = Result { try prepare(request, handle: handle, bridge: bridge, canceled: canceled) }
        guard bridge.close(handle) else { throw AtlasMarkdownError.closeFailed }
        try check(canceled)
        return try result.get()
    }

    private static func prepare(_ request: AtlasMarkdownRequest, handle: UInt64,
        bridge: any AtlasMarkdownBridge, canceled: () -> Bool) throws -> AtlasMarkdownDocument {
        var transferred = 0
        func response(_ json: String?) throws -> Data {
            try check(canceled)
            guard let json else { throw AtlasMarkdownError.unavailable }
            let count = json.utf8.count
            guard count <= AtlasMarkdownAssembly.maxResponseBytes,
                  count <= AtlasMarkdownAssembly.maxTransferredBytes - transferred else {
                throw AtlasMarkdownError.invalidResponse
            }
            transferred += count
            let data = Data(json.utf8)
            let header: Header = try decode(data)
            guard header.schema == "fcb.reader-session/1" else { throw AtlasMarkdownError.invalidResponse }
            guard header.status == "ok" else { throw AtlasMarkdownError.unavailable }
            return data
        }
        func envelope(_ data: Data, command: String) throws -> AtlasMarkdownEnvelope {
            let value: AtlasMarkdownEnvelope = try decode(data)
            guard value.command == command, value.owner.value == handle,
                  value.file_id.value > 0, value.source_revision.value > 0,
                  value.additional_source_bytes_read.value == 0, value.encoding == "utf8" else {
                throw AtlasMarkdownError.invalidResponse
            }
            guard value.captured_bytes.value == UInt64(request.source.utf8.count) else {
                throw AtlasMarkdownError.changedSource
            }
            return value
        }
        try check(canceled)
        let opened = try response(bridge.open(handle, path: request.fullPath,
            limit: UInt64(AtlasMarkdownRequest.maxSourceBytes)))
        let capture = try envelope(opened, command: "info")
        try check(canceled)
        let copied = try response(bridge.copy(handle, count: capture.captured_bytes.value))
        guard try envelope(copied, command: "copy-range").sameCapture(as: capture) else {
            throw AtlasMarkdownError.invalidResponse
        }
        let copy: AtlasMarkdownCopy = try decode(copied)
        guard copy.copy_domain == "original-bytes", copy.original_range.lower == 0,
              copy.original_range.upper == capture.captured_bytes.value else {
            throw AtlasMarkdownError.invalidResponse
        }
        try verify(hex: copy.original_hex, source: request.source, canceled: canceled)
        try check(canceled)
        let prepared = try response(bridge.prepare(handle, width: UInt64(request.width)))
        guard try envelope(prepared, command: "document-prepare").sameCapture(as: capture) else {
            throw AtlasMarkdownError.invalidResponse
        }
        let summary: AtlasMarkdownSummary = try decode(prepared)
        var assembly = try AtlasMarkdownAssembly(request: request, capture: capture, summary: summary)
        try assembly.append(decode(prepared) as AtlasMarkdownPage)
        try assembly.append(decode(prepared) as AtlasMarkdownHeadingPage)
        while assembly.rows.count < Int(summary.total_flow_lines.value) {
            try check(canceled)
            let data = try response(bridge.window(handle, first: UInt64(assembly.rows.count)))
            guard try envelope(data, command: "document-window").sameCapture(as: capture),
                  try decode(data) as AtlasMarkdownSummary == summary else {
                throw AtlasMarkdownError.invalidResponse
            }
            try assembly.append(decode(data) as AtlasMarkdownPage)
        }
        while assembly.headings.count < Int(summary.total_headings.value) {
            try check(canceled)
            let data = try response(bridge.headings(handle, first: UInt64(assembly.headings.count)))
            guard try envelope(data, command: "document-headings").sameCapture(as: capture),
                  try decode(data) as AtlasMarkdownSummary == summary else {
                throw AtlasMarkdownError.invalidResponse
            }
            try assembly.append(decode(data) as AtlasMarkdownHeadingPage)
        }
        try check(canceled)
        return try assembly.finish()
    }

    private struct Header: Decodable { let schema, status: String }
    private static func decode<T: Decodable>(_ data: Data) throws -> T {
        do { return try JSONDecoder().decode(T.self, from: data) }
        catch { throw AtlasMarkdownError.invalidResponse }
    }
    private static func check(_ canceled: () -> Bool) throws {
        if canceled() { throw AtlasMarkdownError.canceled }
    }
    private static func verify(hex: String, source: String, canceled: () -> Bool) throws {
        let encoded = Array(hex.utf8)
        guard encoded.count == source.utf8.count * 2 else { throw AtlasMarkdownError.changedSource }
        let alphabet = Array("0123456789abcdef".utf8)
        for (index, byte) in source.utf8.enumerated() {
            if index.isMultiple(of: 4096) { try check(canceled) }
            guard encoded[2 * index] == alphabet[Int(byte >> 4)],
                  encoded[2 * index + 1] == alphabet[Int(byte & 15)] else {
                throw AtlasMarkdownError.changedSource
            }
        }
    }
}
