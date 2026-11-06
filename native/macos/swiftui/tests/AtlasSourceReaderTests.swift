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
        themedReaderKeepsSelectionAndInstalledStyles()
        emptyAndOversizedSourcesHaveDistinctOutcomes()
        unwrappedLongLinesScrollHorizontally()
        wrappingReflowsWithoutReplacingSourceOrSelection()
        readersKeepIndependentWrappingAndScroll()
        wrappingCannotReviveStaleNavigation()
        print("AtlasSourceReader: 14 native selection, layout, scrolling and source-identity scenarios passed")
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
        precondition(reader.update(source: source, navigation: UUID(), selection: nil,
            backgroundColor: .black, foregroundColor: .white) {
            NSAttributedString(string: "é\n")
        })
        precondition(reader.textView.string.utf8.elementsEqual(source.text.utf8))
        precondition(reader.textView.backgroundColor == .black && reader.scrollView.backgroundColor == .black)
        precondition(reader.textView.textStorage?.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor == .white,
                     "exact-text fallback lost the admitted foreground color")
    }
    static func themedReaderKeepsSelectionAndInstalledStyles() {
        let source = AtlasSource(path: "page.rs", text: "fn call() { /*😀*/ }\r\n")
        let reader = AtlasSourceReaderController(), navigation = UUID()
        let selected = (source.text as NSString).range(of: "😀")
        var installations = 0
        func update() {
            precondition(reader.update(source: source, navigation: navigation, selection: nil,
                backgroundColor: .black, foregroundColor: .white) {
                installations += 1
                let styled = NSMutableAttributedString(string: source.text, attributes: [
                    .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular),
                    .foregroundColor: NSColor.white
                ])
                styled.addAttribute(.foregroundColor, value: NSColor.systemPink, range: NSRange(location: 0, length: 2))
                return styled
            })
        }
        update()
        reader.textView.setSelectedRange(selected)
        for _ in 0..<20 { update() }
        precondition(installations == 1 && reader.textView.selectedRange() == selected,
                     "presentation updates reinstalled source or disturbed manual selection")
        precondition((reader.textView.string as NSString).substring(with: selected) == "😀",
                     "selection/copy no longer addresses exact source")
        precondition(reader.textView.string.utf8.elementsEqual(source.text.utf8))
        precondition(reader.textView.backgroundColor == .black && reader.scrollView.backgroundColor == .black)
        precondition(reader.textView.textStorage?.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor == .systemPink,
                     "presentation colors replaced token attributes")
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

    /// Hidden native windows give the real scroll/clip/document views a finite
    /// viewport. Nothing is ordered on screen or made the first responder.
    static func host(_ reader: AtlasSourceReaderController) -> NSWindow {
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 640, height: 260),
            styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = reader.view
        reader.view.layoutSubtreeIfNeeded()
        precondition(reader.scrollView.contentSize.width > 300 && reader.scrollView.contentSize.height > 100)
        return window
    }
    static func layout(_ reader: AtlasSourceReaderController) -> (NSLayoutManager, NSTextContainer) {
        guard let manager = reader.textView.layoutManager, let container = reader.textView.textContainer else {
            preconditionFailure("the production reader lost its native text layout")
        }
        manager.ensureLayout(for: container)
        reader.scrollView.tile()
        manager.ensureLayout(for: container)
        return (manager, container)
    }
    static func rect(_ reader: AtlasSourceReaderController, _ range: NSRange) -> NSRect {
        let (manager, container) = layout(reader)
        let glyphs = manager.glyphRange(forCharacterRange: range, actualCharacterRange: nil)
        return manager.boundingRect(forGlyphRange: glyphs, in: container)
            .offsetBy(dx: reader.textView.textContainerOrigin.x, dy: reader.textView.textContainerOrigin.y)
    }
    static func unwrappedLongLinesScrollHorizontally() {
        let source = AtlasSource(path: "wide.rs", text: String(repeating: "column\t", count: 160) + "target\r\nnext")
        let reader = AtlasSourceReaderController(), window = host(reader)
        defer { window.close() }
        precondition(present(reader, source, nil))
        let target = (source.text as NSString).range(of: "target")
        let (manager, _) = layout(reader)
        let first = manager.lineFragmentRect(forGlyphAt: 0, effectiveRange: nil)
        let last = manager.lineFragmentRect(forGlyphAt: manager.glyphIndexForCharacter(at: target.location),
            effectiveRange: nil)
        precondition(first.minY == last.minY, "a physical source line still wraps by default")
        precondition(reader.scrollView.hasHorizontalScroller && reader.textView.isHorizontallyResizable)
        precondition(reader.textView.textContainer?.widthTracksTextView == false)
        precondition(reader.textView.frame.width > reader.scrollView.contentSize.width * 2,
                     "the document did not grow beyond the actual clip width")
        reader.textView.scrollRangeToVisible(target)
        precondition(reader.scrollView.contentView.bounds.minX > 0,
                     "the end of a long physical line cannot be reached horizontally")
        precondition(rect(reader, target).intersects(reader.textView.visibleRect),
                     "horizontal navigation did not reveal the requested source")
        precondition(reader.textView.string.utf8.elementsEqual(source.text.utf8))
    }
    static func wrappingReflowsWithoutReplacingSourceOrSelection() {
        let source = AtlasSource(path: "wrapped.rs", text: (0..<80).map {
            "row \($0): " + String(repeating: "word\t", count: 40) + "😀 e\u{301} אב\r\n"
        }.joined())
        let reader = AtlasSourceReaderController(), window = host(reader), navigation = UUID()
        defer { window.close() }
        let search = AtlasReaderSelection(source: source, range: NSRange(location: 0, length: 3))
        let manual = (source.text as NSString).range(of: "😀 e\u{301} אב")
        let middle = (source.text as NSString).range(of: "row 40:")
        var installations = 0
        func update(_ wrapped: Bool) {
            precondition(reader.update(source: source, navigation: navigation, selection: search, wrapsLines: wrapped) {
                installations += 1
                let styled = NSMutableAttributedString(string: source.text, attributes: [
                    .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular)
                ])
                styled.addAttribute(.foregroundColor, value: NSColor.systemPink, range: search.range)
                return styled
            })
        }
        update(false)
        reader.textView.setSelectedRange(manual)
        let middleRect = rect(reader, middle)
        var bounds = reader.scrollView.contentView.bounds
        bounds.origin = NSPoint(x: 0, y: middleRect.minY)
        reader.scrollView.contentView.scroll(to: reader.scrollView.contentView.constrainBoundsRect(bounds).origin)
        reader.scrollView.reflectScrolledClipView(reader.scrollView.contentView)
        let originalY = reader.textView.visibleRect.minY
        precondition(originalY > 0 && middleRect.intersects(reader.textView.visibleRect))
        for wrapped in [true, false, true, false] {
            update(wrapped)
            let (manager, container) = layout(reader)
            let start = manager.glyphIndexForCharacter(at: middle.location)
            let end = manager.glyphIndexForCharacter(at: middle.location + 160)
            let startLine = manager.lineFragmentRect(forGlyphAt: start, effectiveRange: nil)
            let endLine = manager.lineFragmentRect(forGlyphAt: end, effectiveRange: nil)
            precondition(wrapped ? endLine.minY > startLine.minY : endLine.minY == startLine.minY,
                         "changing Wrap lines did not reflow and restore the physical line")
            precondition(container.widthTracksTextView == wrapped)
            precondition(reader.scrollView.hasHorizontalScroller != wrapped)
            if wrapped {
                precondition(reader.textView.frame.width <= reader.scrollView.contentSize.width + 1,
                             "wrapped source still retains its wide scrollable frame")
                precondition(reader.scrollView.contentView.bounds.minX == 0)
            } else {
                precondition(reader.textView.frame.width > reader.scrollView.contentSize.width)
                precondition(abs(reader.textView.visibleRect.minY - originalY) < 2,
                             "a wrap round trip lost the current source row")
            }
            precondition(rect(reader, middle).intersects(reader.textView.visibleRect),
                         "reflow jumped away from the visible source anchor")
            precondition(installations == 1 && reader.textView.selectedRange() == manual,
                         "layout changes reinstalled source or replayed the old search selection")
            precondition(reader.textView.string.utf8.elementsEqual(source.text.utf8))
            precondition((reader.textView.string as NSString).substring(with: manual) == "😀 e\u{301} אב")
            precondition(reader.textView.textStorage?.attribute(.foregroundColor, at: 0,
                effectiveRange: nil) as? NSColor == .systemPink, "layout changes discarded syntax styles")
        }
        // A subsequent explicit search navigation still activates exactly once.
        precondition(reader.update(source: source, navigation: UUID(), selection: search) {
            preconditionFailure("navigation must not reinstall this capture")
        })
        precondition(reader.textView.selectedRange() == search.range)
    }
    static func readersKeepIndependentWrappingAndScroll() {
        let source = AtlasSource(path: "same.rs", text: String(repeating: "source ", count: 180) + "last")
        let a = AtlasSourceReaderController(), b = AtlasSourceReaderController()
        let aWindow = host(a), bWindow = host(b), navigation = UUID()
        defer { aWindow.close(); bWindow.close() }
        precondition(present(a, source, nil, navigation: navigation))
        precondition(present(b, source, nil, navigation: navigation))
        let end = (source.text as NSString).range(of: "last")
        _ = layout(b)
        b.textView.scrollRangeToVisible(end)
        let independentOrigin = b.scrollView.contentView.bounds.origin
        precondition(independentOrigin.x > 0)
        precondition(a.update(source: source, navigation: navigation, selection: nil, wrapsLines: true) {
            preconditionFailure("wrapping a peer reader must not rebuild shared source")
        })
        aWindow.setContentSize(NSSize(width: 420, height: 260))
        a.view.layoutSubtreeIfNeeded()
        _ = layout(a)
        precondition(a.textView.textContainer?.widthTracksTextView == true)
        precondition(a.textView.frame.width <= a.scrollView.contentSize.width + 1,
                     "wrapped source stopped tracking the resized reader")
        precondition(b.textView.textContainer?.widthTracksTextView == false && b.scrollView.hasHorizontalScroller)
        precondition(b.scrollView.contentView.bounds.origin == independentOrigin,
                     "changing one reader moved another reader's viewport")
        precondition(rect(b, end).intersects(b.textView.visibleRect))
    }
    static func wrappingCannotReviveStaleNavigation() {
        let old = AtlasSource(path: "same.rs", text: "old source")
        let current = AtlasSource(path: "same.rs", text: "current source")
        let reader = AtlasSourceReaderController(), navigation = UUID()
        precondition(present(reader, current, nil, navigation: navigation))
        let manual = NSRange(location: 8, length: 6)
        reader.textView.setSelectedRange(manual)
        let stale = AtlasReaderSelection(source: old, range: NSRange(location: 0, length: 3))
        for wrapped in [true, false] {
            precondition(!reader.update(source: current, navigation: navigation,
                selection: stale, wrapsLines: wrapped) { preconditionFailure("source must stay installed") })
            precondition(reader.textView.selectedRange() == manual && !reader.notice.isHidden)
            precondition(reader.textView.string.utf8.elementsEqual(current.text.utf8))
        }
    }
}
