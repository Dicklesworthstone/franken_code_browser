import Foundation

// Actual native decoder fixtures, not proof of Rust shaping or native pixels.
@main enum AtlasDocumentDisplayTests {
    static var document: AtlasReaderDocumentInventory {
        let identity = AtlasReaderIdentity(owner: 7, file: 2, revision: 3, capturedBytes: 20,
            pathBytes: Array("/p/README.md".utf8), encoding: "utf8", displayPath: "/p/README.md")
        return .init(identity: identity, generation: 5, width: 80, totalLines: 1,
            totalHeadings: 0, renderedBytes: 5, sourceBase: 0)
    }
    static func rect(_ width: Double = 200) -> [String: Double] {
        ["x": 0, "y": 0, "width": width, "height": 20]
    }
    static func glyph(id: String = "4", font: String = "0") -> [String: Any] {
        ["glyph_id": id, "font_index": font, "x": 0, "y": 0]
    }
    static func item(index: UInt64 = 0, glyphs: [[String: Any]]? = nil) -> [String: Any] {
        ["item_index": String(index), "kind": "text", "bounds": rect(),
         "enclosing_original_range": ["start": "0", "end": "20"],
         "text": "Hello", "color_role": "body", "font_size": 14,
         "glyphs": glyphs ?? [glyph()]]
    }
    static func wire() -> [String: Any] {
        ["schema": "fcb.reader-session/1", "status": "ok", "command": "document-display-prepare",
         "owner": "7", "file_id": "2", "source_revision": "3", "captured_bytes": "20",
         "encoding": "utf8", "additional_source_bytes_read": "0", "native_presented": false,
         "path": ["encoding": "unix-bytes", "hex": "2f702f524541444d452e6d64", "display": "/p/README.md"],
         "document_generation": "5", "display_width_points": "200",
         "rendering": "bundled-frankenmarkdown-display", "native_shaped": false, "glyph_shaped": true,
         "source_mapping": "enclosing-ast-blocks-not-glyph-exact", "source_ranges_qualified": true,
         "total_display_items": "1", "unresolved_images": "0", "display_bounds": rect(),
         "fonts": [["font_index": "0", "byte_length": "4"]],
         "first_display_item": "0", "display_items": [item()], "next_display_item": NSNull()]
    }
    static func json(_ wire: [String: Any]) throws -> String {
        String(decoding: try JSONSerialization.data(withJSONObject: wire), as: UTF8.self)
    }
    static func decode(_ wire: [String: Any]) throws -> AtlasDocumentDisplayPage {
        try AtlasDocumentDisplayWire.page(json(wire), document: document,
            command: "document-display-prepare", first: 0, width: 200)
    }
    static func rejects(_ wire: [String: Any], _ label: String) {
        do { _ = try decode(wire); preconditionFailure("Accepted invalid display: \(label)") }
        catch { }
    }
    static func anchoredWire(_ offset: UInt64) -> [String: Any] {
        var w = wire()
        w["command"] = "document-display-from-source"
        w["original_anchor"] = String(offset)
        return w
    }
    static func decodeAnchor(_ wire: [String: Any], offset: UInt64) throws -> AtlasDocumentDisplayPage {
        try AtlasDocumentDisplayWire.page(json(wire), document: document,
            command: "document-display-from-source", first: nil, width: 200, sourceAnchor: offset)
    }
    static func rejectsAnchor(_ wire: [String: Any], offset: UInt64, _ label: String) {
        do {
            _ = try decodeAnchor(wire, offset: offset)
            preconditionFailure("Accepted invalid source-anchored display: \(label)")
        } catch { }
    }
    static func main() throws {
        let accepted = try decode(wire())
        precondition(accepted.items.count == 1 && accepted.first == 0 && accepted.next == nil)
        precondition(accepted.items[0].original == .init(start: 0, end: 20))
        precondition(accepted.inventory.document.identity.matches(document.identity))
        for (field, value) in [
            ("owner", "8"), ("source_revision", "4"), ("document_generation", "6"),
            ("display_width_points", "201"), ("first_display_item", "1"),
            ("total_display_items", "01"), ("rendering", "logical-frankenmarkdown-flow")
        ] {
            var w = wire(); w[field] = value; rejects(w, field)
        }
        do {
            var w = wire(); w["native_shaped"] = true; rejects(w, "invented native shaping")
            w = wire(); w["additional_source_bytes_read"] = "1"; rejects(w, "live source read")
            w = wire(); w["next_display_item"] = "1"; rejects(w, "false continuation")
            w = wire(); w["source_ranges_qualified"] = false
            rejects(w, "unqualified source link")
            var row = item(); row["enclosing_original_range"] = NSNull(); w["display_items"] = [row]
            let page = try decode(w)
            precondition(page.items[0].original == nil && !page.inventory.qualifiedSources)
        }
        do {
            var w = wire(), row = item()
            row["enclosing_original_range"] = ["start": "0", "end": "21"]
            w["display_items"] = [row]; rejects(w, "source outside capture")
            row = item(); row["bounds"] = rect(-1)
            w["display_items"] = [row]; rejects(w, "negative geometry")
            row["bounds"] = rect(202)
            w["display_items"] = [row]; rejects(w, "geometry outside layout")
        }
        do {
            var w = wire()
            for invalid in [glyph(id: "0"), glyph(id: "65536"), glyph(font: "1"), glyph(id: "04")] {
                w["display_items"] = [item(glyphs: [invalid])]; rejects(w, "invalid glyph/font identity")
            }
            w["display_items"] = [item(glyphs: Array(repeating: glyph(), count: 4097))]
            rejects(w, "oversized single run")
            w["total_display_items"] = "5"
            w["display_items"] = (0..<5).map { item(index: UInt64($0), glyphs: Array(repeating: glyph(), count: 4096)) }
            rejects(w, "aggregate page glyph cap")
            w = wire(); w["fonts"] = [["font_index": "0", "byte_length": "8388609"]]
            rejects(w, "font byte admission")
            w = wire(); w["fonts"] = [["font_index": "1", "byte_length": "4"]]
            rejects(w, "noncontiguous font table")
        }
        do {
            var w = wire(), row = item()
            row["kind"] = "vector"; row.removeValue(forKey: "glyphs")
            row["shape"] = "diagram-arrow"; row["stroke_width"] = 1
            w["display_items"] = [row]
            rejects(w, "unqualified guessed vector geometry")
        }
        do {
            let restored = try decodeAnchor(anchoredWire(7), offset: 7)
            precondition(restored.first == 0 && restored.items[0].original == .init(start: 0, end: 20))
            var eof = anchoredWire(20)
            eof["first_display_item"] = "1"; eof["display_items"] = [[String: Any]]()
            let end = try decodeAnchor(eof, offset: 20)
            precondition(end.first == end.inventory.totalItems && end.items.isEmpty && end.next == nil)

            rejectsAnchor(anchoredWire(8), offset: 7, "different echoed anchor")
            rejectsAnchor(anchoredWire(21), offset: 21, "anchor past retained capture")
            rejectsAnchor(anchoredWire(20), offset: 20, "nonempty page at EOF")
            eof["original_anchor"] = "7"
            rejectsAnchor(eof, offset: 7, "empty end page for an interior source anchor")

            var w = anchoredWire(7), row = item()
            row["enclosing_original_range"] = ["start": "0", "end": "5"]
            w["display_items"] = [row]
            rejectsAnchor(w, offset: 7, "unrelated source block despite correct echoed anchor")
            w["original_anchor"] = "5"
            rejectsAnchor(w, offset: 5, "exclusive source-region end")

            w = anchoredWire(7); row = item()
            row["enclosing_original_range"] = NSNull(); w["display_items"] = [row]
            rejectsAnchor(w, offset: 7, "unmapped first display element")
            w["source_ranges_qualified"] = false
            rejectsAnchor(w, offset: 7, "unqualified source catalog")

            w = anchoredWire(7); w["total_display_items"] = "2"
            var first = item(), second = item(index: 1)
            first["enclosing_original_range"] = ["start": "0", "end": "5"]
            second["enclosing_original_range"] = ["start": "5", "end": "20"]
            w["display_items"] = [first, second]
            rejectsAnchor(w, offset: 7, "anchor only occurs after the first returned element")

            let withBOM = AtlasReaderDocumentInventory(identity: document.identity, generation: document.generation,
                width: document.width, totalLines: document.totalLines, totalHeadings: document.totalHeadings,
                renderedBytes: document.renderedBytes, sourceBase: 3)
            w = anchoredWire(2); row = item()
            row["enclosing_original_range"] = ["start": "3", "end": "20"]
            w["display_items"] = [row]
            do {
                _ = try AtlasDocumentDisplayWire.page(json(w), document: withBOM,
                    command: "document-display-from-source", first: nil, width: 200, sourceAnchor: 2)
                preconditionFailure("Accepted source anchor before the parser's original-byte domain")
            } catch { }
        }
        do {
            var w = wire()
            w["command"] = "document-display-font"; w["font_index"] = "0"; w["font_offset"] = "0"
            w["font_bytes"] = "4"; w["total_font_bytes"] = "4"; w["font_hex"] = "00010203"
            w["next_font_offset"] = NSNull()
            let bytes = try AtlasDocumentDisplayWire.fontChunk(json(w), inventory: accepted.inventory,
                spec: .init(index: 0, bytes: 4), offset: 0)
            precondition(bytes == Data([0, 1, 2, 3]))
            for (field, bad) in [("font_hex", "00010x03"), ("font_offset", "1"),
                                 ("total_font_bytes", "5"), ("document_generation", "6")] {
                var badWire = w; badWire[field] = bad
                do {
                    _ = try AtlasDocumentDisplayWire.fontChunk(json(badWire), inventory: accepted.inventory,
                        spec: .init(index: 0, bytes: 4), offset: 0)
                    preconditionFailure("Accepted invalid font chunk: \(field)")
                } catch { }
            }
        }
        print("Native display wire: capture/generation, geometry, provenance, source anchors, glyph/page budgets and font framing passed")
    }
}
