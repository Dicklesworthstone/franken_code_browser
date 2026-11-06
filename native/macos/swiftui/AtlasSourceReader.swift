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
    var backgroundColor: NSColor = .textBackgroundColor
    var foregroundColor: NSColor = .textColor
    let styledSource: () -> NSAttributedString

    func makeCoordinator() -> AtlasSourceReaderController { AtlasSourceReaderController() }
    func makeNSView(context: Context) -> NSStackView { context.coordinator.view }
    func updateNSView(_ view: NSStackView, context: Context) {
        context.coordinator.update(source: source, navigation: navigation,
            selection: selection, wrapsLines: wrapsLines, backgroundColor: backgroundColor,
            foregroundColor: foregroundColor, styledSource: styledSource)
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
    private var installedSource: AtlasSource?
    private var lastNavigation: UUID?
    private var lastTargetSource: AtlasSource?
    private var lastRange: NSRange?
    private var lastAccepted = true
    private var wrapsLines = false
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
        notice.font = .systemFont(ofSize: 11)
        notice.textColor = .secondaryLabelColor
        notice.isHidden = true
    }

    @discardableResult
    func update(source: AtlasSource, navigation: UUID, selection: AtlasReaderSelection?,
                wrapsLines: Bool = false,
                backgroundColor: NSColor = .textBackgroundColor, foregroundColor: NSColor = .textColor,
                styledSource: () -> NSAttributedString) -> Bool {
        precondition(Thread.isMainThread)
        if textView.backgroundColor != backgroundColor { textView.backgroundColor = backgroundColor }
        if scrollView.backgroundColor != backgroundColor { scrollView.backgroundColor = backgroundColor }
        textView.insertionPointColor = foregroundColor
        let changed = installedSource !== source
        if changed {
            // Same bound as the complete-source native opening route. Never
            // publish a prefix under a whole-source label, or retain another
            // capture under the replacement source's surrounding UI.
            guard source.text.utf8.count <= 4 * 1024 * 1024 else {
                return refuseSource("Source exceeds this native reader's 4 MiB limit.")
            }
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
            textView.setAccessibilityLabel("Read-only source: \(source.path)")
            textView.setSelectedRange(NSRange(location: 0, length: 0))
        }
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
        if let anchor { restoreViewport(anchor) }
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
        guard glyph < layout.numberOfGlyphs else { return nil }
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
