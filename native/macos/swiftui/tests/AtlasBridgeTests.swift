import Foundation

@_silgen_name("fcb_search_workspace")
func searchWorkspace(_ root: UnsafePointer<CChar>?, _ query: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_source_document")
func sourceDocument(_ path: UnsafePointer<CChar>?) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_text_tile_positions")
func positions(_ heights: UnsafePointer<Double>?, _ count: UInt64, _ columns: UInt64,
               _ width: Double, _ gap: Double) -> UnsafeMutablePointer<CChar>?
@_silgen_name("fcb_free_string")
func freeReply(_ pointer: UnsafeMutablePointer<CChar>?)

@main struct AtlasBridgeTests {
    static func take(_ pointer: UnsafeMutablePointer<CChar>?) -> Data {
        guard let pointer else { preconditionFailure("real bridge response required") }
        defer { freeReply(pointer) }
        return Data(String(cString: pointer).utf8)
    }
    static func main() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        let file = directory.appendingPathComponent("source.rs")
        let text = "// 🦀 unicode\nfn main() { let n = 42; let s = \"hello\"; }\n"
        try text.write(to: file, atomically: true, encoding: .utf8)
        let capture = try JSONDecoder().decode(AtlasHighlightCapture.self, from: take(sourceDocument(file.path)))
        precondition(capture.text == text && capture.validatedRuns() != nil, "real capture and UTF16 spans agree")
        precondition(capture.runs.contains { $0.role == "keyword" }, "upstream lexer supplies keyword color")
        precondition(capture.runs.contains { $0.role == "string" }, "upstream lexer supplies string color")
        precondition(capture.runs.contains { $0.role == "comment" }, "upstream lexer supplies comment color")
        precondition(AtlasDocument(path: "source.rs", capture: capture) != nil, "real capture shapes natively")
        precondition(sourceDocument(directory.appendingPathComponent("missing.rs").path) == nil,
                     "missing file cannot turn into fabricated source")
        let report = try AtlasSearchReport.decode(String(data: take(searchWorkspace(directory.path, "hello")), encoding: .utf8))
        precondition(report.hits.count == 1)
        let hit = report.hits[0]
        let document = AtlasDocument(path: "source.rs", capture: capture)!
        for tile in document.tiles { tile.rect = CGRect(x: 0, y: 0, width: AtlasTextTile.width, height: tile.height) }
        let match = AtlasMatch.resolve(document: document, byteStart: hit.start, byteEnd: hit.end,
            expectedSHA256: hit.captureSHA256!, expectedByteCount: hit.captureByteLength!)!
        precondition(match.sourceRange == (text as NSString).range(of: "hello"), "real bridge offsets map past multibyte UTF8")
        let changedText = text.replacingOccurrences(of: "42", with: "43")
        try changedText.write(to: file, atomically: true, encoding: .utf8)
        let changedCapture = try JSONDecoder().decode(AtlasHighlightCapture.self, from: take(sourceDocument(file.path)))
        let changed = AtlasDocument(path: "source.rs", capture: changedCapture)!
        for tile in changed.tiles { tile.rect = CGRect(x: 0, y: 0, width: AtlasTextTile.width, height: tile.height) }
        precondition(AtlasMatch.resolve(document: changed, byteStart: hit.start, byteEnd: hit.end,
            expectedSHA256: hit.captureSHA256!, expectedByteCount: hit.captureByteLength!) == nil,
            "same-word same-offset same-size changed capture cannot reuse old exact anchor")
        let fresh = try AtlasSearchReport.decode(String(data: take(searchWorkspace(directory.path, "hello")), encoding: .utf8)).hits[0]
        precondition(AtlasMatch.resolve(document: changed, byteStart: fresh.start, byteEnd: fresh.end,
            expectedSHA256: fresh.captureSHA256!, expectedByteCount: fresh.captureByteLength!) != nil,
            "reconciled search succeeds on the changed capture")
        let heights = [100.0, 40.0, 70.0, 20.0]
        let data = heights.withUnsafeBufferPointer { take(positions($0.baseAddress, 4, 2, 648, 6)) }
        let rects = try JSONDecoder().decode([[Double]].self, from: data)
        precondition(rects == [[0, 0, 648, 100], [654, 0, 648, 40],
                              [654, 46, 648, 70], [0, 106, 648, 20]], "actual packing ABI preserves dense geometry")
        let invalid = [Double.nan]
        precondition(invalid.withUnsafeBufferPointer { positions($0.baseAddress, 1, 2, 648, 6) } == nil,
                     "nonfinite geometry refuses at actual ABI")
        print("AtlasBridge: actual file capture, upstream highlighting, native shaping and dense layout ABI passed")
    }
}
