import Foundation
import Dispatch

// Fixed responses at the injected C boundary: these exercise the production
// Swift decoder, queue, model and lifecycle, not a substitute Markdown parser.
enum DocumentWireFixture {
    static let path = "/project/README.md"
    static let text = "\u{feff}# Café\n\nA **bold** line.\n\n## Next\n\nTail.\n"
    static let raw = Array(text.utf8)
    static let start = UInt64(raw.firstIndex(of: 65)!)
    static let end = start + UInt64("A **bold** line.\n".utf8.count)
    static func hex(_ bytes: some Sequence<UInt8>) -> String { bytes.map { String(format: "%02x", $0) }.joined() }
    static func range(_ start: UInt64, _ end: UInt64) -> [String: String] { ["start": String(start), "end": String(end)] }
    static func json(_ value: [String: Any]) -> String {
        String(data: try! JSONSerialization.data(withJSONObject: value), encoding: .utf8)!
    }
    static func header(_ command: String, owner: UInt64) -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": command,
         "owner": String(owner), "file_id": "1", "source_revision": "1",
         "captured_bytes": String(raw.count), "encoding": "utf8",
         "additional_source_bytes_read": "0", "native_presented": false,
         "capture_origin": "host-supplied",
         "path": ["encoding": "unix-bytes", "hex": hex(path.utf8), "display": path]]
    }
    static func sourcePage(_ command: String, owner: UInt64) -> [String: Any] {
        var out = header(command, owner: owner)
        out["text_kind"] = "logical-captured-text-not-shaped"
        out["requested_original_range"] = range(0, UInt64(raw.count))
        out["visible_range"] = range(0, UInt64(raw.count))
        out["range_limited"] = false; out["boundaries_adjusted"] = false; out["has_replacements"] = false
        out["first_physical_line"] = NSNull(); out["next_offset"] = NSNull()
        out["text"] = String(decoding: raw.dropFirst(3), as: UTF8.self)
        out["original_hex"] = hex(raw)
        return out
    }
    static func page(_ command: String, owner: UInt64, generation: UInt64, width: UInt64,
        first: UInt64 = 0, headingFirst: UInt64 = 0) -> [String: Any] {
        let total: UInt64 = 70, headingCount: UInt64 = 65
        var out = header(command, owner: owner)
        out["document_generation"] = String(generation); out["document_ready"] = true
        out["layout_complete"] = true; out["native_shaped"] = false
        out["rendering"] = "logical-frankenmarkdown-flow"
        out["source_mapping"] = "enclosing-regions-not-glyph-exact"
        out["width_columns"] = String(width); out["parser_source_base"] = "3"
        out["total_flow_lines"] = String(total); out["total_headings"] = String(headingCount)
        out["rendered_utf8_bytes"] = String(total * 20); out["first_flow_line"] = String(first)
        let last = min(first + 64, total)
        out["flow_lines"] = (first..<last).map { index in
            ["flow_line": String(index), "text": "A bold line.",
             "rendered_utf8_range": range(index * 20, index * 20 + 16),
             "enclosing_original_range": range(start, end)] as [String: Any]
        }
        out["next_flow_line"] = last < total ? String(last) as Any : NSNull()
        out["whole_document_visible"] = first == 0 && last == total
        let headingEnd = min(headingFirst + 64, headingCount)
        out["headings"] = (headingFirst..<headingEnd).map { index in
            ["slug": index == 0 ? "café" : "heading-\(index)", "title": "Café",
             "rendered_utf8_offset": "0", "original_range": range(3, 11)] as [String: Any]
        }
        out["next_heading"] = headingEnd < headingCount ? String(headingEnd) as Any : NSNull()
        return out
    }
    static func selection(generation: UInt64, start: UInt64, end: UInt64) -> [String: Any] {
        ["document_generation": String(generation), "selection_namespace": "document",
         "source_mapping": "enclosing-regions-not-glyph-exact", "rendered_utf8_range": range(start, end)]
    }
}

final class DocumentCallGate: @unchecked Sendable {
    private let lock = NSLock()
    private var entered = false
    private let semaphore = DispatchSemaphore(value: 0)
    var hasEntered: Bool { lock.withLock { entered } }
    func wait() {
        lock.withLock { entered = true }
        precondition(semaphore.wait(timeout: .now() + 10) == .success, "test gate timed out")
    }
    func release() { semaphore.signal() }
}

final class DocumentBoundary: @unchecked Sendable {
    struct Counts { var opens = 0; var closes = 0; var calls = 0; var active = 0; var peak = 0 }
    let owner: UInt64
    private let lock = NSLock()
    private var counts = Counts()
    private var width: UInt64 = 100
    private var gate: (String, DocumentCallGate)?
    private var corrupt = false
    init(owner: UInt64 = 7) { self.owner = owner }
    var snapshot: Counts { lock.withLock { counts } }
    func blockNext(_ command: String) -> DocumentCallGate {
        let next = DocumentCallGate()
        lock.withLock { gate = (command, next) }
        return next
    }
    func corruptNext() { lock.withLock { corrupt = true } }
    private func call(_ command: String, generation: UInt64, first: UInt64 = 0,
        headingFirst: UInt64 = 0, newWidth: UInt64? = nil, slug: String = "café",
        start: UInt64 = 0, end: UInt64 = 0, mode: UInt8 = 0) -> String {
        let (blocked, width, corrupt) = lock.withLock {
            counts.calls += 1; counts.active += 1; counts.peak = max(counts.peak, counts.active)
            if let newWidth { self.width = newWidth }
            let blocked = gate?.0 == command ? gate?.1 : nil
            if blocked != nil { gate = nil }
            let corrupt = self.corrupt; self.corrupt = false
            return (blocked, self.width, corrupt)
        }
        defer { lock.withLock { counts.active -= 1 } }
        blocked?.wait()
        var out: [String: Any]
        if command == "document-source" {
            out = DocumentWireFixture.sourcePage(command, owner: owner)
            var selected = DocumentWireFixture.selection(generation: generation, start: start, end: end)
            selected["original_range"] = DocumentWireFixture.range(DocumentWireFixture.start, DocumentWireFixture.end)
            selected["window_utf8_range"] = DocumentWireFixture.range(DocumentWireFixture.start - 3, DocumentWireFixture.end - 3)
            selected["original_hex"] = DocumentWireFixture.hex(DocumentWireFixture.raw[Int(DocumentWireFixture.start)..<Int(DocumentWireFixture.end)])
            out["selection"] = selected
        } else if command == "document-copy" {
            out = DocumentWireFixture.header(command, owner: owner)
            out.merge(DocumentWireFixture.selection(generation: generation, start: start, end: end)) { _, new in new }
            out["enclosing_original_range"] = DocumentWireFixture.range(DocumentWireFixture.start, DocumentWireFixture.end)
            if mode == 0 { out["copy_domain"] = "rendered-text-utf8"; out["text"] = "A bold line." }
            else {
                out["copy_domain"] = "enclosing-original-markdown"
                out["original_hex"] = DocumentWireFixture.hex(DocumentWireFixture.raw[Int(DocumentWireFixture.start)..<Int(DocumentWireFixture.end)])
            }
        } else {
            out = DocumentWireFixture.page(command, owner: owner, generation: generation,
                width: width, first: first, headingFirst: headingFirst)
            if command == "document-heading" {
                out["heading_slug"] = slug; out["heading_original_range"] = DocumentWireFixture.range(3, 11)
            }
            if command == "document-from-source" { out["original_anchor"] = String(start) }
        }
        if corrupt { out["source_revision"] = "999" }
        return DocumentWireFixture.json(out)
    }
    var reader: AtlasReaderTransport {
        .init(create: { [self] in owner }, open: { [self] handle, path, _ in
            precondition(handle == owner && path == DocumentWireFixture.path)
            lock.withLock { counts.opens += 1 }
            return DocumentWireFixture.json(DocumentWireFixture.header("info", owner: owner))
        }, window: { [self] handle, offset, _ in
            precondition(handle == owner && offset == 0)
            return DocumentWireFixture.json(DocumentWireFixture.sourcePage("window", owner: owner))
        }, lines: { _, _, _, _ in nil }, cancel: { _ in true }, close: { [self] handle in
            precondition(handle == owner)
            lock.withLock { precondition(counts.active == 0); counts.closes += 1 }
            return true
        }, retirementFailed: { preconditionFailure("retirement failed") })
    }
    var document: AtlasReaderDocumentTransport {
        .init(prepare: { [self] handle, generation, width in
            precondition(handle == owner)
            return call("document-prepare", generation: generation, newWidth: width)
        }, window: { [self] _, generation, first, count in
            precondition(count == 64)
            return call("document-window", generation: generation, first: first)
        }, headings: { [self] _, generation, first, count in
            precondition(count == 64)
            return call("document-headings", generation: generation, headingFirst: first)
        }, heading: { [self] _, generation, slug, _ in
            call("document-heading", generation: generation, slug: slug)
        }, fromSource: { [self] _, generation, offset, _ in
            call("document-from-source", generation: generation, start: offset)
        }, source: { [self] _, generation, start, end, context in
            precondition(context == 2048)
            return call("document-source", generation: generation, start: start, end: end)
        }, copy: { [self] _, generation, start, end, mode in
            call("document-copy", generation: generation, start: start, end: end, mode: mode)
        })
    }
}
