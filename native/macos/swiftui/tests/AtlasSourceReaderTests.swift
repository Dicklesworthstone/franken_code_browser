import AppKit
import Foundation

/// Compile with AtlasSource.swift and AtlasSourceReader.swift on a native Mac.
/// Invoke the Swift 6 warnings-as-errors build through strict RCH at the source
/// revision under review. These scenarios do not qualify hardware presentation.
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
        appearanceSwitchKeepsInstalledSourceAndViewport()
        emptyAndOversizedSourcesHaveDistinctOutcomes()
        unwrappedLongLinesScrollHorizontally()
        wrappingReflowsWithoutReplacingSourceOrSelection()
        readersKeepIndependentWrappingAndScroll()
        wrappingCannotReviveStaleNavigation()
        physicalLineMetadataPreservesTerminatorsAndUTF16()
        lineBasesAreExplicitAndOverflowSafe()
        rulerUsesPhysicalStartsAcrossWrapping()
        horizontalScrollKeepsShortAndBlankLineLabels()
        readingGuideTogglesPreserveSourceSelectionStylesAndViewport()
        emptyAndTerminatedSourcesDoNotNumberExtraLayoutLine()
        replacementSourcesRetireOldLineMetadata()
        largeLineMetadataStaysCachedDuringPresentation()
        manualSelectionReportsInstalledSourceAndNavigation()
        clearingSelectionRevokesExactIntent()
        programmaticPresentationDoesNotCreateSelectionIntent()
        replacementAndSameTextPagesHaveDistinctSelectionAuthority()
        noncontiguousSelectionCannotBecomeAnEnclosingRange()
        separateReaderCallbacksRemainIndependent()
        print("AtlasSourceReader: 29 native selection, layout, appearance, reading-guide and source-identity scenarios passed")
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
        precondition(reader.lineRuler.lineIndex == nil && reader.lineRuler.visibleLabels().isEmpty,
                     "source refusal retained stale line metadata")
    }

    static func appearanceSwitchKeepsInstalledSourceAndViewport() {
        let source = AtlasSource(path: "theme.rs", text: String(repeating: "fn café() { /*😀*/ }\r\n", count: 120))
        let reader = AtlasSourceReaderController(), navigation = UUID(), window = host(reader)
        defer { window.close() }
        var installations = 0
        func update(_ palette: AtlasAppearancePalette) {
            precondition(reader.update(source: source, navigation: navigation, selection: nil,
                backgroundColor: palette.background.nsColor, foregroundColor: palette.color("plain").nsColor,
                palette: palette) {
                installations += 1
                let styled = NSMutableAttributedString(string: source.text, attributes: [
                    .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular),
                    .foregroundColor: AtlasAppearancePalette.canonical.color("plain").nsColor
                ])
                styled.addAttribute(.foregroundColor, value: AtlasAppearancePalette.canonical.color("keyword").nsColor,
                    range: NSRange(location: 0, length: 2))
                return styled
            })
        }
        update(.canonical)
        let selection = NSRange(location: ("fn café() { /*😀*/ }\r\n" as NSString).length * 60, length: 2)
        reader.textView.setSelectedRange(selection)
        reader.textView.scrollRangeToVisible(selection)
        _ = layout(reader)
        let viewport = reader.scrollView.contentView.bounds.origin
        let font = reader.textView.textStorage!.attribute(.font, at: 0, effectiveRange: nil) as! NSFont
        for palette in [AtlasAppearancePalette.light, .highContrastDark, .dark, .highContrastLight, .canonical] {
            update(palette)
            precondition(installations == 1, "appearance must not call the source styling closure again")
            precondition(reader.textView.string.utf8.elementsEqual(source.text.utf8))
            precondition(reader.textView.selectedRange() == selection && reader.scrollView.contentView.bounds.origin == viewport,
                         "appearance must preserve native selection and the reader's scroll anchor")
            precondition(reader.textView.textStorage!.attribute(.font, at: 0, effectiveRange: nil) as? NSFont == font)
            precondition(reader.textView.textStorage!.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor
                == palette.color("keyword").nsColor, "retained lexical color must follow every appearance switch")
        }
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

    static func physicalLineMetadataPreservesTerminatorsAndUTF16() {
        let cases: [(String, [UInt32])] = [
            ("", []), ("\n", [0]), ("\r", [0]), ("\r\n", [0]),
            ("a\n", [0]), ("a\r\n", [0]), ("\n\n", [0, 1]),
            ("\r\r\n\n", [0, 1, 3]), ("a\r\nb\rc\nd", [0, 3, 5, 7]),
            ("😀\r\ne\u{301}\nאב\rz", [0, 4, 7, 10]),
            ("\u{85}\u{2028}\u{2029}text", [0])
        ]
        for (text, expected) in cases {
            guard let index = AtlasReaderLineIndex(text: text) else {
                preconditionFailure("a bounded source failed line admission")
            }
            precondition(index.starts == expected, "physical starts disagree with CR/LF/CRLF source semantics")
            precondition(index.utf16Length == (text as NSString).length)
            precondition(AtlasSource(path: "lines", text: text).lines.count == expected.count,
                         "the reader gutter disagrees with the retained source's physical line contract")
            for (line, start) in expected.enumerated() {
                precondition(index.line(containing: Int(start)) == line)
            }
            precondition(index.line(containing: -1) == nil)
            precondition(index.line(containing: index.utf16Length) == nil,
                         "the native EOF insertion point became a source line")
        }
        let crlf = AtlasReaderLineIndex(text: "a\r\nb")!
        precondition(crlf.line(containing: 1) == 0 && crlf.line(containing: 2) == 0,
                     "CRLF was split into two physical lines")
    }

    static func lineBasesAreExplicitAndOverflowSafe() {
        let source = AtlasSource(path: "window.rs", text: "one\r\ntwo\nthree")
        let reader = AtlasSourceReaderController(), window = host(reader), navigation = UUID()
        defer { window.close() }
        func update(_ base: UInt64?) {
            precondition(reader.update(source: source, navigation: navigation, selection: nil,
                showsLineNumbers: true, firstPhysicalLine: base) {
                NSAttributedString(string: source.text, attributes: [
                    .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular)
                ])
            })
            _ = layout(reader)
        }
        update(97)
        let installed = reader.lineRuler.lineIndex
        let manual = (source.text as NSString).range(of: "two")
        reader.textView.setSelectedRange(manual)
        let cases: [(UInt64?, [String])] = [
            (97, ["97", "98", "99"]),
            (nil, ["+1", "+2", "+3"]),
            (0, ["+1", "+2", "+3"]),
            (UInt64.max, [String(UInt64.max), "?", "?"])
        ]
        for (base, expected) in cases {
            update(base)
            precondition(reader.lineRuler.visibleLabels().map(\.text) == expected,
                         "unknown or overflowing page bases produced misleading line numbers")
            precondition(reader.lineRuler.lineIndex === installed,
                         "a page-base change rescanned the immutable source")
            precondition(reader.textView.selectedRange() == manual)
        }
        precondition(reader.lineRuler.ruleThickness > 100,
                     "an overflowing last number hid the valid first 20-digit line")
        let ordinary = AtlasSourceReaderController()
        precondition(present(ordinary, source, nil))
        precondition(!ordinary.scrollView.rulersVisible && ordinary.lineRuler.firstPhysicalLine == nil,
                     "a source fragment caller accidentally claimed an absolute line base")
    }

    static func rulerUsesPhysicalStartsAcrossWrapping() {
        let source = AtlasSource(path: "long.rs",
            text: String(repeating: "word ", count: 350) + "\r\nshort\n😀last")
        let reader = AtlasSourceReaderController(), window = host(reader), navigation = UUID()
        defer { window.close() }
        func update(_ wrapped: Bool) {
            precondition(reader.update(source: source, navigation: navigation, selection: nil,
                wrapsLines: wrapped, showsLineNumbers: true, firstPhysicalLine: 42) {
                NSAttributedString(string: source.text, attributes: [
                    .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular)
                ])
            })
            _ = layout(reader)
        }
        update(false)
        let installed = reader.lineRuler.lineIndex
        precondition(reader.lineRuler.visibleLabels().map(\.text) == ["42", "43", "44"])
        update(true)
        precondition(reader.lineRuler.visibleLabels().filter { $0.physicalLine == 0 }.count == 1,
                     "wrapped fragments received duplicate physical line numbers")
        let continuation = NSRange(location: 1400, length: 1)
        reader.textView.scrollRangeToVisible(continuation)
        precondition(reader.textView.visibleRect.minY > rect(reader, NSRange(location: 0, length: 1)).maxY)
        precondition(reader.lineRuler.visibleLabels().allSatisfy { $0.physicalLine != 0 },
                     "a scrolled wrapped continuation was relabeled as a physical start")
        let next = (source.text as NSString).range(of: "short")
        reader.textView.scrollRangeToVisible(next)
        let labels = reader.lineRuler.visibleLabels()
        guard let label = labels.first(where: { $0.physicalLine == 1 }) else {
            preconditionFailure("the next visible physical source line has no gutter label")
        }
        let (manager, _) = layout(reader)
        let fragment = manager.lineFragmentRect(
            forGlyphAt: manager.glyphIndexForCharacter(at: next.location), effectiveRange: nil)
        let expected = reader.lineRuler.convert(NSPoint(x: reader.textView.textContainerOrigin.x,
            y: reader.textView.textContainerOrigin.y + fragment.minY), from: reader.textView)
        precondition(label.text == "43" && abs(label.frame.minY - expected.y) < 1)
        precondition(reader.lineRuler.lineIndex === installed)
        precondition(reader.textView.string.utf8.elementsEqual(source.text.utf8))
    }

    static func horizontalScrollKeepsShortAndBlankLineLabels() {
        let source = AtlasSource(path: "horizontal.rs",
            text: "short\n" + String(repeating: "wide ", count: 200) + "target\r\n\r\nlast")
        let reader = AtlasSourceReaderController(), window = host(reader)
        defer { window.close() }
        precondition(reader.update(source: source, navigation: UUID(), selection: nil,
            showsLineNumbers: true, firstPhysicalLine: 800) {
            NSAttributedString(string: source.text, attributes: [
                .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular)
            ])
        })
        _ = layout(reader)
        reader.textView.scrollRangeToVisible((source.text as NSString).range(of: "target"))
        precondition(reader.scrollView.contentView.bounds.minX > 0)
        precondition(reader.lineRuler.visibleLabels().map(\.text) == ["800", "801", "802", "803"],
                     "horizontal scrolling hid short or blank physical line labels")
    }

    static func readingGuideTogglesPreserveSourceSelectionStylesAndViewport() {
        let source = AtlasSource(path: "guides.rs", text: (0..<80).map {
            "row \($0):\t" + String(repeating: "word ", count: 30) + "😀 e\u{301} אב\r\n"
        }.joined())
        let reader = AtlasSourceReaderController(), window = host(reader), navigation = UUID()
        defer { window.close() }
        let oldTarget = AtlasReaderSelection(source: source, range: NSRange(location: 0, length: 3))
        let manual = (source.text as NSString).range(of: "😀 e\u{301} אב")
        let middle = (source.text as NSString).range(of: "row 40:")
        var installations = 0
        func update(_ numbers: Bool, _ whitespace: Bool) {
            precondition(reader.update(source: source, navigation: navigation, selection: oldTarget,
                showsLineNumbers: numbers, showsWhitespace: whitespace, firstPhysicalLine: 1) {
                installations += 1
                let styled = NSMutableAttributedString(string: source.text, attributes: [
                    .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular)
                ])
                styled.addAttribute(.foregroundColor, value: NSColor.systemPink, range: oldTarget.range)
                return styled
            })
        }
        update(false, false)
        reader.textView.setSelectedRange(manual)
        let selected = reader.textView.selectedRanges
        let middleRect = rect(reader, middle)
        let clip = reader.scrollView.contentView
        var bounds = clip.bounds
        bounds.origin = NSPoint(x: 0, y: middleRect.minY)
        clip.scroll(to: clip.constrainBoundsRect(bounds).origin)
        reader.scrollView.reflectScrolledClipView(clip)
        let originalY = reader.textView.visibleRect.minY
        let installed = reader.lineRuler.lineIndex
        let responder = window.firstResponder
        precondition(originalY > 0)
        for (numbers, whitespace) in [(true, false), (true, true), (false, true), (false, false), (true, true)] {
            update(numbers, whitespace)
            _ = layout(reader)
            precondition(reader.scrollView.rulersVisible == numbers)
            precondition(reader.textView.layoutManager?.showsInvisibleCharacters == whitespace)
            precondition(reader.textView.selectedRanges == selected && installations == 1,
                         "reading guides reinstalled source or replayed an old search selection")
            precondition(reader.lineRuler.lineIndex === installed,
                         "presentation updates rebuilt physical line metadata")
            precondition(reader.textView.string.utf8.elementsEqual(source.text.utf8))
            precondition((reader.textView.string as NSString).substring(with: manual) == "😀 e\u{301} אב")
            precondition(reader.textView.textStorage?.attribute(.foregroundColor, at: 0,
                effectiveRange: nil) as? NSColor == .systemPink)
            precondition(abs(reader.textView.visibleRect.minY - originalY) < 2
                && rect(reader, middle).intersects(reader.textView.visibleRect),
                "a display toggle moved away from the visible source")
            precondition(window.firstResponder === responder, "reading guides stole native focus")
        }
    }

    static func emptyAndTerminatedSourcesDoNotNumberExtraLayoutLine() {
        let reader = AtlasSourceReaderController(), window = host(reader)
        defer { window.close() }
        for (text, count) in [("", 0), ("\n", 1), ("\r", 1), ("\r\n", 1),
                              ("a\n", 1), ("a\r\n", 1), ("\n\n", 2), ("\r\n\r\n", 2)] {
            let source = AtlasSource(path: "ending", text: text)
            precondition(reader.update(source: source, navigation: UUID(), selection: nil,
                showsLineNumbers: true, firstPhysicalLine: 1) {
                NSAttributedString(string: source.text, attributes: [
                    .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular)
                ])
            })
            _ = layout(reader)
            precondition(reader.lineRuler.visibleLabels().map(\.physicalLine) == Array(0..<count),
                         "AppKit's extra EOF layout fragment became a numbered source line")
        }
    }

    static func replacementSourcesRetireOldLineMetadata() {
        let old = AtlasSource(path: "same.rs", text: "one\ntwo\nthree")
        let current = AtlasSource(path: "same.rs", text: "replacement")
        let reader = AtlasSourceReaderController(), window = host(reader), navigation = UUID()
        defer { window.close() }
        func update(_ source: AtlasSource, _ selection: AtlasReaderSelection? = nil) -> Bool {
            reader.update(source: source, navigation: navigation, selection: selection,
                showsLineNumbers: true, firstPhysicalLine: 300) {
                NSAttributedString(string: source.text, attributes: [
                    .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular)
                ])
            }
        }
        precondition(update(old))
        let oldLines = reader.lineRuler.lineIndex
        precondition(update(current))
        _ = layout(reader)
        precondition(reader.lineRuler.lineIndex !== oldLines)
        precondition(reader.lineRuler.visibleLabels().map(\.text) == ["300"])
        let currentLines = reader.lineRuler.lineIndex
        precondition(!update(current, AtlasReaderSelection(source: old, range: NSRange(location: 0, length: 3))))
        precondition(reader.lineRuler.lineIndex === currentLines
            && reader.lineRuler.visibleLabels().map(\.text) == ["300"],
            "a stale target revived old source line metadata")
    }

    static func largeLineMetadataStaysCachedDuringPresentation() {
        let source = AtlasSource(path: "many-lines", text: String(repeating: "\n", count: 65_536))
        let reader = AtlasSourceReaderController(), window = host(reader), navigation = UUID()
        defer { window.close() }
        var installations = 0
        func update(_ whitespace: Bool) {
            precondition(reader.update(source: source, navigation: navigation, selection: nil,
                showsLineNumbers: true, showsWhitespace: whitespace, firstPhysicalLine: 1) {
                installations += 1
                return NSAttributedString(string: source.text, attributes: [
                    .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular)
                ])
            })
        }
        update(false)
        guard let installed = reader.lineRuler.lineIndex else { preconditionFailure("missing line cache") }
        precondition(installed.starts.count == 65_536 && installed.utf16Length == 65_536)
        for whitespace in [true, false, true, false] {
            update(whitespace)
            let labels = reader.lineRuler.visibleLabels()
            precondition(!labels.isEmpty && labels.count <= AtlasReaderLineRuler.maximumVisibleLabels)
            precondition(reader.lineRuler.lineIndex === installed && installations == 1,
                         "scroll-time presentation rebuilt a newline-heavy capture")
        }
    }

    static func manualSelectionReportsInstalledSourceAndNavigation() {
        let source = AtlasSource(path: "selection.rs", text: "a😀 e\u{301} אב\r\nlast")
        let reader = AtlasSourceReaderController(), navigation = UUID()
        var deliveries: [(AtlasSource, UUID, NSRange?)] = []
        precondition(reader.update(source: source, navigation: navigation, selection: nil,
            onSelectionChange: { deliveries.append(($0, $1, $2)) }) {
                NSAttributedString(string: source.text)
            })
        precondition(deliveries.isEmpty, "installing a page must not fabricate a user selection")
        let range = (source.text as NSString).range(of: "😀 e\u{301} אב\r\n")
        reader.textView.setSelectedRange(range)
        precondition(deliveries.count == 1 && deliveries[0].0 === source
            && deliveries[0].1 == navigation && deliveries[0].2 == range,
            "the native delegate lost the exact displayed selection authority")
        reader.textViewDidChangeSelection(Notification(name: NSTextView.didChangeSelectionNotification,
            object: reader.textView))
        precondition(deliveries.count == 1, "identical drag notifications must be coalesced")
        reader.textViewDidChangeSelection(Notification(name: NSTextView.didChangeSelectionNotification,
            object: NSTextView()))
        precondition(deliveries.count == 1, "another text view cannot supply this reader's selection")
    }

    static func clearingSelectionRevokesExactIntent() {
        let source = AtlasSource(path: "selection.rs", text: "one two")
        let reader = AtlasSourceReaderController()
        var ranges: [NSRange?] = []
        precondition(reader.update(source: source, navigation: UUID(), selection: nil,
            onSelectionChange: { _, _, range in ranges.append(range) }) {
                NSAttributedString(string: source.text)
            })
        reader.textView.setSelectedRange(NSRange(location: 0, length: 3))
        reader.textView.setSelectedRange(NSRange(location: 3, length: 0))
        precondition(ranges.count == 2 && ranges[0] == NSRange(location: 0, length: 3)
            && ranges[1] == nil, "a caret must synchronously revoke the old text-selection intent")
    }

    static func programmaticPresentationDoesNotCreateSelectionIntent() {
        let source = AtlasSource(path: "selection.rs", text: "one\ttwo\r\nthree")
        let reader = AtlasSourceReaderController(), navigation = UUID()
        let target = AtlasReaderSelection(source: source, range: NSRange(location: 0, length: 3))
        var deliveries = 0
        func update(_ guides: Bool, _ selection: AtlasReaderSelection?) {
            precondition(reader.update(source: source, navigation: navigation, selection: selection,
                wrapsLines: guides, showsLineNumbers: guides, showsWhitespace: guides,
                onSelectionChange: { _, _, _ in deliveries += 1 }) {
                    NSAttributedString(string: source.text)
                })
        }
        update(false, target)
        precondition(deliveries == 0)
        let manual = NSRange(location: 4, length: 3)
        reader.textView.setSelectedRange(manual)
        precondition(deliveries == 1)
        update(true, target)
        update(false, nil)
        precondition(deliveries == 1 && reader.textView.selectedRange() == manual,
            "presentation controls must not replay or erase the manual selection")
    }

    static func replacementAndSameTextPagesHaveDistinctSelectionAuthority() {
        let first = AtlasSource(path: "same.rs", text: "same bytes")
        let replacement = AtlasSource(path: "same.rs", text: "same bytes")
        let reader = AtlasSourceReaderController()
        let before = UUID(), after = UUID()
        var deliveries: [(AtlasSource, UUID, NSRange?)] = []
        func update(_ source: AtlasSource, _ navigation: UUID) {
            precondition(reader.update(source: source, navigation: navigation, selection: nil,
                onSelectionChange: { deliveries.append(($0, $1, $2)) }) {
                    NSAttributedString(string: source.text)
                })
        }
        update(first, before)
        reader.textView.setSelectedRange(NSRange(location: 0, length: 4))
        update(replacement, after)
        precondition(deliveries.count == 1, "source replacement must not publish old selection coordinates")
        reader.textView.setSelectedRange(NSRange(location: 0, length: 4))
        precondition(deliveries.count == 2 && deliveries[1].0 === replacement
            && deliveries[1].1 == after && deliveries[0].0 !== deliveries[1].0,
            "equal spelling does not identify a source capture or page")
    }

    static func noncontiguousSelectionCannotBecomeAnEnclosingRange() {
        let source = AtlasSource(path: "selection.rs", text: "one two three")
        let reader = AtlasSourceReaderController()
        var range: NSRange?
        precondition(reader.update(source: source, navigation: UUID(), selection: nil,
            onSelectionChange: { _, _, value in range = value }) {
                NSAttributedString(string: source.text)
            })
        reader.textView.selectedRanges = [
            NSValue(range: NSRange(location: 0, length: 3)),
            NSValue(range: NSRange(location: 8, length: 5))
        ]
        precondition(reader.textView.selectedRanges.count == 2)
        precondition(range?.location == NSNotFound && range?.length == 1,
            "multiple passages must remain an unavailable intent, not silently save intervening bytes")
    }

    static func separateReaderCallbacksRemainIndependent() {
        let source = AtlasSource(path: "same.rs", text: "one two")
        let left = AtlasSourceReaderController(), right = AtlasSourceReaderController()
        let leftNavigation = UUID(), rightNavigation = UUID()
        var leftRanges: [NSRange?] = [], rightRanges: [NSRange?] = []
        precondition(left.update(source: source, navigation: leftNavigation, selection: nil,
            onSelectionChange: { _, navigation, range in
                precondition(navigation == leftNavigation); leftRanges.append(range)
            }) { NSAttributedString(string: source.text) })
        precondition(right.update(source: source, navigation: rightNavigation, selection: nil,
            onSelectionChange: { _, navigation, range in
                precondition(navigation == rightNavigation); rightRanges.append(range)
            }) { NSAttributedString(string: source.text) })
        left.textView.setSelectedRange(NSRange(location: 0, length: 3))
        right.textView.setSelectedRange(NSRange(location: 4, length: 3))
        precondition(leftRanges == [NSRange(location: 0, length: 3)]
            && rightRanges == [NSRange(location: 4, length: 3)])
    }

}
