import Foundation
#if canImport(CryptoKit)
import CryptoKit
#endif

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
    static func path(_ text: String) -> [String: String] {
        ["encoding": "unix-bytes", "hex": text.utf8.map { String(format: "%02x", $0) }.joined(), "display": text]
    }
    static func imagePageWire() -> [String: Any] {
        var w = wire(), row = item()
        row.removeValue(forKey: "glyphs")
        row["kind"] = "image"; row["request_id"] = "44"; row["destination"] = "images/café.png"
        row["alt_text"] = "Actual source image"; row["is_resolved"] = false
        w["display_items"] = [row]; w["fonts"] = [[String: Any]](); w["unresolved_images"] = "1"
        return w
    }
    static func imageAuthorityWire() -> [String: Any] {
        var w = imagePageWire()
        w["command"] = "document-images-authorize"
        w["requested_root"] = path("/p"); w["asset_root"] = path("/canonical/p")
        w["source_relative"] = path("README.md")
        w["capture_sha256"] = String(repeating: "ab", count: 32)
        w["images_authorized"] = true; w["source_authority"] = "verified-current-source"
        w["authorization_reused"] = false
        w["source_verification_bytes_read"] = "20"; w["source_verification_read_calls"] = "1"
        w["additional_source_bytes_read"] = "20"
        w["max_image_bytes"] = "4194304"; w["max_image_cache_bytes"] = "8388608"
        w["max_images"] = "16"; w["max_image_chunk_bytes"] = "65536"
        w["retained_image_bytes"] = "0"; w["retained_images"] = "0"
        return w
    }
    static func authorize(_ w: [String: Any]) throws -> AtlasDocumentImageAuthorization {
        try AtlasDocumentDisplayWire.imageAuthorization(json(w), document: document, root: "/p", sourceRelative: "README.md")
    }
    static func preparedImageWire(total: Int = 3) -> [String: Any] {
        var w = imageAuthorityWire()
        w["command"] = "document-image-prepare"; w["additional_source_bytes_read"] = "0"
        w["item_index"] = "0"; w["request_id"] = "44"; w["destination"] = "images/café.png"
        w["enclosing_original_range"] = ["start": "0", "end": "20"]
        w["image_sha256"] = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        w["image_format"] = "png"; w["format_validation"] = "signature-only"
        w["decoded"] = false; w["image_observation"] = "current-root-capture"
        w["total_image_bytes"] = String(total); w["image_offset"] = "0"
        w["image_bytes"] = String(min(total, 65536))
        // These receipt bytes test framing and digests, not image decoding.
        // The native image lane below uses actual PNG/JPEG encoded fixtures.
        w["image_hex"] = total == 3 ? "616263" : String(repeating: "00", count: min(total, 65536))
        if total > 65536 { w["next_image_offset"] = "65536" } else { w["next_image_offset"] = NSNull() }
        w["image_bytes_read"] = String(total); w["image_read_calls"] = "1"
        w["retained_image_bytes"] = String(total); w["retained_images"] = "1"
        return w
    }
    static func imageReceipts() throws {
        let page = try decode(imagePageWire()), target = page.imageTargets[0]
        precondition(target.itemIndex == 0 && target.requestID == 44 && target.destination == "images/café.png")
        precondition(page.images.isEmpty && page.imageNotice == nil)
        var unqualified = imagePageWire()
        unqualified["source_ranges_qualified"] = false
        var unqualifiedRow = (unqualified["display_items"] as! [[String: Any]])[0]
        unqualifiedRow["enclosing_original_range"] = NSNull(); unqualified["display_items"] = [unqualifiedRow]
        let unmapped = try decode(unqualified)
        precondition(unmapped.imageTargets.isEmpty, "Unqualified source must never request an asset read")
        let authority = try authorize(imageAuthorityWire())
        guard case .current(let requestedRoot, let assetRoot) = authority.authority else {
            preconditionFailure("Current image route acquired a saved archive authority")
        }
        precondition(requestedRoot == Array("/p".utf8) && assetRoot == Array("/canonical/p".utf8))
        for inventory in [page.inventory, unmapped.inventory] {
            let missingEvidence = AtlasDocumentDisplayImageTarget(inventory: inventory, itemIndex: target.itemIndex,
                requestID: target.requestID, destination: target.destination, bounds: target.bounds, original: nil)
            var forged = preparedImageWire(); forged["enclosing_original_range"] = NSNull()
            do {
                _ = try AtlasDocumentDisplayWire.imagePrepared(json(forged), target: missingEvidence, authorization: authority)
                preconditionFailure("Image receipt accepted missing enclosing source evidence")
            } catch AtlasReaderError.invalidResponse { }
        }
        do {
            var reused = imageAuthorityWire()
            reused["authorization_reused"] = true; reused["additional_source_bytes_read"] = "0"
            reused["source_verification_bytes_read"] = "0"; reused["source_verification_read_calls"] = "0"
            _ = try authorize(reused)
            reused["source_verification_read_calls"] = "1"
            do { _ = try authorize(reused); preconditionFailure("Reused image authority claimed new I/O") }
            catch AtlasReaderError.invalidResponse { }
        }
        for (field, value) in [
            ("owner", "8"), ("source_revision", "4"), ("document_generation", "6"),
            ("additional_source_bytes_read", "0"), ("source_verification_bytes_read", "0"),
            ("source_authority", "path-label"), ("capture_sha256", "ab"),
            ("max_image_bytes", "4194305"), ("retained_images", "17")
        ] {
            var w = imageAuthorityWire(); w[field] = value
            do { _ = try authorize(w); preconditionFailure("Image authority accepted \(field)") }
            catch AtlasReaderError.invalidResponse { }
        }
        for (field, value) in [
            ("requested_root", "/elsewhere"), ("source_relative", "other.md"), ("asset_root", "relative/root")
        ] {
            var w = imageAuthorityWire(); w[field] = path(value)
            do { _ = try authorize(w); preconditionFailure("Image authority accepted a different \(field)") }
            catch AtlasReaderError.invalidResponse { }
        }
        let receipt = try AtlasDocumentDisplayWire.imagePrepared(json(preparedImageWire()), target: target, authorization: authority)
        precondition(receipt.firstChunk == Data("abc".utf8) && receipt.totalBytes == 3 && receipt.format == .png)
        #if canImport(CryptoKit)
        try AtlasDocumentDisplayWire.verifyImage(Data("abc".utf8), receipt: receipt)
        do { try AtlasDocumentDisplayWire.verifyImage(Data("abd".utf8), receipt: receipt)
            preconditionFailure("Different bytes accepted under the retained image digest")
        } catch AtlasReaderError.invalidResponse { }
        #endif
        do {
            try AtlasDocumentDisplayWire.verifyImage(Data("ab".utf8), receipt: receipt)
            preconditionFailure("Truncated image assembly accepted")
        } catch AtlasReaderError.invalidResponse { }
        for (field, value) in [
            ("item_index", "1"), ("request_id", "0"), ("display_width_points", "201"),
            ("destination", "images/cafe\u{301}.png"), ("capture_sha256", String(repeating: "cd", count: 32)),
            ("image_hex", "61626g"), ("image_bytes", "2"), ("image_offset", "1"),
            ("image_format", "gif"), ("format_validation", "pixel-decoded"),
            ("image_observation", "historical-capture"), ("additional_source_bytes_read", "1"),
            ("image_sha256", String(repeating: "AB", count: 32)), ("retained_image_bytes", "8388609")
        ] {
            var w = preparedImageWire(); w[field] = value
            do { _ = try AtlasDocumentDisplayWire.imagePrepared(json(w), target: target, authorization: authority)
                preconditionFailure("Image prepare accepted \(field)")
            } catch AtlasReaderError.invalidResponse { }
        }
        do {
            var w = preparedImageWire(); w["enclosing_original_range"] = ["start": "0", "end": "19"]
            do { _ = try AtlasDocumentDisplayWire.imagePrepared(json(w), target: target, authorization: authority)
                preconditionFailure("Image attached to different source evidence")
            } catch AtlasReaderError.invalidResponse { }
            w = preparedImageWire(); w["image_bytes_read"] = "0"; w["image_read_calls"] = "0"
            _ = try AtlasDocumentDisplayWire.imagePrepared(json(w), target: target, authorization: authority)
        }
        let large = try AtlasDocumentDisplayWire.imagePrepared(json(preparedImageWire(total: 65537)),
            target: target, authorization: authority)
        precondition(large.firstChunk.count == 65536)
        var tail = preparedImageWire(total: 65537)
        tail["command"] = "document-image-chunk"; tail["image_offset"] = "65536"; tail["image_bytes"] = "1"
        tail["image_hex"] = "ff"; tail["next_image_offset"] = NSNull()
        tail["image_bytes_read"] = "0"; tail["image_read_calls"] = "0"
        let last = try AtlasDocumentDisplayWire.imageChunk(json(tail), receipt: large, offset: 65536)
        precondition(last == Data([255]))
        for (field, value) in [
            ("image_offset", "65535"), ("next_image_offset", "65537"), ("image_bytes", "2"),
            ("image_sha256", String(repeating: "cd", count: 32)), ("image_bytes_read", "1"),
            ("image_read_calls", "1"), ("request_id", "45"), ("total_image_bytes", "65538")
        ] {
            var w = tail; w[field] = value
            do { _ = try AtlasDocumentDisplayWire.imageChunk(json(w), receipt: large, offset: 65536)
                preconditionFailure("Image chunk accepted \(field)")
            } catch AtlasReaderError.invalidResponse { }
        }
        var swappedRoot = tail; swappedRoot["asset_root"] = path("/different/canonical/root")
        do { _ = try AtlasDocumentDisplayWire.imageChunk(json(swappedRoot), receipt: large, offset: 65536)
            preconditionFailure("Image chunk crossed canonical root authority")
        } catch AtlasReaderError.invalidResponse { }
        for field in ["request_id", "destination"] {
            var w = imagePageWire(), row = (w["display_items"] as! [[String: Any]])[0]
            row.removeValue(forKey: field); w["display_items"] = [row]
            rejects(w, "missing parser image identity \(field)")
        }
        var w = imagePageWire(), row = (w["display_items"] as! [[String: Any]])[0]
        row["request_id"] = "044"; w["display_items"] = [row]; rejects(w, "noncanonical image request ID")
        row["request_id"] = "0"; w["display_items"] = [row]
        let zeroID = try decode(w)
        precondition(zeroID.imageTargets[0].requestID == 0)
    }
    static var savedDocument: AtlasReaderDocumentInventory {
        let reader = document.identity
        let identity = AtlasReaderIdentity(owner: reader.owner, file: reader.file, revision: reader.revision,
            capturedBytes: reader.capturedBytes, pathBytes: Array("README.md".utf8),
            encoding: reader.encoding, displayPath: "README.md")
        return .init(identity: identity, generation: document.generation, width: document.width,
            totalLines: document.totalLines, totalHeadings: document.totalHeadings,
            renderedBytes: document.renderedBytes, sourceBase: document.sourceBase)
    }
    static var savedSource: AtlasSavedDocumentImageSource {
        try! .init(archiveOwner: 40, archiveDigest: String(repeating: "cd", count: 32), sourceMember: 0, members: 3)
    }
    static func savedImageWire(_ wire: [String: Any]) -> [String: Any] {
        var w = wire
        w["path"] = path("README.md"); w["source_relative"] = path("README.md")
        w.removeValue(forKey: "requested_root"); w.removeValue(forKey: "asset_root")
        w["saved_archive_owner"] = "40"; w["saved_archive_digest"] = savedSource.archiveDigest
        w["saved_source_member"] = "0"; w["live_root_authority"] = false
        w["source_authority"] = "verified-saved-member"
        w["source_verification_bytes_read"] = "0"; w["source_verification_read_calls"] = "0"
        w["additional_source_bytes_read"] = "0"
        if w["image_observation"] != nil {
            w["image_observation"] = "verified-saved-member"
            w["image_member"] = "1"; w["image_archive_digest"] = savedSource.archiveDigest
        }
        return w
    }
    static func savedAuthorize(_ w: [String: Any]) throws -> AtlasDocumentImageAuthorization {
        try AtlasDocumentDisplayWire.savedImageAuthorization(json(w), document: savedDocument, source: savedSource)
    }
    static func savedImageReceipts() throws {
        let page = try AtlasDocumentDisplayWire.page(json(savedImageWire(imagePageWire())),
            document: savedDocument, command: "document-display-prepare", first: 0, width: 200)
        let target = page.imageTargets[0]
        let authority = try savedAuthorize(savedImageWire(imageAuthorityWire()))
        guard case .saved(let accepted) = authority.authority else {
            preconditionFailure("Saved images acquired current-root authority")
        }
        precondition(accepted == savedSource && authority.sourceRelative == Array("README.md".utf8))
        let receipt = try AtlasDocumentDisplayWire.imagePrepared(json(savedImageWire(preparedImageWire())),
            target: target, authorization: authority)
        precondition(receipt.firstChunk == Data("abc".utf8) && receipt.savedImageMember == 1)
        for (key, value) in [
            ("saved_archive_owner", "41"), ("saved_archive_digest", String(repeating: "de", count: 32)),
            ("saved_source_member", "1"), ("source_verification_read_calls", "1"),
            ("source_verification_bytes_read", "20"), ("source_authority", "verified-current-source"),
            ("document_generation", "6"), ("owner", "40")
        ] {
            var w = savedImageWire(imageAuthorityWire()); w[key] = value
            do { _ = try savedAuthorize(w); preconditionFailure("Saved authority accepted \(key)") }
            catch AtlasReaderError.invalidResponse { }
        }
        for key in ["requested_root", "asset_root", "source_relative", "live_root_authority"] {
            var w = savedImageWire(imageAuthorityWire())
            if key == "live_root_authority" { w[key] = true }
            else { w[key] = path(key == "source_relative" ? "other.md" : "/live") }
            do { _ = try savedAuthorize(w); preconditionFailure("Saved authority accepted \(key)") }
            catch AtlasReaderError.invalidResponse { }
        }
        for (key, value) in [
            ("image_member", "3"), ("image_archive_digest", String(repeating: "de", count: 32)),
            ("saved_archive_owner", "41"), ("saved_source_member", "1"),
            ("image_observation", "current-root-capture"), ("request_id", "45"),
            ("destination", "images/cafe\u{301}.png"), ("display_width_points", "201"),
            ("capture_sha256", String(repeating: "de", count: 32))
        ] {
            var w = savedImageWire(preparedImageWire()); w[key] = value
            do { _ = try AtlasDocumentDisplayWire.imagePrepared(json(w), target: target, authorization: authority)
                preconditionFailure("Saved image receipt accepted \(key)")
            } catch AtlasReaderError.invalidResponse { }
        }
        let large = try AtlasDocumentDisplayWire.imagePrepared(json(savedImageWire(preparedImageWire(total: 65537))),
            target: target, authorization: authority)
        var tail = savedImageWire(preparedImageWire(total: 65537))
        tail["command"] = "document-image-chunk"; tail["image_offset"] = "65536"
        tail["image_bytes"] = "1"; tail["image_hex"] = "ff"; tail["next_image_offset"] = NSNull()
        tail["image_bytes_read"] = "0"; tail["image_read_calls"] = "0"
        let last = try AtlasDocumentDisplayWire.imageChunk(json(tail), receipt: large, offset: 65536)
        precondition(last == Data([255]))
        // Equal content digests still cannot switch archive member authority.
        for (key, value) in [("image_member", "2"), ("saved_source_member", "1"),
            ("image_archive_digest", String(repeating: "de", count: 32))] {
            var swapped = tail; swapped[key] = value
            do { _ = try AtlasDocumentDisplayWire.imageChunk(json(swapped), receipt: large, offset: 65536)
                preconditionFailure("Saved chunk changed \(key)")
            } catch AtlasReaderError.invalidResponse { }
        }
        let unavailable: [String: Any] = ["schema": "fcb.saved-repository/1", "status": "error",
            "owner": "40", "exit_code": "2", "error": ["code": "DOCUMENT_IMAGE_SAVED_MEMBER_MISSING"]]
        do { _ = try AtlasDocumentDisplayWire.imagePrepared(json(unavailable), target: target, authorization: authority)
            preconditionFailure("Missing captured asset was accepted")
        } catch AtlasReaderError.engine(let code) { precondition(code == "DOCUMENT_IMAGE_SAVED_MEMBER_MISSING") }
        for (key, value) in [("owner", "41"), ("status", "ok"), ("exit_code", "0"), ("schema", "unknown")] {
            var forged = unavailable; forged[key] = value
            do { _ = try AtlasDocumentDisplayWire.imagePrepared(json(forged), target: target, authorization: authority)
                preconditionFailure("Saved error accepted invalid \(key)")
            } catch AtlasReaderError.invalidResponse { }
        }
        let current = try authorize(imageAuthorityWire())
        let currentPage = try decode(imagePageWire())
        do { _ = try AtlasDocumentDisplayWire.imagePrepared(json(unavailable), target: currentPage.imageTargets[0],
            authorization: current)
            preconditionFailure("Saved error schema entered current-root image route")
        } catch AtlasReaderError.invalidResponse { }
    }
    static func main() throws {
        try savedImageReceipts()
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
        try imageReceipts()
        print("Native display wire: capture/generation, geometry, source anchors, glyph/font budgets and exact image receipts passed")
    }
}
