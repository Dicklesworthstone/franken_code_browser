import AppKit
import Foundation

/// Compile with AtlasSource.swift and AtlasSourceReader.swift on a native Mac.
/// No clipboard write, source I/O, search, simulator, or release claim occurs.
@MainActor @main struct AtlasSourceReaderTests {
    static func main() {
        _ = NSApplication.shared
        preservesSourceBytesAndStyle()
        selectsVerifiedRangeWithoutTakingFocus()
        repeatedUpdatesPreserveManualSelection()
        staleCaptureCannotSelectReplacement()
        invalidRangesAreNotClamped()
        partialComposedCharactersAreNotExpanded()
        duplicateReadersKeepIndependentSelections()
        styleMismatchCannotRewriteSource()
        emptyAndOversizedSourcesHaveDistinctOutcomes()
        print("AtlasSourceReader: 9 native selection and source-identity scenarios passed")
    }
    static func present(_ reader: AtlasSourceReaderController, _ source: AtlasSource,
                        _ range: NSRange?, navigation: UUID = UUID()) -> Bool {
        reader.update(source: source, navigation: navigation,
                      selection: range.map { AtlasReaderSelection(source: source, range: $0) }) {
            NSAttributedString(string: source.text, attributes: [
                .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular)
            ])
        }
    }
    static func preservesSourceBytesAndStyle() {
        let source = AtlasSource(path: "a.swift", text: "\u{feff}a\t😀\r\ne\u{301}\r")
        let reader = AtlasSourceReaderController()
        let styled = NSMutableAttributedString(string: source.text)
        styled.addAttribute(.foregroundColor, value: NSColor.systemRed, range: NSRange(location: 1, length: 1))
        precondition(reader.update(source: source, navigation: UUID(), selection: nil) { styled })
        precondition(reader.textView.string.utf8.elementsEqual(source.text.utf8))
        precondition(reader.textView.textStorage?.attribute(.foregroundColor, at: 1, effectiveRange: nil) as? NSColor == .systemRed)
        precondition(!reader.textView.isEditable && reader.textView.isSelectable && !reader.textView.allowsUndo)
        precondition(reader.textView.usesFindPanel)
    }
    static func selectsVerifiedRangeWithoutTakingFocus() {
        let source = AtlasSource(path: "a.swift", text: "a😀 needle\r\nlast")
        let reader = AtlasSourceReaderController()
        let range = (source.text as NSString).range(of: "needle")
        precondition(present(reader, source, range))
        precondition(reader.textView.selectedRange() == range)
        precondition((reader.textView.string as NSString).substring(with: reader.textView.selectedRange()) == "needle")
        precondition(reader.textView.window == nil, "selection must not require or steal window focus")
    }
    static func repeatedUpdatesPreserveManualSelection() {
        let source = AtlasSource(path: "a.rs", text: "one two")
        let reader = AtlasSourceReaderController(), nav = UUID()
        var installations = 0
        func update(_ selection: AtlasReaderSelection?) {
            precondition(reader.update(source: source, navigation: nav, selection: selection) {
                installations += 1; return NSAttributedString(string: source.text)
            })
        }
        let target = AtlasReaderSelection(source: source, range: NSRange(location: 0, length: 3))
        update(target)
        let manual = NSRange(location: 4, length: 3)
        reader.textView.setSelectedRange(manual)
        for _ in 0..<20 { update(target) }
        precondition(reader.textView.selectedRange() == manual && installations == 1)
        update(nil)
        precondition(reader.textView.selectedRange() == manual, "clearing a search must not move the reader")
        precondition(present(reader, source, target.range))
        precondition(reader.textView.selectedRange() == target.range)
    }
    static func staleCaptureCannotSelectReplacement() {
        let old = AtlasSource(path: "same.rs", text: "old bytes")
        let new = AtlasSource(path: "same.rs", text: "new bytes")
        let reader = AtlasSourceReaderController()
        precondition(present(reader, old, NSRange(location: 0, length: 3)))
        let stale = AtlasReaderSelection(source: old, range: NSRange(location: 0, length: 3))
        precondition(!reader.update(source: new, navigation: UUID(), selection: stale) {
            NSAttributedString(string: new.text)
        })
        precondition(reader.textView.string == "new bytes")
        precondition(reader.textView.selectedRange().length == 0 && !reader.notice.isHidden)
        precondition(present(reader, new, NSRange(location: 0, length: 3)))
        precondition(reader.notice.isHidden)
    }
    static func invalidRangesAreNotClamped() {
        let source = AtlasSource(path: "a", text: "abc")
        let reader = AtlasSourceReaderController(), valid = NSRange(location: 0, length: 1)
        precondition(present(reader, source, valid))
        for range in [NSRange(location: NSNotFound, length: 0), NSRange(location: -1, length: 1),
                      NSRange(location: 4, length: 0), NSRange(location: 2, length: Int.max)] {
            precondition(!present(reader, source, range))
            precondition(reader.textView.selectedRange() == valid && !reader.notice.isHidden)
        }
    }
    static func partialComposedCharactersAreNotExpanded() {
        let source = AtlasSource(path: "unicode", text: "e\u{301} 😀")
        let reader = AtlasSourceReaderController()
        precondition(present(reader, source, nil))
        for range in [NSRange(location: 0, length: 1), NSRange(location: 1, length: 1),
                      NSRange(location: 3, length: 1), NSRange(location: 4, length: 1)] {
            precondition(!present(reader, source, range))
            precondition(reader.textView.selectedRange() == NSRange(location: 0, length: 0))
        }
        precondition(present(reader, source, NSRange(location: 0, length: 2)))
        precondition(reader.textView.selectedRange() == NSRange(location: 0, length: 2))
    }
    static func duplicateReadersKeepIndependentSelections() {
        let source = AtlasSource(path: "same", text: "one two")
        let a = AtlasSourceReaderController(), b = AtlasSourceReaderController()
        precondition(present(a, source, NSRange(location: 0, length: 3)))
        precondition(present(b, source, NSRange(location: 4, length: 3)))
        precondition(a.textView.selectedRange() != b.textView.selectedRange())
        precondition(a.textView.string.utf8.elementsEqual(b.textView.string.utf8))
    }
    static func styleMismatchCannotRewriteSource() {
        let source = AtlasSource(path: "canonical", text: "e\u{301}\r\n")
        let reader = AtlasSourceReaderController()
        precondition(reader.update(source: source, navigation: UUID(), selection: nil) {
            NSAttributedString(string: "é\n")
        })
        precondition(reader.textView.string.utf8.elementsEqual(source.text.utf8))
    }
    static func emptyAndOversizedSourcesHaveDistinctOutcomes() {
        let source = AtlasSource(path: "empty", text: "")
        let reader = AtlasSourceReaderController()
        precondition(present(reader, source, NSRange(location: 0, length: 0)))
        precondition(reader.textView.string.isEmpty && reader.notice.isHidden)
        let tooLarge = AtlasSource(path: "large", text: String(repeating: "x", count: 4 * 1024 * 1024 + 1))
        precondition(!reader.update(source: tooLarge, navigation: UUID(), selection: nil) {
            preconditionFailure("oversized source reached styling")
        })
        precondition(reader.textView.string.isEmpty && !reader.notice.isHidden)
    }
}
