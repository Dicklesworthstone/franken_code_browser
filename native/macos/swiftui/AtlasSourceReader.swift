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
    let styledSource: () -> NSAttributedString

    func makeCoordinator() -> AtlasSourceReaderController { AtlasSourceReaderController() }
    func makeNSView(context: Context) -> NSStackView { context.coordinator.view }
    func updateNSView(_ view: NSStackView, context: Context) {
        context.coordinator.update(source: source, navigation: navigation,
                                   selection: selection, styledSource: styledSource)
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
        scrollView.hasHorizontalScroller = false
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
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]
        textView.minSize = .zero
        textView.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude,
                                  height: CGFloat.greatestFiniteMagnitude)
        textView.textContainerInset = NSSize(width: 8, height: 8)
        textView.textContainer?.containerSize = NSSize(width: 624, height: CGFloat.greatestFiniteMagnitude)
        textView.textContainer?.widthTracksTextView = true
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
                styledSource: () -> NSAttributedString) -> Bool {
        precondition(Thread.isMainThread)
        let changed = installedSource !== source
        if changed {
            // Same bound as the complete-source native opening route. Never
            // publish a prefix under a whole-source label.
            guard source.text.utf8.count <= 4 * 1024 * 1024 else {
                return refuse("Source exceeds this native reader's 4 MiB limit.")
            }
            let styled = styledSource()
            let exact = styled.string.utf8.elementsEqual(source.text.utf8)
            let content = exact ? styled : NSAttributedString(string: source.text, attributes: [
                .font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular),
                .foregroundColor: NSColor.textColor
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
