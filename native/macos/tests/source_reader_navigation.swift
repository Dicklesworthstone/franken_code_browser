// Run on macOS from the repository root:
// swiftc native/macos/swiftui/AtlasSource.swift \
//   native/macos/swiftui/AtlasSourceReader.swift \
//   native/macos/tests/source_reader_navigation.swift -o /tmp/fcb-reader-navigation
// /tmp/fcb-reader-navigation
import AppKit

@main
struct SourceReaderNavigationTests {
    @MainActor static func main() {
        let controller = AtlasSourceReaderController()
        let source = AtlasSource(path: "old/source.swift", text: "first\nsecond\n")
        let navigation = UUID()
        var styleCalls = 0
        let style = {
            styleCalls += 1
            return NSAttributedString(string: source.text)
        }
        precondition(controller.update(source: source, navigation: navigation,
            selection: nil, styledSource: style))
        precondition(controller.textView.string == source.text)
        let manual = NSRange(location: 6, length: 6)
        controller.textView.setSelectedRange(manual)
        precondition(controller.update(source: source, navigation: navigation,
            selection: nil, styledSource: style))
        precondition(controller.textView.selectedRange() == manual)
        precondition(styleCalls == 1, "Repeated presentation must not restyle source")

        // Same pathname and equal text do not authorize a different capture's
        // selection. Refusing a selection must retain current source and caret.
        let otherCapture = AtlasSource(path: source.path, text: source.text)
        let stale = AtlasReaderSelection(source: otherCapture,
                                        range: NSRange(location: 0, length: 5))
        precondition(!controller.update(source: source, navigation: navigation,
            selection: stale, styledSource: style))
        precondition(controller.textView.string == source.text)
        precondition(controller.textView.selectedRange() == manual)

        let oversized = AtlasSource(path: "new/source.swift",
            text: String(repeating: "x", count: 4 * 1024 * 1024 + 1))
        for _ in 0..<2 {
            precondition(!controller.update(source: oversized, navigation: UUID(),
                selection: nil, styledSource: {
                    preconditionFailure("Rejected captures must not be styled")
                }))
            precondition(controller.textView.string.isEmpty,
                         "Rejected replacement must not retain the previous capture")
            precondition(controller.textView.selectedRange() == NSRange(location: 0, length: 0))
            precondition(controller.textView.accessibilityLabel() == "Read-only source unavailable")
            precondition(!controller.notice.isHidden)
        }

        // Restoring the exact previously installed object and navigation token
        // must reinstall it, not reuse the refusal or its old manual selection.
        precondition(controller.update(source: source, navigation: navigation,
            selection: nil, styledSource: style))
        precondition(controller.textView.string == source.text)
        precondition(controller.textView.selectedRange() == NSRange(location: 0, length: 0))
        precondition(controller.notice.isHidden)
        precondition(styleCalls == 2)

        // Source publication cannot borrow a prior capture's attributed bytes.
        let replacement = AtlasSource(path: "new/source.swift", text: "replacement\n")
        precondition(controller.update(source: replacement, navigation: UUID(),
            selection: nil, styledSource: { NSAttributedString(string: source.text) }))
        precondition(controller.textView.string == replacement.text)
        precondition(controller.textView.accessibilityLabel() == "Read-only source: new/source.swift")
        print("source reader navigation regressions passed")
    }
}
