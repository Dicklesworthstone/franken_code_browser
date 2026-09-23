import Foundation

/// One immutable exact UTF-8 observation delivered by the engine. Line ranges
/// preserve CR/LF/CRLF and never use the atlas's sampled profile as a line count.
/// Kept only for the selected file; no per-frame source read or line scan.
final class AtlasSource {
    let path: String
    let text: String
    var lines: [Range<String.Index>] { lineMetadata.ranges }
    private var lineBytes: [Int] { lineMetadata.bytes }
    var maxDisplayColumns: Int { lineMetadata.columns }

    init(path: String, text: String) {
        self.path = path
        self.text = text
    }

    // The atlas uses prepared glyph geometry; a logical reader line index is
    // materialized once, only when a reader actually requests it.
    private lazy var lineMetadata: (ranges: [Range<String.Index>], bytes: [Int], columns: Int) = {
        var ranges: [Range<String.Index>] = []
        var sizes: [Int] = []
        var start = text.startIndex
        var cursor = start
        while cursor < text.endIndex {
            let next = text.index(after: cursor)
            let character = text[cursor]
            if character == "\n" || character == "\r" || character == "\r\n" {
                ranges.append(start..<cursor)
                sizes.append(text[start..<cursor].utf8.count)
                start = next
            }
            cursor = next
        }
        if start < text.endIndex {
            ranges.append(start..<text.endIndex)
            sizes.append(text[start..<text.endIndex].utf8.count)
        }
        let columns = ranges.reduce(1) { longest, range in
            let line = text[range]
            return max(longest, line.utf16.count + line.filter { $0 == "\t" }.count * 7)
        }
        return (ranges, sizes, columns)
    }()

    /// Only visible complete lines are shaped. A pathological single line is
    /// explicitly deferred rather than incorrectly shaping an arbitrary suffix.
    func displayLine(_ index: Int, maxUTF8Bytes: Int = 16_384) -> String? {
        guard lines.indices.contains(index) else { return nil }
        let line = text[lines[index]]
        guard lineBytes[index] <= maxUTF8Bytes else { return nil }
        return String(line).replacingOccurrences(of: "\t", with: "        ")
    }

    /// Pure geometric range calculation, independent of source length.
    func visibleLines(top: Double, lineHeight: Double, viewportHeight: Double) -> Range<Int> {
        guard lineHeight >= 6 else { return 0..<0 }
        return Self.visibleRows(count: lines.count, top: top, lineHeight: lineHeight, viewportHeight: viewportHeight)
    }

    static func visibleRows(count: Int, top: Double, lineHeight: Double, viewportHeight: Double) -> Range<Int> {
        guard top.isFinite, lineHeight.isFinite, viewportHeight.isFinite,
              lineHeight > 0, viewportHeight > 0, count > 0 else { return 0..<0 }
        let first = min(Double(count), max(0, floor(-top / lineHeight)))
        let end = min(Double(count), max(first, ceil((viewportHeight - top) / lineHeight)))
        return Int(first)..<Int(end)
    }
}
