import Foundation

/// Transport fixtures exercise the production worker and wire admission. These
/// are NOT Markdown-parser or native-AppKit qualification. The separate bridge
/// integration program invokes the real linked FrankenMarkdown implementation.
private final class Bridge: AtlasMarkdownBridge {
    var created = 0, closed = 0
    var events: [String] = []
    var createValue: UInt64 = 42
    var closes = true
    var source: String
    var rowCount: Int
    var headingCount: Int
    var mutate: (String, inout [String: Any]) -> Void = { _, _ in }
    var after: (String) -> Void = { _ in }
    var nullCommand: String?

    init(source: String = "# Heading\n**hello** 🌍\n", rows: Int = 2, headings: Int = 1) {
        self.source = source; rowCount = rows; headingCount = headings
    }
    func create() -> UInt64 { created += 1; events.append("create"); return createValue }
    func close(_ handle: UInt64) -> Bool {
        precondition(handle == createValue); closed += 1; events.append("close"); return closes
    }
    func open(_ handle: UInt64, path: String, limit: UInt64) -> String? {
        precondition(limit == 262_144); return reply("info")
    }
    func copy(_ handle: UInt64, count: UInt64) -> String? {
        reply("copy-range", extra: ["copy_domain": "original-bytes",
            "original_range": range(0, source.utf8.count),
            "original_hex": source.utf8.map { String(format: "%02x", $0) }.joined()])
    }
    func prepare(_ handle: UInt64, width: UInt64) -> String? {
        reply("document-prepare", extra: summary(width).merging(page(0, 64)) { _, b in b }
            .merging(headingPage(0, 64)) { _, b in b })
    }
    func window(_ handle: UInt64, first: UInt64) -> String? {
        reply("document-window", extra: summary().merging(page(Int(first), 128)) { _, b in b })
    }
    func headings(_ handle: UInt64, first: UInt64) -> String? {
        reply("document-headings", extra: summary().merging(headingPage(Int(first), 128)) { _, b in b })
    }
    func summary(_ width: UInt64 = 100) -> [String: Any] {
        ["document_generation": "1", "document_ready": true, "layout_complete": true,
         "native_shaped": false, "rendering": "logical-frankenmarkdown-flow",
         "source_mapping": "enclosing-regions-not-glyph-exact", "width_columns": String(width),
         "total_flow_lines": String(rowCount), "total_headings": String(headingCount),
         "rendered_utf8_bytes": String(rowCount * 4),
         "parser_source_base": source.utf8.starts(with: [239, 187, 191]) ? "3" : "0"]
    }
    func range(_ a: Int, _ b: Int) -> [String: String] { ["start": String(a), "end": String(b)] }
    func original() -> [String: String] {
        range(source.utf8.starts(with: [239, 187, 191]) ? 3 : 0, source.utf8.count)
    }
    func page(_ first: Int, _ count: Int) -> [String: Any] {
        let end = min(first + count, rowCount)
        let rows: [[String: Any]] = (first..<end).map { index in
            ["flow_line": String(index), "text": "abc",
             "rendered_utf8_range": range(index * 4, index * 4 + 4),
             "enclosing_original_range": original()]
        }
        return ["first_flow_line": String(first), "flow_lines": rows,
            "next_flow_line": end == rowCount ? NSNull() : String(end) as Any,
            "whole_document_visible": first == 0 && end == rowCount]
    }
    func headingPage(_ first: Int, _ count: Int) -> [String: Any] {
        let end = min(first + count, headingCount)
        let headings: [[String: Any]] = (first..<end).map { index in
            ["slug": "heading-\(index)", "title": "Heading \(index)",
             "rendered_utf8_offset": String(min(index, max(0, rowCount - 1)) * 4),
             "original_range": original()]
        }
        return ["headings": headings, "next_heading": end == headingCount ? NSNull() : String(end) as Any]
    }
    func reply(_ command: String, extra: [String: Any] = [:]) -> String? {
        events.append(command)
        defer { after(command) }
        guard command != nullCommand else { return nil }
        var object: [String: Any] = ["schema": "fcb.reader-session/1", "status": "ok", "command": command,
            "owner": String(createValue), "file_id": "1", "source_revision": "1",
            "captured_bytes": String(source.utf8.count), "encoding": "utf8", "additional_source_bytes_read": "0"]
        object.merge(extra) { _, b in b }; mutate(command, &object)
        return String(data: try! JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]), encoding: .utf8)
    }
}

@MainActor @main struct AtlasMarkdownRegressionTests {
    static var passed = 0
    static func test(_ name: String, _ body: () throws -> Void) rethrows {
        try body(); passed += 1; print("PASS \(name)")
    }
    static func request(_ source: String, width: Int = 100) throws -> AtlasMarkdownRequest {
        try AtlasMarkdownRequest(root: "/project", path: "docs/readme.md", source: source, width: width)
    }
    private static func load(_ bridge: Bridge) throws -> AtlasMarkdownDocument {
        try AtlasMarkdownWorker.load(request(bridge.source), bridge: bridge)
    }
    static func fails(_ expected: AtlasMarkdownError, _ body: () throws -> Void) {
        do { try body(); fatalError("expected \(expected)") }
        catch let error as AtlasMarkdownError { precondition(error == expected, "\(error) != \(expected)") }
        catch { fatalError("unexpected \(error)") }
    }
    static func main() throws {
        try test("complete capture, preview text and distinct source coordinates") {
            let b = Bridge(), r = try request(b.source)
            let doc = try AtlasMarkdownWorker.load(r, bridge: b)
            precondition(doc.requestID == r.id && doc.displayText == "abc\nabc\n")
            precondition(doc.rows.count == 2 && doc.headings.count == 1 && doc.headingRows == [0])
            precondition(doc.sourceRange(for: NSRange(location: 0, length: 3))?.upper == UInt64(b.source.utf8.count))
            precondition(doc.row(atSourceOffset: 5) == 0)
            precondition(b.closed == 1 && b.events == ["create", "info", "copy-range", "document-prepare", "close"])
        }
        try test("independent row and heading pagination") {
            let b = Bridge(rows: 301, headings: 200), doc = try load(b)
            precondition(doc.rows.count == 301 && doc.headings.count == 200 && doc.headingRows.last! == 199)
            precondition(b.events.filter { $0 == "document-window" }.count == 2)
            precondition(b.events.filter { $0 == "document-headings" }.count == 2 && b.closed == 1)
        }
        try test("empty document is a successful empty preview") {
            let b = Bridge(source: "", rows: 0, headings: 0), doc = try load(b)
            precondition(doc.rows.isEmpty && doc.displayText.isEmpty && b.closed == 1)
        }
        try test("UTF8 BOM preserves original source base") {
            let b = Bridge(source: "\u{feff}# Heading\n"), doc = try load(b)
            precondition(doc.rows[0].enclosing_original_range?.lower == 3)
            precondition(doc.row(atSourceOffset: 0) == nil)
        }
        test("source size admitted before any backend call") {
            fails(.sourceLimit) { _ = try request(String(repeating: "x", count: 262_145)) }
        }
        for path in ["", "/absolute.md", "../readme.md", "docs/../r.md", "a//b.md", "./a.md", "a/", "x\0.md"] {
            test("reject unsafe relative path \(path.debugDescription)") {
                fails(.invalidRequest) { _ = try AtlasMarkdownRequest(root: "/p", path: path, source: "x", width: 100) }
            }
        }
        for width in [3, 513, Int.max] {
            test("reject width \(width)") { fails(.invalidRequest) { _ = try request("x", width: width) } }
        }
        for stage in ["info", "copy-range", "document-prepare", "document-window", "document-headings"] {
            test("cancellation after \(stage) closes exactly once") {
                let b = Bridge(rows: 200, headings: 100); var canceled = false
                b.after = { if $0 == stage { canceled = true } }
                fails(.canceled) { _ = try AtlasMarkdownWorker.load(request(b.source), bridge: b, canceled: { canceled }) }
                precondition(b.closed == 1 && b.events.last == "close")
            }
            test("null response at \(stage) is not an empty success") {
                let b = Bridge(rows: 200, headings: 100); b.nullCommand = stage
                fails(.unavailable) { _ = try load(b) }; precondition(b.closed == 1)
            }
        }
        test("cancel before admission creates no handle") {
            let b = Bridge()
            fails(.canceled) { _ = try AtlasMarkdownWorker.load(request(b.source), bridge: b, canceled: { true }) }
            precondition(b.created == 0 && b.closed == 0)
        }
        test("capacity refusal creates no cleanup obligation") {
            let b = Bridge(); b.createValue = 0
            fails(.unavailable) { _ = try load(b) }; precondition(b.closed == 0)
        }
        test("failed close never produces a success packet or double-close") {
            let b = Bridge(); b.closes = false
            fails(.closeFailed) { _ = try load(b) }; precondition(b.closed == 1)
        }
        test("changed same-length source never reaches parser") {
            let b = Bridge(source: "old"), r = try! request("new")
            fails(.changedSource) { _ = try AtlasMarkdownWorker.load(r, bridge: b) }
            precondition(!b.events.contains("document-prepare") && b.closed == 1)
        }
        test("byte-distinct canonical-equivalent source is rejected") {
            let b = Bridge(source: "é"), r = try! request("e\u{301}")
            fails(.changedSource) { _ = try AtlasMarkdownWorker.load(r, bridge: b) }
            precondition(!b.events.contains("document-prepare"))
        }
        let mutations: [(String, String, (inout [String: Any]) -> Void)] = [
            ("owner mismatch", "info", { $0["owner"] = "43" }),
            ("noncanonical integer", "info", { $0["file_id"] = "01" }),
            ("numeric JSON integer", "info", { $0["owner"] = 42 }),
            ("overflow integer", "info", { $0["source_revision"] = "18446744073709551616" }),
            ("capture replacement", "document-window", { $0["source_revision"] = "2" }),
            ("generation replacement", "document-window", { $0["document_generation"] = "2" }),
            ("width replacement", "document-window", { $0["width_columns"] = "80" }),
            ("incomplete layout", "document-prepare", { $0["layout_complete"] = false }),
            ("unqualified rendering", "document-prepare", { $0["rendering"] = "html" }),
            ("claimed native shaping", "document-prepare", { $0["native_shaped"] = true }),
            ("oversized line count", "document-prepare", { $0["total_flow_lines"] = "8193" }),
            ("repeated continuation", "document-window", { $0["next_flow_line"] = "64" }),
            ("missing continuation", "document-prepare", { $0["next_flow_line"] = NSNull() }),
            ("false whole-document flag", "document-prepare", { $0["whole_document_visible"] = true }),
            ("early empty page", "document-window", { $0["flow_lines"] = [] }),
            ("inconsistent heading continuation", "document-headings", { $0["next_heading"] = "64" }),
            ("unbounded source region", "document-prepare", { object in
                var rows = object["flow_lines"] as! [[String: Any]]
                rows[0]["enclosing_original_range"] = ["start": "0", "end": "18446744073709551615"]
                object["flow_lines"] = rows
            }),
            ("rendered coordinates out of range", "document-prepare", { object in
                var rows = object["flow_lines"] as! [[String: Any]]
                rows[0]["rendered_utf8_range"] = ["start": "0", "end": "99999999"]
                object["flow_lines"] = rows
            }),
            ("duplicate heading identity", "document-prepare", { object in
                var headings = object["headings"] as! [[String: Any]]
                headings[1]["slug"] = headings[0]["slug"]; object["headings"] = headings
            })
        ]
        for (name, stage, mutation) in mutations {
            test(name) {
                let b = Bridge(rows: 301, headings: 200)
                b.mutate = { command, object in if command == stage { mutation(&object) } }
                fails(.invalidResponse) { _ = try load(b) }; precondition(b.closed == 1)
            }
        }
        test("backend error envelope is refused and handle released") {
            let b = Bridge(); b.mutate = { if $0 == "document-prepare" { $1["status"] = "error" } }
            fails(.unavailable) { _ = try load(b) }; precondition(b.closed == 1)
        }
        test("source mapping refuses an interior UTF8 byte") {
            let b = Bridge(source: "🌍abc")
            b.mutate = { command, object in
                if command == "document-prepare" {
                    var rows = object["flow_lines"] as! [[String: Any]]
                    rows[0]["enclosing_original_range"] = ["start": "1", "end": "4"]; object["flow_lines"] = rows
                }
            }
            fails(.invalidResponse) { _ = try load(b) }; precondition(b.closed == 1)
        }
        try test("native selection ranges do not overflow or invent an EOF map") {
            let doc = try load(Bridge())
            for range in [NSRange(location: NSNotFound, length: 0), NSRange(location: -1, length: 1),
                          NSRange(location: 0, length: Int.max), NSRange(location: doc.displayText.utf16.count, length: 0)] {
                precondition(doc.sourceRange(for: range) == nil)
            }
        }
        try test("published value stays readable after backend capture replacement") {
            let b = Bridge(), doc = try load(b)
            b.source = "changed"; precondition(doc.displayText == "abc\nabc\n" && b.closed == 1)
        }
        print("\(passed) Markdown worker/adapter tests passed")
    }
}
