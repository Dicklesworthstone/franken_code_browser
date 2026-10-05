// Compile on macOS with the same production sources as AtlasMatchTests.swift.
// These checks deliberately need no atlas layout, glyph shaping, or file I/O.
import Foundation
import CryptoKit

@main struct AtlasSourceMatchTests {
    static func hash(_ text: String) -> String {
        SHA256.hash(data: Data(text.utf8)).map { String(format: "%02x", $0) }.joined()
    }

    static func main() {
        let text = String(repeating: "line\r\n", count: 300) + "日本語 😀 e\u{301}\n"
        let source = AtlasSource(path: "not-in-eager-atlas/source.swift", text: text)
        let digest = hash(text)
        let count = UInt64(text.utf8.count)
        func range(_ start: UInt64, _ end: UInt64, digest proof: String? = nil,
                   count size: UInt64? = nil) -> NSRange? {
            AtlasMatch.resolveSourceRange(source: source, byteStart: start, byteEnd: end,
                expectedSHA256: proof ?? digest, expectedByteCount: size ?? count)
        }
        precondition(range(0, count) == NSRange(location: 0, length: text.utf16.count),
                     "Source navigation must not depend on the 256-row overlay budget")
        precondition(range(0, count, digest: digest.uppercased()) == range(0, count))
        let japanese = UInt64(300 * "line\r\n".utf8.count)
        precondition(range(japanese, japanese + 9) == NSRange(location: 1800, length: 3))
        precondition(range(japanese + 1, japanese + 3) == nil)
        precondition(range(0, 0) == nil)
        precondition(range(10, 1) == nil)
        precondition(range(0, UInt64.max) == nil)
        precondition(range(0, count, count: count + 1) == nil)
        precondition(range(0, count, digest: String(repeating: "z", count: 64)) == nil)
        precondition(range(0, count, digest: hash(text.replacingOccurrences(of: "line", with: "same"))) == nil,
                     "An equally sized but changed capture must not authorize offsets")
        let composed = AtlasSource(path: source.path, text: "é\n")
        precondition(AtlasMatch.resolveSourceRange(source: composed, byteStart: 0, byteEnd: 2,
            expectedSHA256: hash("e\u{301}\n"), expectedByteCount: UInt64(composed.text.utf8.count)) == nil,
            "Unicode canonical equivalence is not byte identity")
        print("AtlasSourceMatch: layout-independent selection and capture-proof refusals passed")
    }
}
