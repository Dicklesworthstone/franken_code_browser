// Foundation-only production tests; runnable on macOS or Linux:
// swiftc -parse-as-library native/macos/swiftui/AtlasSource.swift \
//   native/macos/swiftui/tests/AtlasSourceRangeTests.swift -o /tmp/fcb-source-ranges
// /tmp/fcb-source-ranges
import Foundation

@main struct AtlasSourceRangeTests {
    static func expected(_ bytes: [UInt8], _ start: UInt64, _ end: UInt64) -> NSRange? {
        guard start < end, end <= UInt64(bytes.count),
              let prefix = String(bytes: bytes[..<Int(start)], encoding: .utf8),
              let selected = String(bytes: bytes[Int(start)..<Int(end)], encoding: .utf8)
        else { return nil }
        return NSRange(location: prefix.utf16.count, length: selected.utf16.count)
    }

    static func main() {
        let texts = ["", "a", "\r\n", "a😀e\u{301}日本語\r\nlast\rfinal\n",
                     "é", "e\u{301}", "שלום\t😀", "👩‍👩‍👦\n"]
        var checked = 0
        for text in texts {
            let source = AtlasSource(path: "captured/source", text: text)
            let bytes = Array(text.utf8)
            let n = UInt64(bytes.count)
            let ranges: [(UInt64, UInt64)] = (0...(n + 1)).flatMap { a in (0...(n + 1)).map { (a, $0) } }
                + [(UInt64.max, UInt64.max), (0, UInt64.max)]
            let actual = source.utf16Ranges(byteRanges: ranges)
            precondition(actual.count == ranges.count)
            for ((start, end), batch) in zip(ranges, actual) {
                let reference = expected(bytes, start, end)
                precondition(batch == reference, "Incorrect batch range \(start)..<\(end) in \(text.debugDescription)")
                precondition(source.utf16Range(byteStart: start, byteEnd: end) == reference)
                checked += 1
            }
            precondition(source.utf16Ranges(byteRanges: []).isEmpty)
        }
        let source = AtlasSource(path: "a", text: "a😀e\u{301}\r\n")
        precondition(source.utf16Range(byteStart: 1, byteEnd: 5) == NSRange(location: 1, length: 2))
        precondition(source.utf16Range(byteStart: 6, byteEnd: 8) == NSRange(location: 4, length: 1),
                     "A combining scalar is not widened to its grapheme")
        precondition(source.utf16Range(byteStart: 8, byteEnd: 10) == NSRange(location: 5, length: 2))
        let duplicates: [(UInt64, UInt64)] = [(1, 5), (0, 1), (1, 5), (2, 5), (0, 10)]
        precondition(source.utf16Ranges(byteRanges: duplicates) == duplicates.map {
            source.utf16Range(byteStart: $0.0, byteEnd: $0.1)
        })
        let tooMany = source.utf16Ranges(byteRanges: Array(repeating: (0, 1), count: 4097))
        precondition(tooMany.count == 4097 && tooMany.allSatisfy { $0 == nil })
        let fullBatch = source.utf16Ranges(byteRanges: Array(repeating: (0, 1), count: 4096))
        precondition(fullBatch.allSatisfy { $0 == NSRange(location: 0, length: 1) })
        print("AtlasSourceRange: \(checked) exhaustive ranges plus ordering and batch-limit regressions passed")
    }
}
