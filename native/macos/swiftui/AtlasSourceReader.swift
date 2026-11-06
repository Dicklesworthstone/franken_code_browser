import SwiftUI
import AppKit

/// A native UTF-16 target bound to the immutable source that produced it. This
/// is not an original-byte range and never authorizes a fresh pathname read.
struct AtlasReaderSelection {
    let source: AtlasSource
    let range: NSRange
}

/// Read-only native text selection over the already admitted source. Source
/// styling is requested only when capture identity changes, not on every SwiftUI
/// update. AppKit owns selection, keyboard navigation, copy and accessibility.
@MainActor struct AtlasSourceReader: NSViewRepresentable {
    let source: AtlasSource
    let navigation: UUID
    let selection: AtlasReaderSelection?
    var wrapsLines = false
    var showsLineNumbers = false
    var showsWhitespace = false
    var firstPhysicalLine: UInt64? = nil
    var backgroundColor: NSColor = .textBackgroundColor
    var foregroundColor: NSColor = .textColor
    let styledSource: () -> NSAttributedString

    func makeCoordinator() -> AtlasSourceReaderController { AtlasSourceReaderController() }
    func makeNSView(context: Context) -> NSStackView { context.coordinator.view }
    func updateNSView(_ view: NSStackView, context: Context) {
        context.coordinator.update(source: source, navigation: navigation,
            selection: selection, wrapsLines: wrapsLines, showsLineNumbers: showsLineNumbers,
            showsWhitespace: showsWhitespace, firstPhysicalLine: firstPhysicalLine,
            backgroundColor: backgroundColor, foregroundColor: foregroundColor, styledSource: styledSource)
    }
}

/// Also exercised directly by the native tests. Updates never read, search,
/// rewrite, or save source. A repeated presentation does not reset a user's
/// manual selection or scroll position. Explicit navigation alone reveals a hit.
@MainActor final class AtlasSourceReaderController {
    let view: NSStackView
    let scrollView: NSScrollView
    let textView: NSTextView
    let notice: NSTextField
    let lineRuler: AtlasReaderLineRuler
    private var installedSource: AtlasSource?
    private var lastNavigation: UUID?
    private var lastTargetSource: AtlasSource?
    private var lastRange: NSRange?
    private var lastAccepted = true
    private var wrapsLines = false
    private var showsLineNumbers = false
    private var showsWhitespace = false
    // The native route already admits at most 4 MiB of source (and smaller
    // retained pages). This is AppKit's finite unconstrained layout dimension,
    // not a document frame size or a claim of giant-line virtualization.
    private static let layoutExtent = CGFloat(Float.greatestFiniteMagnitude)

    private struct ViewportAnchor {
        let character: Int
        let offset: NSPoint
    }

    init() {
        scrollView = NSScrollView(frame: NSRect(x: 0, y: 0, width: 640, height: 220))
        textView = NSTextView(frame: NSRect(x: 0, y: 0, width: 640, height: 220))
        lineRuler = AtlasReaderLineRuler(scrollView: scrollView, textView: textView)
        notice = NSTextField(labelWithString: "")
        view = NSStackView(views: [scrollView, notice])
        view.orientation = .vertical
        view.alignment = .leading
        view.spacing = 4
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        scrollView.widthAnchor.constraint(equalTo: view.widthAnchor).isActive = true
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.borderType = .noBorder
        textView.isEditable = false
        textView.isSelectable = true
        textView.isRichText = true
        textView.importsGraphics = false
        textView.allowsUndo = false
        textView.usesFindPanel = true
        textView.isAutomaticLinkDetectionEnabled = false
        textView.isContinuousSpellCheckingEnabled = false
        textView.isAutomaticSpellingCorrectionEnabled = false
        textView.isAutomaticQuoteSubstitutionEnabled = false
        textView.isAutomaticDashSubstitutionEnabled = false
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = true
        textView.autoresizingMask = [.width, .height]
        textView.minSize = .zero
        textView.maxSize = NSSize(width: Self.layoutExtent, height: Self.layoutExtent)
        textView.textContainerInset = NSSize(width: 8, height: 8)
        textView.textContainer?.containerSize = NSSize(width: Self.layoutExtent, height: Self.layoutExtent)
        textView.textContainer?.widthTracksTextView = false
        textView.layoutManager?.allowsNonContiguousLayout = true
        textView.font = .monospacedSystemFont(ofSize: 13, weight: .regular)
        textView.setAccessibilityLabel("Read-only source")
        scrollView.documentView = textView
        lineRuler.clientView = textView
        scrollView.verticalRulerView = lineRuler
        scrollView.hasHorizontalRuler = false
        scrollView.hasVerticalRuler = true
        scrollView.rulersVisible = false
        notice.font = .systemFont(ofSize: 11)
        notice.textColor = .secondaryLabelColor
        notice.isHidden = true
    }

    @discardableResult
    func update(source: AtlasSource, navigation: UUID, selection: AtlasReaderSelection?,
                wrapsLines: Bool = false, showsLineNumbers: Bool = false,
                showsWhitespace: Bool = false, firstPhysicalLine: UInt64? = nil,
                backgroundColor: NSColor = .textBackgroundColor, foregroundColor: NSColor = .textColor,
                styledSource: () -> NSAttributedString) -> Bool {
        precondition(Thread.isMainThread)
        if textView.backgroundColor != backgroundColor { textView.backgroundColor = backgroundColor }
        if scrollView.backgroundColor != backgroundColor { scrollView.backgroundColor = backgroundColor }
        textView.insertionPointColor = foregroundColor
        let changed = installedSource !== source
        var replacementLines: AtlasReaderLineIndex?
        if changed {
            // Same bound as the complete-source native opening route. Never
            // publish a prefix under a whole-source label, or retain another
            // capture under the replacement source's surrounding UI.
            guard let lines = AtlasReaderLineIndex(text: source.text) else {
                return refuseSource("Source exceeds this native reader's 4 MiB limit.")
            }
            replacementLines = lines
        }
        setWrapping(wrapsLines, preservingPosition: !changed)
        if changed {
            let styled = styledSource()
            let exact = styled.string.utf8.elementsEqual(source.text.utf8)
            let content = exact ? styled : NSAttributedString(string: source.text, attributes: [
                .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular),
                .foregroundColor: foregroundColor
            ])
            textView.textStorage?.setAttributedString(content)
            installedSource = source
            lineRuler.install(replacementLines)
            textView.setAccessibilityLabel("Read-only source: \(source.path)")
            textView.setSelectedRange(NSRange(location: 0, length: 0))
        }
        setReadingGuides(showsLineNumbers: showsLineNumbers, showsWhitespace: showsWhitespace,
            firstPhysicalLine: firstPhysicalLine, foregroundColor: foregroundColor,
            backgroundColor: backgroundColor, preservingPosition: !changed)
        let moved = changed || lastNavigation != navigation
        let retargeted = lastTargetSource !== selection?.source || lastRange != selection?.range
        if !moved && !retargeted { return lastAccepted }
        lastNavigation = navigation
        lastTargetSource = selection?.source
        lastRange = selection?.range
        notice.isHidden = true
        lastAccepted = true
        guard let selection else {
            // Clearing search highlighting is not a request to discard manual
            // text selection. Opening a file explicitly does reset its caret.
            if moved {
                textView.setSelectedRange(NSRange(location: 0, length: 0))
                textView.scrollRangeToVisible(NSRange(location: 0, length: 0))
            }
            return true
        }
        guard selection.source === source, nativeRange(selection.range) else {
            return refuse("Exact native selection unavailable for this match. Source bytes are unchanged.")
        }
        let previous = textView.selectedRange()
        textView.setSelectedRange(selection.range)
        guard textView.selectedRange() == selection.range else {
            textView.setSelectedRange(previous)
            return refuse("AppKit could not select this exact range. The previous selection is retained.")
        }
        textView.scrollRangeToVisible(selection.range)
        return true
    }

    /// Wrapping is presentation state of this reader, not a source/navigation
    /// request. Do not reinstall text, set selection, or replay a search target.
    /// Reflow keeps the character at the viewport's leading edge in view instead
    /// of preserving a scroll percentage over a newly shaped document height.
    private func setWrapping(_ wrapsLines: Bool, preservingPosition: Bool) {
        guard self.wrapsLines != wrapsLines else { return }
        let anchor = preservingPosition ? viewportAnchor() : nil
        self.wrapsLines = wrapsLines
        applyWrappingGeometry()
        if let anchor { restoreViewport(anchor) }
        lineRuler.needsDisplay = true
    }

    private func applyWrappingGeometry() {
        let container = textView.textContainer
        container?.widthTracksTextView = false
        scrollView.hasHorizontalScroller = !wrapsLines
        textView.isHorizontallyResizable = !wrapsLines
        textView.autoresizingMask = wrapsLines ? [.width] : [.width, .height]
        if wrapsLines {
            let width = max(1, scrollView.contentSize.width)
            textView.setFrameSize(NSSize(width: width, height: textView.frame.height))
            container?.containerSize = NSSize(
                width: max(1, width - 2 * textView.textContainerInset.width), height: Self.layoutExtent)
            container?.widthTracksTextView = true
        } else {
            container?.containerSize = NSSize(width: Self.layoutExtent, height: Self.layoutExtent)
        }
    }

    /// Reading guides are layout/drawing state. They never substitute source
    /// characters, replay navigation, or change the bytes selected for copy.
    private func setReadingGuides(showsLineNumbers: Bool, showsWhitespace: Bool,
                                  firstPhysicalLine: UInt64?, foregroundColor: NSColor,
                                  backgroundColor: NSColor, preservingPosition: Bool) {
        let base = firstPhysicalLine.flatMap { $0 > 0 ? $0 : nil }
        let width = lineRuler.thickness(firstPhysicalLine: base)
        lineRuler.foregroundColor = foregroundColor
        lineRuler.backgroundColor = backgroundColor
        guard self.showsLineNumbers != showsLineNumbers || self.showsWhitespace != showsWhitespace
                || lineRuler.firstPhysicalLine != base || lineRuler.ruleThickness != width else {
            lineRuler.needsDisplay = true
            return
        }
        let anchor = preservingPosition ? viewportAnchor() : nil
        let selections = textView.selectedRanges
        self.showsLineNumbers = showsLineNumbers
        self.showsWhitespace = showsWhitespace
        lineRuler.firstPhysicalLine = base
        lineRuler.toolTip = base == nil
            ? "Page lines (+). Physical file line numbers are unavailable for this byte window."
            : "Physical source lines. Wrapped continuations keep the same line number."
        lineRuler.ruleThickness = width
        scrollView.rulersVisible = showsLineNumbers
        textView.layoutManager?.showsInvisibleCharacters = showsWhitespace
        scrollView.tile()
        applyWrappingGeometry()
        if textView.selectedRanges != selections { textView.selectedRanges = selections }
        if let anchor { restoreViewport(anchor) }
        lineRuler.needsDisplay = true
    }

    private func viewportAnchor() -> ViewportAnchor? {
        let visible = textView.visibleRect
        let origin = textView.textContainerOrigin
        let point = NSPoint(x: max(visible.minX, origin.x), y: max(visible.minY, origin.y))
        let character = textView.characterIndexForInsertion(at: point)
        guard let location = location(of: character) else { return nil }
        return ViewportAnchor(character: character,
            offset: NSPoint(x: visible.minX - location.x, y: visible.minY - location.y))
    }

    private func restoreViewport(_ anchor: ViewportAnchor) {
        // AppKit lays out the anchor and updates its scrollable document extent.
        // This does not change the user's selected ranges or first responder.
        textView.scrollRangeToVisible(NSRange(location: anchor.character, length: 0))
        guard let location = location(of: anchor.character) else { return }
        let clip = scrollView.contentView
        var bounds = clip.bounds
        bounds.origin = NSPoint(x: wrapsLines ? 0 : location.x + anchor.offset.x,
            y: location.y + anchor.offset.y)
        clip.scroll(to: clip.constrainBoundsRect(bounds).origin)
        scrollView.reflectScrolledClipView(clip)
    }

    private func location(of character: Int) -> NSPoint? {
        guard let storage = textView.textStorage, character >= 0, character < storage.length,
              let layout = textView.layoutManager else { return nil }
        let glyph = layout.glyphIndexForCharacter(at: character)
        guard layout.isValidGlyphIndex(glyph) else { return nil }
        let fragment = layout.lineFragmentRect(forGlyphAt: glyph, effectiveRange: nil)
        let position = layout.location(forGlyphAt: glyph)
        let origin = textView.textContainerOrigin
        let location = NSPoint(x: origin.x + fragment.minX + position.x, y: origin.y + fragment.minY)
        return location.x.isFinite && location.y.isFinite ? location : nil
    }

    /// Refusing a replacement capture revokes the old presentation as well.
    /// Selection-only refusals below intentionally retain the current source;
    /// source-admission failures must not leave it available to read or copy.
    private func refuseSource(_ message: String) -> Bool {
        textView.textStorage?.setAttributedString(NSAttributedString(string: ""))
        textView.setSelectedRange(NSRange(location: 0, length: 0))
        textView.scrollRangeToVisible(NSRange(location: 0, length: 0))
        textView.setAccessibilityLabel("Read-only source unavailable")
        installedSource = nil
        lineRuler.install(nil)
        lastNavigation = nil
        lastTargetSource = nil
        lastRange = nil
        return refuse(message)
    }

    private func refuse(_ message: String) -> Bool {
        notice.stringValue = message
        notice.isHidden = false
        lastAccepted = false
        return false
    }

    private func nativeRange(_ range: NSRange) -> Bool {
        let text = textView.string as NSString
        guard range.location != NSNotFound, range.location >= 0, range.length >= 0,
              range.location <= text.length, range.length <= text.length - range.location else { return false }
        if range.length == 0 {
            return range.location == text.length || range.location == 0
                || text.rangeOfComposedCharacterSequence(at: range.location).location == range.location
        }
        // AppKit requires glyph-aligned selections: a byte-exact search can
        // legitimately target only a combining mark or part of a ligature.
        // Refuse those selections rather than silently widening copied text.
        guard text.rangeOfComposedCharacterSequences(for: range) == range,
              let layout = textView.layoutManager else { return false }
        var actual = NSRange(location: NSNotFound, length: 0)
        _ = layout.glyphRange(forCharacterRange: range, actualCharacterRange: &actual)
        return actual == range
    }
}

/// Physical CR/LF/CRLF starts in the admitted text, expressed in AppKit's UTF-16
/// coordinates. A single bounded scan belongs to capture installation, never
/// scrolling. UInt32 keeps even a newline-only 4 MiB capture's table at 16 MiB.
final class AtlasReaderLineIndex {
    static let maximumSourceBytes = 4 * 1024 * 1024
    let starts: [UInt32]
    let utf16Length: Int

    init?(text: String) {
        guard text.utf8.count <= Self.maximumSourceBytes else { return nil }
        var starts: [UInt32] = []
        starts.reserveCapacity(min(text.utf8.count, 4096))
        var units = text.utf16.makeIterator()
        var next = units.next()
        var offset = 0
        var beginsLine = true
        while let unit = next {
            if beginsLine {
                starts.append(UInt32(offset))
                beginsLine = false
            }
            offset += 1
            next = units.next()
            if unit == 13 {
                if next == 10 {
                    offset += 1
                    next = units.next()
                }
                beginsLine = true
            } else if unit == 10 {
                beginsLine = true
            }
        }
        // No virtual EOF row: an empty capture has no physical source lines,
        // and a final terminator belongs to the preceding physical line.
        self.starts = starts
        utf16Length = offset
    }

    func line(containing character: Int) -> Int? {
        guard character >= 0, character < utf16Length, !starts.isEmpty else { return nil }
        var low = 0
        var high = starts.count
        while low < high {
            let middle = low + (high - low) / 2
            if Int(starts[middle]) <= character { low = middle + 1 }
            else { high = middle }
        }
        return low - 1
    }

    func label(line: Int, firstPhysicalLine: UInt64?) -> String {
        guard starts.indices.contains(line) else { return "?" }
        guard let base = firstPhysicalLine, base > 0 else { return "+\(line + 1)" }
        let (number, overflow) = base.addingReportingOverflow(UInt64(line))
        return overflow ? "?" : String(number)
    }
}

/// A decorative, separately drawn gutter over exact source text. AppKit owns
/// line fragment layout; soft wraps and Unicode layout separators never invent
/// additional physical source lines. Unknown page bases use explicit + labels.
@MainActor final class AtlasReaderLineRuler: NSRulerView {
    struct Label {
        let physicalLine: Int
        let text: String
        let frame: NSRect
    }
    static let maximumVisibleLabels = 2048
    private weak var sourceTextView: NSTextView?
    private(set) var lineIndex: AtlasReaderLineIndex?
    var firstPhysicalLine: UInt64?
    var foregroundColor: NSColor = .textColor
    var backgroundColor: NSColor = .textBackgroundColor
    private let numberFont = NSFont.monospacedDigitSystemFont(ofSize: 11, weight: .regular)

    init(scrollView: NSScrollView, textView: NSTextView) {
        sourceTextView = textView
        super.init(scrollView: scrollView, orientation: .verticalRuler)
        ruleThickness = 44
        let clip = scrollView.contentView
        clip.postsBoundsChangedNotifications = true
        clip.postsFrameChangedNotifications = true
        textView.postsFrameChangedNotifications = true
        NotificationCenter.default.addObserver(self, selector: #selector(viewportChanged(_:)),
            name: NSView.boundsDidChangeNotification, object: clip)
        NotificationCenter.default.addObserver(self, selector: #selector(viewportChanged(_:)),
            name: NSView.frameDidChangeNotification, object: clip)
        NotificationCenter.default.addObserver(self, selector: #selector(viewportChanged(_:)),
            name: NSView.frameDidChangeNotification, object: textView)
        setAccessibilityElement(false)
    }

    required init(coder: NSCoder) { fatalError("AtlasReaderLineRuler requires its source text view") }
    deinit { NotificationCenter.default.removeObserver(self) }
    override var isFlipped: Bool { true }

    @objc private func viewportChanged(_ notification: Notification) { needsDisplay = true }

    func install(_ lines: AtlasReaderLineIndex?) {
        lineIndex = lines
        needsDisplay = true
    }

    func thickness(firstPhysicalLine: UInt64?) -> CGFloat {
        guard let lines = lineIndex, !lines.starts.isEmpty else { return 44 }
        let first = lines.label(line: 0, firstPhysicalLine: firstPhysicalLine)
        let last = lines.label(line: lines.starts.count - 1, firstPhysicalLine: firstPhysicalLine)
        // Include the first label: an overflowing last label is "?" and must
        // not shrink a valid UInt64.max first line into an unreadable gutter.
        let digits = min(21, max(first.count, last.count))
        let sample = String(repeating: "8", count: digits) as NSString
        return max(44, ceil(sample.size(withAttributes: [.font: numberFont]).width) + 16)
    }

    /// Shared by drawing and native geometry tests. Inspect only the visible
    /// glyph band and at most a bounded number of physical starts after a
    /// binary search; no entire-source line/glyph scan belongs to a redraw.
    func visibleLabels() -> [Label] {
        guard let textView = sourceTextView, let layout = textView.layoutManager,
              let container = textView.textContainer, let lines = lineIndex,
              textView.textStorage?.length == lines.utf16Length,
              !lines.starts.isEmpty else { return [] }
        let origin = textView.textContainerOrigin
        let visible = textView.visibleRect.offsetBy(dx: -origin.x, dy: -origin.y)
        guard visible.minY.isFinite, visible.maxX.isFinite, visible.height.isFinite,
              visible.height > 0 else { return [] }
        // Horizontal scrolling must not make short/blank lines disappear from
        // the gutter. Query the full row back to its physical text origin.
        let rowBand = NSRect(x: 0, y: visible.minY, width: max(1, visible.maxX), height: visible.height)
        let glyphs = layout.glyphRange(forBoundingRect: rowBand, in: container)
        guard glyphs.location != NSNotFound, glyphs.length > 0 else { return [] }
        let characters = layout.characterRange(forGlyphRange: glyphs, actualGlyphRange: nil)
        guard characters.location != NSNotFound,
              let first = lines.line(containing: characters.location) else { return [] }
        let end = min(lines.utf16Length, characters.location + characters.length)
        var labels: [Label] = []
        labels.reserveCapacity(min(Self.maximumVisibleLabels, lines.starts.count - first))
        var index = first
        // Bound examined starts, including starts suppressed by wrapping.
        while index < lines.starts.count, Int(lines.starts[index]) < end,
              index - first < Self.maximumVisibleLabels {
            let character = Int(lines.starts[index])
            let glyph = layout.glyphIndexForCharacter(at: character)
            guard layout.isValidGlyphIndex(glyph) else { break }
            let fragment = layout.lineFragmentRect(forGlyphAt: glyph, effectiveRange: nil)
            let point = convert(NSPoint(x: origin.x, y: origin.y + fragment.minY), from: textView)
            let frame = NSRect(x: 0, y: point.y, width: bounds.width, height: fragment.height)
            if frame.minY.isFinite, frame.height.isFinite, frame.height > 0, frame.intersects(bounds) {
                labels.append(Label(physicalLine: index,
                    text: lines.label(line: index, firstPhysicalLine: firstPhysicalLine), frame: frame))
            }
            index += 1
        }
        return labels
    }

    override func drawHashMarksAndLabels(in rect: NSRect) {
        backgroundColor.setFill()
        rect.fill()
        let attributes: [NSAttributedString.Key: Any] = [
            .font: numberFont, .foregroundColor: foregroundColor.withAlphaComponent(0.65)
        ]
        for label in visibleLabels() where label.frame.intersects(rect) {
            let value = label.text as NSString
            let size = value.size(withAttributes: attributes)
            value.draw(at: NSPoint(x: max(0, bounds.width - size.width - 8),
                y: label.frame.minY + max(0, (label.frame.height - size.height) / 2)),
                withAttributes: attributes)
        }
        foregroundColor.withAlphaComponent(0.16).setFill()
        NSRect(x: max(0, bounds.width - 1), y: rect.minY, width: 1, height: rect.height).fill()
    }
}
