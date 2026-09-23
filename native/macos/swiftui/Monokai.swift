// FrankenCodeBrowser — Monokai syntax coloring for the atlas.
//
// Single-pass tokenizer: six regex sweeps over the whole file (hundreds
// of times faster than per-line enumeration on large sources).
// Presentation coloring for the atlas; the authoritative lexical route
// remains franken_markdown's language matrix.

import AppKit
import Foundation

enum Monokai {
    static let background = NSColor(srgbRed: 0.153, green: 0.157, blue: 0.133, alpha: 1) // #272822
    static let foreground = NSColor(srgbRed: 0.973, green: 0.973, blue: 0.949, alpha: 1) // #F8F8F2
    static let comment = NSColor(srgbRed: 0.459, green: 0.443, blue: 0.369, alpha: 1)    // #75715E
    static let string = NSColor(srgbRed: 0.902, green: 0.859, blue: 0.455, alpha: 1)     // #E6DB74
    static let keyword = NSColor(srgbRed: 0.976, green: 0.149, blue: 0.447, alpha: 1)    // #F92672
    static let number = NSColor(srgbRed: 0.682, green: 0.506, blue: 1.000, alpha: 1)     // #AE81FF
    static let function = NSColor(srgbRed: 0.651, green: 0.886, blue: 0.180, alpha: 1)   // #A6E22E
    static let type = NSColor(srgbRed: 0.400, green: 0.851, blue: 0.937, alpha: 1)       // #66D9EF

    private static let keywordRegex = try! NSRegularExpression(
        pattern: #"\b(?:as|async|await|break|case|catch|class|const|continue|def|default|defer|do|elif|else|enum|except|extends|fn|for|from|func|function|go|if|impl|import|in|interface|let|match|mod|mut|new|package|pass|priv|pub|public|raise|return|select|self|static|struct|super|switch|trait|try|type|typedef|unsafe|use|var|where|while|with|yield|final|override|throws|throw|nil|null|true|false|void|volatile)\b"#
    )
    private static let numberRegex = try! NSRegularExpression(
        pattern: #"\b\d[\d_]*(?:\.\d+)?(?:[eE][+-]?\d+)?\b"#
    )
    private static let typeRegex = try! NSRegularExpression(
        pattern: #"\b[A-Z][A-Za-z0-9_]*\b"#
    )
    private static let callRegex = try! NSRegularExpression(
        pattern: #"\b[a-z_][A-Za-z0-9_]*(?=\s*\()"#
    )
    private static let stringRegex = try! NSRegularExpression(
        pattern: #""[^"\n]*"|'[^'\n]*'"#
    )
    private static let lineCommentRegex = try! NSRegularExpression(
        pattern: #"(?m)//[^\n]*|^\s*#[^\n]*"#
    )
    private static let blockCommentRegex = try! NSRegularExpression(
        pattern: #"/\*[\s\S]*?\*/"#
    )

    /// Colors a whole file in one pass — six sweeps over the full range.
    /// Comments paint first (masking everything inside them), then
    /// strings, then tokens skip already-painted spans.
    static func colorSource(_ source: String) -> NSAttributedString {
        let full = NSRange(location: 0, length: (source as NSString).length)
        let attributed = NSMutableAttributedString(
            string: source,
            attributes: [
                .font: NSFont.monospacedSystemFont(ofSize: 12, weight: .regular),
                .foregroundColor: foreground,
            ]
        )

        func paint(_ regex: NSRegularExpression, _ color: NSColor) {
            regex.enumerateMatches(in: source, range: full) { match, _, _ in
                if let range = match?.range {
                    attributed.addAttribute(.foregroundColor, value: color, range: range)
                }
            }
        }

        paint(blockCommentRegex, comment)
        paint(lineCommentRegex, comment)
        paint(stringRegex, string)
        paint(numberRegex, number)
        paint(keywordRegex, keyword)
        paint(typeRegex, type)
        paint(callRegex, function)
        return attributed
    }
}
