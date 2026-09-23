import Foundation

@main struct AtlasSourceTests {
    static func main() {
        let text = "one\r\ntwo\r🦀 three\nfour"
        let source = AtlasSource(path: "sample.rs", text: text)
        precondition(source.text == text)
        precondition(source.lines.count == 4)
        precondition((0..<4).map { source.displayLine($0) ?? "" } == ["one", "two", "🦀 three", "four"])
        precondition(source.visibleLines(top: -20, lineHeight: 10, viewportHeight: 10) == 2..<3)
        precondition(source.visibleLines(top: 10, lineHeight: 10, viewportHeight: 10).isEmpty)
        precondition(AtlasSource(path: "empty", text: "").lines.isEmpty)
        precondition(AtlasSource(path: "newline", text: "a\n").lines.count == 1)
        precondition(AtlasSource(path: "blank", text: "\n\n").lines.count == 2)
        let huge = AtlasSource(path: "huge", text: String(repeating: "x", count: 20_000) + "\nlast")
        precondition(huge.displayLine(0) == nil)
        precondition(huge.displayLine(1) == "last")
        precondition(huge.text.utf8.count == 20_005, "deferring layout must preserve all source")
        let many = AtlasSource(path: "many", text: String(repeating: "line\n", count: 10_000))
        precondition(many.visibleLines(top: -99_980, lineHeight: 10, viewportHeight: 20) == 9998..<10000)
        precondition(many.displayLine(9999) == "line", "source beyond old profile row cap stays readable")
        precondition(AtlasSource.visibleRows(count: 4000, top: -5000, lineHeight: 2,
            viewportHeight: 20) == 2500..<2510, "panning past the first 600 profile rows must not go blank")
        precondition(AtlasSource.visibleRows(count: 4000, top: 0, lineHeight: 0,
            viewportHeight: 20).isEmpty)
        print("AtlasSource: 15 exact-source, newline, clipping and large-source assertions passed")
    }
}
